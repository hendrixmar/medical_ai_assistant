#[cfg(test)]
pub(crate) mod tests;

use axum::Router;

use crate::common::{api_doc, config::telemetry, config::Config, middlewares};
use crate::database::Db;
use crate::modules;

#[derive(Clone)]
pub struct AppState {
  pub db: Db,
  pub cfg: Config,
  pub clock: std::sync::Arc<dyn modules::doctor::Clock>,
  pub waha: Option<modules::whatsapp::client::Client>,
  pub google: Option<modules::google_calendar::client::Client>,
}

pub fn router(cfg: Config, db: Db) -> Router {
  router_with_clock(cfg, db, std::sync::Arc::new(modules::doctor::SystemClock))
}

pub fn router_with_clock(
  cfg: Config,
  db: Db,
  clock: std::sync::Arc<dyn modules::doctor::Clock>,
) -> Router {
  compose(cfg, db, clock).0
}

/// Router and process workers share one HTTP pool. Construction starts no tasks.
pub fn compose(
  cfg: Config,
  db: Db,
  clock: std::sync::Arc<dyn modules::doctor::Clock>,
) -> (
  Router,
  Option<modules::whatsapp::client::Client>,
  Option<modules::google_calendar::client::Client>,
) {
  let waha = cfg.waha.as_ref().map(|config| {
    modules::whatsapp::client::Client::new(config).unwrap_or_else(|message| panic!("{message}"))
  });
  let google = cfg.google.as_ref().map(|config| {
    modules::google_calendar::client::Client::new(config)
      .unwrap_or_else(|message| panic!("{message}"))
  });
  let app_state = AppState {
    db,
    cfg,
    clock,
    waha: waha.clone(),
    google: google.clone(),
  };

  // Middleware that adds high level tracing to a Service.
  // Trace comes with good defaults but also supports customizing many aspects of the output:
  // https://docs.rs/tower-http/latest/tower_http/trace/index.html
  let trace_layer = telemetry::trace_layer();

  // Sets 'x-request-id' header with randomly generated uuid v7.
  let request_id_layer = middlewares::request_id_layer();

  // Propagates 'x-request-id' header from the request to the response.
  let propagate_request_id_layer = middlewares::propagate_request_id_layer();

  // Layer that applies the Cors middleware which adds headers for CORS.
  let cors_layer = middlewares::cors_layer();

  // Layer that applies the Timeout middleware, which sets a timeout for requests.
  // The default value is 15 seconds.
  let timeout_layer = middlewares::timeout_layer();

  // Any trailing slashes from request paths will be removed. For example, a request with `/foo/`
  // will be changed to `/foo` before reaching the internal service.
  let normalize_path_layer = middlewares::normalize_path_layer();

  // Create the router with the routes.
  let router = modules::router();
  #[cfg(test)]
  let router = router.route("/__test/marker", axum::routing::get(tests::marker));

  // Create the API documentation using OpenAPI and Swagger UI.
  let api_doc = match app_state.cfg.env {
    crate::common::config::Environment::Development => {
      Router::new().merge(api_doc::swagger_ui(&app_state.cfg))
    }
    crate::common::config::Environment::Production => Router::new(),
  };

  // Combine all the routes and apply the middleware layers.
  // The order of the layers is important. The first layer is the outermost layer.
  let router = Router::new()
    .merge(router)
    .merge(modules::whatsapp::webhook_router())
    .merge(modules::doctor::router(app_state.clone()))
    .merge(api_doc)
    .layer(normalize_path_layer)
    .layer(cors_layer)
    .layer(timeout_layer)
    .layer(propagate_request_id_layer)
    .layer(trace_layer)
    .layer(request_id_layer)
    .layer(axum::middleware::from_fn(callback_envelope))
    .with_state(app_state);
  (router, waha, google)
}

// Outside authentication, route rejections, security headers AND the global timeout.
async fn callback_envelope(
  request: axum::http::Request<axum::body::Body>,
  next: axum::middleware::Next,
) -> axum::response::Response {
  use axum::response::IntoResponse;
  let callback = request.uri().path().trim_end_matches('/') == "/settings/google/callback";
  let response = next.run(request).await;
  if !callback {
    return response;
  }
  (
    axum::http::StatusCode::SEE_OTHER,
    [
      ("location", "/settings/google"),
      ("cache-control", "no-store"),
      ("referrer-policy", "no-referrer"),
      (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
      ),
      ("x-content-type-options", "nosniff"),
    ],
  )
    .into_response()
}
