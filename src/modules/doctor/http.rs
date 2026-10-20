use super::{
  crypto,
  store::{self, Session},
  views::{self, LoginError, LoginView},
  Error,
};
use crate::app::AppState;
use axum::{
  body::{to_bytes, Body},
  extract::{FromRequest, Query, State},
  http::{HeaderMap, Method, Request, StatusCode},
  middleware::{self, Next},
  response::{Html, IntoResponse, Redirect, Response},
  routing::{get, post},
  Extension, Form, Router,
};
use serde::Deserialize;

const PREAUTH: &str = "__Host-doctor_preauth";
const SESSION: &str = "__Host-doctor_session";

pub fn router(state: AppState) -> Router<AppState> {
  let protected = Router::new()
    .merge(crate::modules::conversation::http::protected_router())
    .merge(crate::modules::appointments::http::protected_router())
    .merge(crate::modules::payments::http::protected_router())
    .merge(crate::modules::practice::router())
    .merge(crate::modules::whatsapp::dashboard_router())
    .merge(crate::modules::doctor_number::router())
    .merge(crate::modules::google_calendar::router())
    .route("/logout", post(logout))
    .route_layer(middleware::from_fn_with_state(state, authorize));
  Router::new()
    .route("/login", get(login_get).post(login_post))
    .route(
      "/aviso-de-privacidad",
      get(|| async { Html(crate::modules::conversation::views::privacy()) }),
    )
    .route(
      "/assets/doctor.css",
      get(|| async { ([("content-type", "text/css; charset=utf-8")], views::CSS) }),
    )
    .merge(protected)
    .layer(axum::extract::DefaultBodyLimit::max(8192))
    .layer(middleware::from_fn(security_headers))
}

fn enabled(state: &AppState) -> bool {
  state.cfg.public_origin.is_some() && state.cfg.doctor_totp_key.is_some()
}
fn return_to(value: &str) -> &str {
  match value {
    "/settings" => "/settings",
    "/settings/whatsapp" => "/settings/whatsapp",
    "/settings/doctor-number" => "/settings/doctor-number",
    "/settings/google" => "/settings/google",
    "/appointments" => "/appointments",
    "/contacts" => "/contacts",
    path
      if [
        "/appointments/",
        "/contacts/",
        "/dashboard/payments/receipts/",
      ]
      .iter()
      .any(|prefix| {
        path
          .strip_prefix(prefix)
          .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
      }) =>
    {
      path
    }
    _ => "/dashboard",
  }
}

fn login_page(status: StatusCode, csrf: &str, target: &str, error: Option<LoginError>) -> Response {
  (
    status,
    Html(views::login(LoginView {
      csrf_token: csrf,
      return_to: return_to(target),
      error,
    })),
  )
    .into_response()
}
fn failure(error: Error, csrf: &str, target: &str) -> Response {
  let (status, view) = match error {
    Error::Invalid => (StatusCode::UNAUTHORIZED, LoginError::InvalidCredentials),
    Error::Limited(_) => (StatusCode::TOO_MANY_REQUESTS, LoginError::RateLimited),
    _ => (StatusCode::SERVICE_UNAVAILABLE, LoginError::Unavailable),
  };
  let mut response = login_page(status, csrf, target, Some(view));
  if let Error::Limited(seconds) = error {
    response
      .headers_mut()
      .insert("retry-after", seconds.to_string().parse().unwrap());
  }
  response
}
fn csrf_failure() -> Response {
  login_page(
    StatusCode::FORBIDDEN,
    "",
    "/dashboard",
    Some(LoginError::ExpiredForm),
  )
}
fn set_cookie(response: &mut Response, name: &str, token: &str, seconds: i64) {
  response.headers_mut().append(
    "set-cookie",
    format!("{name}={token}; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age={seconds}")
      .parse()
      .unwrap(),
  );
}
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
  let mut matches = headers
    .get_all("cookie")
    .iter()
    .filter_map(|header| header.to_str().ok())
    .flat_map(|header| header.split(';'))
    .filter_map(|item| item.trim().split_once('='))
    .filter(|(key, _)| *key == name)
    .map(|(_, value)| value);
  let value = matches.next()?;
  if matches.next().is_some() || value.len() != 43 {
    return None;
  }
  Some(value.to_owned())
}
fn valid_origin(state: &AppState, headers: &HeaderMap) -> bool {
  headers.get_all("origin").iter().count() == 1
    && headers.get("origin").and_then(|value| value.to_str().ok())
      == state.cfg.public_origin.as_deref()
}

