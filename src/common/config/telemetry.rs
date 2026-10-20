use axum::{body::Body, http::Request};
use tower_http::{
  classify::{ServerErrorsAsFailures, SharedClassifier},
  trace::{DefaultOnRequest, DefaultOnResponse, TraceLayer},
};
use tracing::{Level, Span};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Credentials can appear in dependency TRACE/DEBUG events. Operator RUST_LOG
/// never widens this allowlist, including explicit reqwest/hyper/sqlx directives.
pub fn safe_filter() -> EnvFilter {
  EnvFilter::new("off,server=info")
}
pub fn setup_tracing() {
  tracing_subscriber::registry()
    .with(safe_filter())
    .with(fmt::layer().json())
    .init();
}

fn request_span(request: &Request<Body>) -> Span {
  let route = match request.uri().path().trim_end_matches('/') {
    "/api/v1/health" => "health",
    "/login" => "login",
    "/logout" => "logout",
    "/dashboard" => "dashboard",
    "/settings" => "settings",
    "/settings/google" => "google_settings",
    "/settings/google/connect" => "google_connect",
    "/settings/google/callback" => "google_callback",
    "/settings/google/calendars" => "google_selection",
    "/settings/google/probe" => "google_probe",
    _ => "other",
  };
  let method = match request.method().as_str() {
    "GET" => "GET",
    "POST" => "POST",
    "HEAD" => "HEAD",
    "PUT" => "PUT",
    "DELETE" => "DELETE",
    "OPTIONS" => "OPTIONS",
    "PATCH" => "PATCH",
    _ => "other",
  };
  // Do not trust the propagated caller request ID for log correlation.
  tracing::info_span!("request", route, method, correlation = %uuid::Uuid::now_v7())
}

type SafeTrace = TraceLayer<SharedClassifier<ServerErrorsAsFailures>, fn(&Request<Body>) -> Span>;
pub fn trace_layer() -> SafeTrace {
  TraceLayer::new_for_http()
    .make_span_with(request_span as fn(&Request<Body>) -> Span)
    .on_request(DefaultOnRequest::new().level(Level::INFO))
    .on_response(DefaultOnResponse::new().level(Level::INFO))
}
