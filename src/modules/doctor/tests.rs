mod configuration;
mod enrollment;
mod login;
mod maintenance;
mod session;
mod storage_failures;

use crate::{
  app::{router, tests::with_database},
  common::config::{Config, Configuration, Environment},
};
use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

fn configuration() -> Config {
  Arc::new(Configuration {
    env: Environment::Production,
    listen_address: "127.0.0.1:0".parse().unwrap(),
    app_port: 0,
    swagger_endpoint: "/docs".into(),
    db_dsn: std::env::var("TEST_DATABASE_URL").expect("explicit test database required"),
    db_pool_max_size: 2,
    db_timeout: 5,
    db_run_migrations: true,
    bcrypt_cost: 4,
    public_origin: None,
    doctor_totp_key: None,
    waha: None,
    google: None,
    claude_api_key: None,
    claude_model: None,
    claude_test_endpoint: None,
    waha_takeover_enabled: false,
    waha_outbound_enabled: false,
    waha_contact_replies_per_hour: 20,
  })
}

fn enabled_configuration() -> Config {
  let mut cfg = configuration();
  let config = Arc::get_mut(&mut cfg).unwrap();
  config.public_origin = Some("https://practice.test".into());
  config.doctor_totp_key = Some([42; 32]);
  cfg
}

pub(crate) struct TestClock(std::sync::atomic::AtomicI64);
impl super::Clock for TestClock {
  fn now(&self) -> i64 {
    self.0.load(std::sync::atomic::Ordering::SeqCst)
  }
}
impl TestClock {
  pub(crate) fn advance(&self, seconds: i64) {
    self
      .0
      .fetch_add(seconds, std::sync::atomic::Ordering::SeqCst);
  }
}

pub(crate) const NOW: i64 = 1_800_000_000;
const PASSWORD: &str = "A private long test password";
const SECRET: [u8; 20] = [17; 20];