#[derive(Default, Deserialize)]
struct Navigation {
  #[serde(default)]
  return_to: String,
}
async fn login_get(State(state): State<AppState>, Query(nav): Query<Navigation>) -> Response {
  if !enabled(&state) {
    return failure(Error::Unavailable, "", &nav.return_to);
  }
  match store::doctor(&state.db).await {
    Ok(Some(_)) => {}
    _ => return failure(Error::Unavailable, "", &nav.return_to),
  }
  match store::challenge(&state.db, state.clock.now()).await {
    Ok((token, csrf)) => {
      let mut response = login_page(StatusCode::OK, &csrf, &nav.return_to, None);
      set_cookie(&mut response, PREAUTH, &token, store::CHALLENGE_SECONDS);
      response
    }
    Err(error) => failure(error, "", &nav.return_to),
  }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LoginForm {
  csrf_token: String,
  password: String,
  code: String,
  return_to: String,
}
async fn login_post(
  State(state): State<AppState>,
  headers: HeaderMap,
  form: Result<Form<LoginForm>, axum::extract::rejection::FormRejection>,
) -> Response {
  if !enabled(&state) {
    return failure(Error::Unavailable, "", "/dashboard");
  }
  if !valid_origin(&state, &headers) {
    return csrf_failure();
  }
  let Ok(Form(form)) = form else {
    return csrf_failure();
  };
  let Some(preauth) = cookie(&headers, PREAUTH) else {
    return csrf_failure();
  };
  match store::valid_challenge(&state.db, &preauth, &form.csrf_token, state.clock.now()).await {
    Ok(true) => {}
    Ok(false) => return csrf_failure(),
    Err(error) => return failure(error, "", &form.return_to),
  }
  let result = authenticate(&state, &preauth, &form).await;
  match result {
    Ok(token) => {
      let mut response = Redirect::to(return_to(&form.return_to)).into_response();
      set_cookie(&mut response, SESSION, &token, store::SESSION_SECONDS);
      set_cookie(&mut response, PREAUTH, "", 0);
      response
    }
    Err(error) => failure(error, &form.csrf_token, &form.return_to),
  }
}
async fn authenticate(state: &AppState, preauth: &str, form: &LoginForm) -> Result<String, Error> {
  store::reserve_attempt(&state.db, state.clock.now()).await?;
  let doctor = store::doctor(&state.db).await?.ok_or(Error::Unavailable)?;
  if !crypto::password_matches(form.password.clone(), doctor.try_get("", "password_hash")?).await? {
    return Err(Error::Invalid);
  }
  let key = state
    .cfg
    .doctor_totp_key
    .as_ref()
    .ok_or(Error::Unavailable)?;
  let secret = crypto::decrypt(
    key,
    &doctor.try_get::<Vec<u8>>("", "totp_ciphertext")?,
    &doctor.try_get::<Vec<u8>>("", "totp_nonce")?,
  )?;
  let now = state.clock.now();
  let step = crypto::accepted_step(secret, &form.code, now)?;
  store::create_session(&state.db, step, preauth, now).await
}

#[derive(Deserialize)]
struct Csrf {
  csrf_token: String,
}
async fn authorize(
  State(state): State<AppState>,
  mut request: Request<Body>,
  next: Next,
) -> Response {
  if !enabled(&state) {
    return failure(Error::Unavailable, "", "/dashboard");
  }
  let safe = matches!(
    *request.method(),
    Method::GET | Method::HEAD | Method::OPTIONS
  );
  let session = match cookie(request.headers(), SESSION) {
    Some(token) => match store::session(&state.db, &token, state.clock.now()).await {
      Ok(session) => session,
      Err(error) => return failure(error, "", "/dashboard"),
    },
    None => None,
  };
  let Some(session) = session else {
    return if safe {
      Redirect::to(&format!(
        "/login?return_to={}",
        return_to(request.uri().path())
      ))
      .into_response()
    } else {
      StatusCode::UNAUTHORIZED.into_response()
    };
  };
  if !safe {
    if !valid_origin(&state, request.headers()) {
      return csrf_failure();
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = to_bytes(body, 64 * 1024).await else {
      return csrf_failure();
    };
    let mut copy = Request::new(Body::from(bytes.clone()));
    *copy.method_mut() = parts.method.clone();
    *copy.headers_mut() = parts.headers.clone();
    let Ok(Form(csrf)) = Form::<Csrf>::from_request(copy, &state).await else {
      return csrf_failure();
    };
    if !crypto::equal(&session.csrf, &csrf.csrf_token) {
      return csrf_failure();
    }
    request = Request::from_parts(parts, Body::from(bytes));
  }
  request.extensions_mut().insert(session);
  next.run(request).await
}
async fn logout(State(state): State<AppState>, Extension(session): Extension<Session>) -> Response {
  match store::logout(&state.db, &session).await {
    Ok(()) => {
      let mut response = Redirect::to("/login").into_response();
      set_cookie(&mut response, SESSION, "", 0);
      response
    }
    Err(error) => failure(error, "", "/dashboard"),
  }
}

async fn security_headers(request: Request<Body>, next: Next) -> Response {
  // Browsers enforce the initiating document's form-action on OAuth redirects too.
  let form_action = if request.method() == Method::GET && request.uri().path() == "/settings/google"
  {
    "'self' https://accounts.google.com"
  } else {
    "'self'"
  };
  let policy = format!("default-src 'none'; img-src 'self'; style-src 'self'; script-src 'none'; form-action {form_action}; base-uri 'none'; frame-ancestors 'none'");
  let mut response = next.run(request).await;
  for (name, value) in [
    ("cache-control", "no-store"),
    ("content-security-policy", policy.as_str()),
    ("x-frame-options", "DENY"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "same-origin"),
  ] {
    response.headers_mut().insert(name, value.parse().unwrap());
  }
  response
}
