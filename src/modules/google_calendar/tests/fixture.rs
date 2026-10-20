use super::*;
use crate::{
  app::tests::reconnect,
  common::config::Config,
  database::Db,
  modules::doctor::{tests::TestClock, Clock},
};
use axum::{
  body::{to_bytes, Body},
  http::{Request, StatusCode},
  response::IntoResponse,
};
use serde_json::{json, Value};
use tokio::sync::{Mutex, Semaphore};

struct SlowBody {
  first: bool,
  done: bool,
  timer: std::pin::Pin<Box<tokio::time::Sleep>>,
}
impl hyper::body::Body for SlowBody {
  type Data = axum::body::Bytes;
  type Error = std::convert::Infallible;
  fn poll_frame(
    self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
    let this = self.get_mut();
    if this.first {
      this.first = false;
      return std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(
        axum::body::Bytes::from_static(b"{"),
      ))));
    }
    if this.done {
      return std::task::Poll::Ready(None);
    }
    if std::future::Future::poll(this.timer.as_mut(), cx).is_pending() {
      return std::task::Poll::Pending;
    }
    this.done = true;
    std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(
      axum::body::Bytes::from_static(b"\"items\":[]}"),
    ))))
  }
}

pub(crate) struct Provider {
  pub calls: Mutex<Vec<(String, String, Value)>>,
  pub grant: Mutex<Value>,
  pub token_status: Mutex<u16>,
  pub list_status: Mutex<u16>,
  pub list_raw: Mutex<Option<Vec<u8>>>,
  pub slow_list_body: std::sync::atomic::AtomicBool,
  pub list_delay_ms: std::sync::atomic::AtomicU64,
  pub event_retry_after: Mutex<Option<String>>,
  pub metadata_status: Mutex<u16>,
  pub metadata_calls: std::sync::atomic::AtomicU64,
  pub hold_insert: std::sync::atomic::AtomicBool,
  pub insert_entered: Semaphore,
  pub insert_release: Semaphore,
  pub pages: Mutex<Vec<Value>>,
  pub event_pages: Mutex<Vec<Value>>,
  pub freebusy_error: Mutex<bool>,
  pub event: Mutex<Option<Value>>,
  pub insert_status: Mutex<u16>,
  pub patch_status: Mutex<u16>,
  pub delete_status: Mutex<u16>,
  pub hold_delete: std::sync::atomic::AtomicBool,
  pub delete_entered: Semaphore,
  pub delete_release: Semaphore,
  pub event_status: Mutex<u16>,
  pub conference: Mutex<String>,
  pub video: Mutex<String>,
  pub exchange_entered: Semaphore,
  pub exchange_release: Semaphore,
  pub hold_exchange: std::sync::atomic::AtomicBool,
}
impl Default for Provider {
  fn default() -> Self {
    Self {
      calls: Mutex::new(vec![]),
      token_status: Mutex::new(200),
      list_status: Mutex::new(200),
      list_raw: Mutex::new(None),
      slow_list_body: std::sync::atomic::AtomicBool::new(false),
      list_delay_ms: std::sync::atomic::AtomicU64::new(0),
      event_retry_after: Mutex::new(None),
      metadata_status: Mutex::new(200),
      metadata_calls: std::sync::atomic::AtomicU64::new(0),
      hold_insert: std::sync::atomic::AtomicBool::new(false),
      insert_entered: Semaphore::new(0),
      insert_release: Semaphore::new(0),
      grant: Mutex::new(
        json!({"access_token":ACCESS,"refresh_token":REFRESH,"expires_in":3600,"token_type":"Bearer","scope":SCOPES}),
      ),
      pages: Mutex::new(vec![
        json!({"items":[{"id":"secondary@example.invalid","summary":"Consultorio <seguro>","accessRole":"owner","hidden":true},{"id":"writer@example.invalid","summary":"Solo escritura","accessRole":"writer"}]}),
      ]),
      event_pages: Mutex::new(vec![]),
      freebusy_error: Mutex::new(false),
      event: Mutex::new(None),
      insert_status: Mutex::new(200),
      patch_status: Mutex::new(200),
      delete_status: Mutex::new(204),
      hold_delete: std::sync::atomic::AtomicBool::new(false),
      delete_entered: Semaphore::new(0),
      delete_release: Semaphore::new(0),
      event_status: Mutex::new(200),
      conference: Mutex::new("success".into()),
      video: Mutex::new("https://meet.google.com/abcd-ef-ghijk".into()),
      exchange_entered: Semaphore::new(0),
      exchange_release: Semaphore::new(0),
      hold_exchange: std::sync::atomic::AtomicBool::new(false),
    }
  }
}
async fn handle(
  axum::extract::State(state): axum::extract::State<Arc<Provider>>,
  request: Request<Body>,
) -> axum::response::Response {
  let (parts, body) = request.into_parts();
  let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
  let body = serde_json::from_slice::<Value>(&bytes)
    .unwrap_or_else(|_| Value::String(String::from_utf8(bytes.to_vec()).unwrap()));
  let path = parts.uri.path();
  state.calls.lock().await.push((
    parts.method.to_string(),
    parts.uri.to_string(),
    body.clone(),
  ));
  if path == "/token" {
    if state
      .hold_exchange
      .load(std::sync::atomic::Ordering::SeqCst)
    {
      state.exchange_entered.add_permits(1);
      let permit = state.exchange_release.acquire().await.unwrap();
      permit.forget();
    }
    return (
      StatusCode::from_u16(*state.token_status.lock().await).unwrap(),
      axum::Json(state.grant.lock().await.clone()),
    )
      .into_response();
  }
  if path == "/users/me/calendarList" {
    let delay = state
      .list_delay_ms
      .load(std::sync::atomic::Ordering::SeqCst);
    if delay > 0 {
      tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
    if state
      .slow_list_body
      .load(std::sync::atomic::Ordering::SeqCst)
    {
      return axum::response::Response::new(Body::new(SlowBody {
        first: true,
        done: false,
        timer: Box::pin(tokio::time::sleep(std::time::Duration::from_secs(6))),
      }));
    }
    let status = *state.list_status.lock().await;
    if status != 200 {
      return (
        StatusCode::from_u16(status).unwrap(),
        [("location", "http://127.0.0.1:9/MUST_NOT_FOLLOW")],
      )
        .into_response();
    }
    if let Some(bytes) = state.list_raw.lock().await.clone() {
      return bytes.into_response();
    }
    let url = reqwest::Url::parse(&format!("https://fixture.invalid{}", parts.uri)).unwrap();
    let page = url
      .query_pairs()
      .find(|(k, _)| k == "pageToken")
      .and_then(|(_, v)| v.parse::<usize>().ok())
      .unwrap_or(0);
    return axum::Json(
      state
        .pages
        .lock()
        .await
        .get(page)
        .cloned()
        .unwrap_or(json!({"items":[]})),
    )
    .into_response();
  }
  if path.starts_with("/users/me/calendarList/") {
    state
      .metadata_calls
      .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let status = *state.metadata_status.lock().await;
    if status != 200 {
      return StatusCode::from_u16(status).unwrap().into_response();
    }
    return axum::Json(
      json!({"id":"secondary@example.invalid","summary":"Consultorio","accessRole":"owner"}),
    )
    .into_response();
  }
  if path == "/freeBusy" {
    let mut calendars = serde_json::Map::new();
    for entry in body["items"].as_array().unwrap() {
      calendars.insert(
        entry["id"].as_str().unwrap().into(),
        if *state.freebusy_error.lock().await {
          json!({"errors":[{"reason":"notFound"}]})
        } else {
          json!({"busy":[]})
        },
      );
    }
    return axum::Json(json!({"calendars":calendars})).into_response();
  }
  if path.ends_with("/events") && parts.method == axum::http::Method::GET {
    let url = reqwest::Url::parse(&format!("https://fixture.invalid{}", parts.uri)).unwrap();
    let page = url
      .query_pairs()
      .find(|(key, _)| key == "pageToken")
      .and_then(|(_, value)| value.parse::<usize>().ok())
      .unwrap_or(0);
    let pages = state.event_pages.lock().await.clone();
    if let Some(page) = pages.get(page) {
      return axum::Json(page.clone()).into_response();
    }
    let items = state
      .event
      .lock()
      .await
      .clone()
      .into_iter()
      .collect::<Vec<_>>();
    return axum::Json(json!({"items":items})).into_response();
  }
  if path.contains("/events/") {
    match parts.method.as_str() {
      "GET" => {
        let status = *state.event_status.lock().await;
        if status != 200 {
          let mut response = if status == 410 {
            (
              StatusCode::GONE,
              axum::Json(json!({"error":{"errors":[{"reason":"deleted"}]}})),
            )
              .into_response()
          } else {
            StatusCode::from_u16(status).unwrap().into_response()
          };
          if let Some(value) = state.event_retry_after.lock().await.clone() {
            response
              .headers_mut()
              .insert("retry-after", value.parse().unwrap());
          }
          return response;
        }
        let Some(mut event) = state.event.lock().await.clone() else {
          return StatusCode::NOT_FOUND.into_response();
        };
        if event.pointer("/conferenceData/createRequest").is_some() {
          event["conferenceData"]["createRequest"]["status"] =
            json!({"statusCode":state.conference.lock().await.clone()});
          event["conferenceData"]["conferenceSolution"] = json!({"key":{"type":"hangoutsMeet"}});
          event["conferenceData"]["entryPoints"] =
            json!([{"entryPointType":"video","uri":state.video.lock().await.clone()}]);
        }
        *state.event.lock().await = Some(event.clone());
        return axum::Json(event).into_response();
      }
      "PATCH" => {
        assert_eq!(parts.headers.get("if-match").unwrap(), "\"fixture-etag\"");
        let status = *state.patch_status.lock().await;
        if !(200..300).contains(&status) {
          return StatusCode::from_u16(status).unwrap().into_response();
        }
        let Some(mut event) = state.event.lock().await.clone() else {
          return StatusCode::NOT_FOUND.into_response();
        };
        let Some(fields) = body.as_object() else {
          return StatusCode::BAD_REQUEST.into_response();
        };
        let Some(event_fields) = event.as_object_mut() else {
          return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        for (key, value) in fields {
          event_fields.insert(key.clone(), value.clone());
        }
        event["status"] = json!("confirmed");
        event["etag"] = json!("\"fixture-etag\"");
        if event.pointer("/conferenceData/createRequest").is_some() {
          event["conferenceData"]["createRequest"]["status"] =
            json!({"statusCode":state.conference.lock().await.clone()});
          event["conferenceData"]["conferenceSolution"] = json!({"key":{"type":"hangoutsMeet"}});
          event["conferenceData"]["entryPoints"] =
            json!([{"entryPointType":"video","uri":state.video.lock().await.clone()}]);
        }
        *state.event.lock().await = Some(event.clone());
        return axum::Json(event).into_response();
      }
      "DELETE" => {
        assert_eq!(parts.headers.get("if-match").unwrap(), "\"fixture-etag\"");
        let status = *state.delete_status.lock().await;
        if state.hold_delete.load(std::sync::atomic::Ordering::SeqCst) {
          state.delete_entered.add_permits(1);
          state.delete_release.acquire().await.unwrap().forget();
        }
        if (200..300).contains(&status) || status == 410 {
          *state.event.lock().await = None;
        }
        return if status == 410 {
          (
            StatusCode::GONE,
            axum::Json(json!({"error":{"errors":[{"reason":"deleted"}]}})),
          )
            .into_response()
        } else {
          StatusCode::from_u16(status).unwrap().into_response()
        };
      }
      _ => {}
    }
  }
  if path.ends_with("/events") && parts.method == axum::http::Method::POST {
    let status = *state.insert_status.lock().await;
    if status == 200 {
      let mut event = body.clone();
      event["status"] = json!("confirmed");
      event["etag"] = json!("\"fixture-etag\"");
      if event.pointer("/conferenceData/createRequest").is_some() {
        event["conferenceData"]["createRequest"]["status"] =
          json!({"statusCode":state.conference.lock().await.clone()});
        event["conferenceData"]["conferenceSolution"] = json!({"key":{"type":"hangoutsMeet"}});
        event["conferenceData"]["entryPoints"] =
          json!([{"entryPointType":"video","uri":state.video.lock().await.clone()}]);
      }
      *state.event.lock().await = Some(event.clone());
      if state.hold_insert.load(std::sync::atomic::Ordering::SeqCst) {
        state.insert_entered.add_permits(1);
        state.insert_release.acquire().await.unwrap().forget();
      }
      return axum::Json(event).into_response();
    }
    if state.hold_insert.load(std::sync::atomic::Ordering::SeqCst) {
      state.insert_entered.add_permits(1);
      state.insert_release.acquire().await.unwrap().forget();
    }
    return StatusCode::from_u16(status).unwrap().into_response();
  }
  StatusCode::NOT_FOUND.into_response()
}
pub(crate) struct Fixture {
  pub cfg: Config,
  pub clock: Arc<TestClock>,
  pub app: axum::Router,
  pub client: super::super::client::Client,
  pub session: String,
  pub csrf: String,
  pub provider: Arc<Provider>,
  server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
  fn drop(&mut self) {
    self.server.abort();
  }
}
impl Fixture {
  pub async fn new(db: &Db) -> Self {
    let (mut cfg, clock, old_app, totp) = fixture(db).await;
    drop(old_app);
    let provider = Arc::new(Provider::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = axum::Router::new()
      .fallback(handle)
      .with_state(provider.clone());
    let server = tokio::spawn(async move {
      axum::serve(listener, router).await.unwrap();
    });
    let mut google = super::super::config::GoogleConfig::parse(
      Some("client"),
      Some("SYNTHETIC_SECRET"),
      Some(&base64::engine::general_purpose::STANDARD.encode([71; 32])),
      Some("https://practice.test"),
    )
    .unwrap()
    .unwrap();
    google.test_endpoints = Some((
      format!("{base}/token").parse().unwrap(),
      format!("{base}/").parse().unwrap(),
    ));
    Arc::get_mut(&mut cfg).unwrap().google = Some(google);
    let (app, _, client) = crate::app::compose(cfg.clone(), db.clone(), clock.clone());
    let session = signed_in(&app, &totp, clock.now()).await;
    let csrf = field(
      &text(authenticated_get(&app, "/settings", &session).await).await,
      "csrf_token",
    );
    Self {
      cfg,
      clock,
      app,
      client: client.unwrap(),
      session,
      csrf,
      provider,
      server,
    }
  }
  pub async fn post(&self, path: &str, fields: &[(&str, &str)]) -> axum::response::Response {
    let mut data = vec![("csrf_token", self.csrf.as_str())];
    data.extend_from_slice(fields);
    post(
      &self.app,
      path,
      Some(&self.session),
      Some("https://practice.test"),
      &data,
    )
    .await
  }
  pub async fn state(&self) -> String {
    let response = self.post("/settings/google/connect", &[]).await;
    assert_eq!(response.status(), 303);
    reqwest::Url::parse(response.headers()["location"].to_str().unwrap())
      .unwrap()
      .query_pairs()
      .find(|(k, _)| k == "state")
      .unwrap()
      .1
      .into_owned()
  }
  pub async fn connect(&self) {
    let state = self.state().await;
    let response = authenticated_get(
      &self.app,
      &format!("/settings/google/callback?state={state}&code=SYNTHETIC_CODE"),
      &self.session,
    )
    .await;
    assert_eq!(response.headers()["location"], "/settings/google");
  }
  pub async fn row(&self, db: &Db) -> sea_orm::QueryResult {
    let pool = reconnect(db, 2).await;
    let row = pool
      .conn
      .query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT * FROM google_connection WHERE id=1",
      ))
      .await
      .unwrap()
      .unwrap();
    pool.conn.close().await.unwrap();
    row
  }
  pub async fn select(&self) -> axum::response::Response {
    self.select_with_blocking("1", &[]).await
  }
  pub async fn select_with_blocking(
    &self,
    selection_version: &str,
    blocking: &[&str],
  ) -> axum::response::Response {
    let mut fields = vec![
      ("generation", "1"),
      ("selection_version", selection_version),
      ("booking_calendar", "secondary@example.invalid"),
    ];
    fields.extend(
      blocking
        .iter()
        .map(|calendar| ("blocking_calendar", *calendar)),
    );
    self.post("/settings/google/calendars", &fields).await
  }
}