pub(crate) async fn text(response: axum::response::Response) -> String {
  String::from_utf8(
    response
      .into_body()
      .collect()
      .await
      .unwrap()
      .to_bytes()
      .to_vec(),
  )
  .unwrap()
}
pub(crate) fn field(html: &str, name: &str) -> String {
  html
    .split(&format!("name=\"{name}\" value=\""))
    .nth(1)
    .unwrap()
    .split('"')
    .next()
    .unwrap()
    .to_owned()
}
fn cookie(response: &axum::response::Response, name: &str) -> String {
  response
    .headers()
    .get_all("set-cookie")
    .iter()
    .filter_map(|v| v.to_str().ok())
    .find(|v| v.starts_with(&format!("{name}=")))
    .unwrap()
    .split(';')
    .next()
    .unwrap()
    .to_owned()
}
pub(crate) async fn challenge(app: &Router) -> (String, String) {
  let response = get(app, "/login").await;
  assert_eq!(response.status(), 200);
  let cookie = cookie(&response, "__Host-doctor_preauth");
  let csrf = field(&text(response).await, "csrf_token");
  (cookie, csrf)
}
pub(crate) async fn login(
  app: &Router,
  cookie: &str,
  csrf: &str,
  password: &str,
  code: &str,
  return_to: &str,
) -> axum::response::Response {
  let body = reqwest::Url::parse_with_params(
    "https://practice.test",
    [
      ("csrf_token", csrf),
      ("password", password),
      ("code", code),
      ("return_to", return_to),
    ],
  )
  .unwrap()
  .query()
  .unwrap()
  .to_owned();
  app
    .clone()
    .oneshot(
      Request::builder()
        .method("POST")
        .uri("/login")
        .header("origin", "https://practice.test")
        .header("cookie", cookie)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn both_factors_open_protected_dashboard_and_settings() {
  with_database(|db| async move {
    db.run_migrations().await.unwrap();
    let cfg = enabled_configuration();
    let clock = Arc::new(TestClock(std::sync::atomic::AtomicI64::new(NOW)));
    let totp = super::crypto::totp(SECRET).unwrap();
    super::enrollment::enroll(
      &db,
      &cfg,
      PASSWORD.to_owned(),
      SECRET,
      &totp.generate(NOW as u64).to_string(),
      NOW,
    )
    .await
    .unwrap();
    clock.advance(30);
    let app = crate::app::router_with_clock(cfg, db, clock.clone());
    let (preauth, csrf) = challenge(&app).await;
    let response = login(
      &app,
      &preauth,
      &csrf,
      PASSWORD,
      &totp.generate((NOW + 30) as u64).to_string(),
      "/settings",
    )
    .await;
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/settings");
    let session = cookie(&response, "__Host-doctor_session");
    for path in ["/dashboard", "/settings"] {
      let response = app
        .clone()
        .oneshot(
          Request::builder()
            .uri(path)
            .header("cookie", &session)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
      assert_eq!(response.status(), 200);
      assert!(!field(&text(response).await, "csrf_token").is_empty());
    }
  })
  .await;
}

pub(crate) async fn fixture(
  db: &crate::database::Db,
) -> (Config, Arc<TestClock>, Router, totp_rs::Totp) {
  db.run_migrations().await.unwrap();
  let cfg = enabled_configuration();
  let clock = Arc::new(TestClock(std::sync::atomic::AtomicI64::new(NOW)));
  let totp = super::crypto::totp(SECRET).unwrap();
  super::enrollment::enroll(
    db,
    &cfg,
    PASSWORD.to_owned(),
    SECRET,
    &totp.generate(NOW as u64).to_string(),
    NOW,
  )
  .await
  .unwrap();
  clock.advance(30);
  let app = crate::app::router_with_clock(cfg.clone(), db.clone(), clock.clone());
  (cfg, clock, app, totp)
}

pub(crate) async fn post(
  app: &Router,
  path: &str,
  cookie: Option<&str>,
  origin: Option<&str>,
  fields: &[(&str, &str)],
) -> axum::response::Response {
  let url =
    reqwest::Url::parse_with_params("https://practice.test", fields.iter().copied()).unwrap();
  let body = url.query().unwrap_or_default().to_owned();
  let mut request = Request::builder()
    .method("POST")
    .uri(path)
    .header("content-type", "application/x-www-form-urlencoded");
  if let Some(cookie) = cookie {
    request = request.header("cookie", cookie);
  }
  if let Some(origin) = origin {
    request = request.header("origin", origin);
  }
  app
    .clone()
    .oneshot(request.body(Body::from(body)).unwrap())
    .await
    .unwrap()
}
pub(crate) async fn authenticated_get(
  app: &Router,
  path: &str,
  session: &str,
) -> axum::response::Response {
  app
    .clone()
    .oneshot(
      Request::builder()
        .uri(path)
        .header("cookie", session)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap()
}
pub(crate) async fn signed_in(app: &Router, totp: &totp_rs::Totp, now: i64) -> String {
  let (preauth, csrf) = challenge(app).await;
  let response = login(
    app,
    &preauth,
    &csrf,
    PASSWORD,
    &totp.generate(now as u64).to_string(),
    "/dashboard",
  )
  .await;
  assert_eq!(response.status(), 303);
  cookie(&response, "__Host-doctor_session")
}
async fn count(db: &crate::database::Db, table: &str) -> i64 {
  use sea_orm::ConnectionTrait;
  db.conn
    .query_one(super::store::sql(
      &format!("SELECT count(*) AS count FROM {table}"),
      vec![],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "count")
    .unwrap()
}

pub(crate) async fn get(app: &Router, path: &str) -> axum::response::Response {
  app
    .clone()
    .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn missing_auth_configuration_fails_closed_without_breaking_health() {
  with_database(|db| async move {
    db.run_migrations().await.unwrap();
    let app = router(configuration(), db);
    let response = get(&app, "/login").await;
    assert_eq!(response.status(), 503);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&body)
      .unwrap()
      .contains("El acceso todavía no está listo"));
    assert_eq!(get(&app, "/dashboard").await.status(), 503);
    assert_eq!(get(&app, "/api/v1/health").await.status(), 200);
  })
  .await;
}
