use super::intake::KEY;
use crate::{app::tests::with_database, modules::doctor::tests::*};
use axum::{
  body::Body,
  extract::{Request, State},
  response::Response,
  Router,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

const API_KEY: &str = "fixture-only-api-key-never-in-HTML";
const PNG: &str =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
#[derive(Clone)]
struct Fake(Arc<Mutex<Replies>>);
struct Replies {
  status: serde_json::Value,
  http_status: u16,
  start_http_status: Option<u16>,
  qr_http_status: Option<u16>,
  code_http_status: Option<u16>,
  oversized: bool,
  oversized_qr: bool,
  oversized_code: bool,
  stalled_qr: Option<Arc<tokio::sync::Notify>>,
  qr: serde_json::Value,
  code: serde_json::Value,
  code_bodies: Vec<String>,
  requests: Vec<String>,
}
struct Provider {
  fake: Fake,
  origin: String,
  task: tokio::task::JoinHandle<()>,
}
impl Drop for Provider {
  fn drop(&mut self) {
    self.task.abort();
  }
}
impl Provider {
  async fn new() -> Self {
    let fake = Fake(Arc::new(Mutex::new(Replies {
      status: json!({"name":"default","status":"STOPPED","engine":{}}),
      http_status: 200,
      start_http_status: None,
      qr_http_status: None,
      code_http_status: None,
      oversized: false,
      oversized_qr: false,
      oversized_code: false,
      stalled_qr: None,
      qr: json!({"mimetype":"image/png","data":PNG}),
      code: json!({"code":"WXYZ-9876","secret":"synthetic-decoy-never-in-HTML"}),
      code_bodies: vec![],
      requests: vec![],
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(reply).with_state(fake.clone());
    let task = tokio::spawn(async move {
      axum::serve(listener, app).await.unwrap();
    });
    Self { fake, origin, task }
  }
  fn status(&self, status: &str) {
    self.fake.0.lock().unwrap().status = json!({"name":"default","status":status,"engine":{"engine":"GOWS"},"me":{"id":"15550100200@c.us","pushName":API_KEY}});
  }
  fn requests(&self) -> Vec<String> {
    self.fake.0.lock().unwrap().requests.clone()
  }
  fn code_bodies(&self) -> Vec<String> {
    self.fake.0.lock().unwrap().code_bodies.clone()
  }
}
async fn reply(State(fake): State<Fake>, request: Request) -> Response {
  assert_eq!(request.headers().get("x-api-key").unwrap(), API_KEY);
  let is_code_path = request.uri().path() == "/api/default/auth/request-code";
  let (parts, body) = request.into_parts();
  let request_body = if is_code_path {
    String::from_utf8(
      axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap()
        .to_vec(),
    )
    .unwrap()
  } else {
    String::new()
  };
  let request = Request::from_parts(parts, Body::empty());
  assert_eq!(request.headers().get("x-api-key").unwrap(), API_KEY);
  let path = request.uri().to_string();
  let method = request.method().to_string();
  let (response, stalled) = {
    let mut replies = fake.0.lock().unwrap();
    replies.requests.push(format!("{method} {path}"));
    let is_qr = path == "/api/default/auth/qr?format=image";
    let is_code = path == "/api/default/auth/request-code";
    if is_code {
      replies.code_bodies.push(request_body.clone());
    }
    let (value, status) = match (method.as_str(), path.as_str()) {
      ("GET", "/api/sessions/default") => (replies.status.clone(), replies.http_status),
      ("POST", "/api/sessions/default/start") => (
        json!({"status":"WORKING", "secret":API_KEY}),
        replies.start_http_status.unwrap_or(replies.http_status),
      ),
      ("POST", "/api/default/auth/request-code") => (
        replies.code.clone(),
        replies.code_http_status.unwrap_or(replies.http_status),
      ),
      ("GET", "/api/default/auth/qr?format=image") => {
        assert_eq!(request.headers()["accept"], "application/json");
        (
          replies.qr.clone(),
          replies.qr_http_status.unwrap_or(replies.http_status),
        )
      }
      _ => panic!("Unexpected synthetic provider operation"),
    };
    let response = Response::builder()
      .status(status)
      .header("content-type", "application/json")
      .header("location", "/must-not-follow")
      .body(
        if replies.oversized
          || (is_qr && replies.oversized_qr)
          || (is_code && replies.oversized_code)
        {
          Body::new(Chunks(65))
        } else {
          Body::from(value.to_string())
        },
      )
      .unwrap();
    (
      response,
      if is_qr {
        replies.stalled_qr.clone()
      } else {
        None
      },
    )
  };
  if let Some(release) = stalled {
    release.notified().await;
  }
  response
}
// No Content-Length: exercise incremental bounds, not just the header fast path.
struct Chunks(usize);
impl hyper::body::Body for Chunks {
  type Data = axum::body::Bytes;
  type Error = std::convert::Infallible;
  fn poll_frame(
    mut self: std::pin::Pin<&mut Self>,
    _: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
    if self.0 == 0 {
      return std::task::Poll::Ready(None);
    }
    self.0 -= 1;
    std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(axum::body::Bytes::from(
      vec![b'x'; 32768],
    )))))
  }
}
async fn configured(db: &crate::database::Db, provider: &Provider) -> (Router, String) {
  let (mut cfg, clock, app, totp) = fixture(db).await;
  drop(app);
  Arc::get_mut(&mut cfg).unwrap().waha =
    super::super::config::WahaConfig::parse(Some(&provider.origin), Some(API_KEY), Some(KEY))
      .unwrap();
  let app = crate::app::router_with_clock(cfg, db.clone(), clock);
  let session = signed_in(&app, &totp, NOW + 30).await;
  (app, session)
}

#[test]
fn configuration_is_optional_complete_and_origin_only() {
  use super::super::config::WahaConfig;
  assert!(WahaConfig::parse(None, None, None).unwrap().is_none());
  for base in [
    "https://waha.test",
    "http://127.0.0.1:8080",
    "http://[::1]:8080",
    "http://localhost:8080",
  ] {
    assert!(WahaConfig::parse(Some(base), Some(API_KEY), Some(KEY))
      .unwrap()
      .is_some());
  }
  for base in [
    "",
    "http://waha.test",
    "https://waha.test/",
    "https://waha.test/path",
    "https://user:secret@waha.test",
    "https://waha.test?query",
    "https://waha.test#fragment",
    "file:///secret",
    "http://127.0.0.2",
  ] {
    assert!(WahaConfig::parse(Some(base), Some(API_KEY), Some(KEY)).is_err());
  }
  for (base, api, key) in [
    (Some("https://waha.test"), None, Some(KEY)),
    (None, Some(API_KEY), Some(KEY)),
    (Some("https://waha.test"), Some(API_KEY), None),
    (Some("https://waha.test"), Some(""), Some(KEY)),
    (
      Some("https://waha.test"),
      Some("injected\nheader"),
      Some(KEY),
    ),
    (Some("https://waha.test"), Some(API_KEY), Some("short")),
  ] {
    assert!(WahaConfig::parse(base, api, key).is_err());
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn auth_and_csrf_prevent_all_outbound_provider_operations() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    for path in ["/settings/whatsapp", "/settings/whatsapp/qr"] {
      let response = get(&app, path).await;
      assert_eq!(response.status(), 303);
      assert_eq!(response.headers()["cache-control"], "no-store");
    }
    assert_eq!(
      get(&app, "/settings/whatsapp").await.headers()["location"],
      "/login?return_to=/settings/whatsapp"
    );
    assert!(text(get(&app, "/login?return_to=/settings/whatsapp").await)
      .await
      .contains("name=\"return_to\" value=\"/settings/whatsapp\""));
    assert!(
      text(get(&app, "/login?return_to=/settings/whatsapp/qr").await)
        .await
        .contains("name=\"return_to\" value=\"/dashboard\"")
    );
    let csrf = field(
      &text(authenticated_get(&app, "/dashboard", &session).await).await,
      "csrf_token",
    );
    for (cookie, origin, token, expected) in [
      (None, Some("https://practice.test"), csrf.as_str(), 401),
      (Some(session.as_str()), None, csrf.as_str(), 403),
      (
        Some(session.as_str()),
        Some("https://evil.test"),
        csrf.as_str(),
        403,
      ),
      (
        Some(session.as_str()),
        Some("https://practice.test"),
        "incorrect",
        403,
      ),
    ] {
      assert_eq!(
        post(
          &app,
          "/settings/whatsapp/start",
          cookie,
          origin,
          &[("csrf_token", token)]
        )
        .await
        .status(),
        expected
      );
    }
    assert!(provider.requests().is_empty());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn every_provider_state_is_reported_without_secrets_or_optimistic_success() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    for (status, label) in [
      ("STOPPED", "Conexión detenida"),
      ("STARTING", "Conexión iniciándose"),
      ("SCAN_QR_CODE", "Vincula el teléfono"),
      ("PASSKEY_REQUIRED", "solicita una clave"),
      ("PASSKEY_CONFIRMATION_REQUIRED", "solicita confirmar"),
      ("WORKING", "WhatsApp conectado"),
      ("FAILED", "La conexión falló"),
      ("NEW_PRIVATE_STATE", "Estado de conexión no reconocido"),
    ] {
      provider.status(status);
      let response = authenticated_get(&app, "/settings/whatsapp", &session).await;
      assert_eq!(response.status(), 200);
      assert_eq!(response.headers()["referrer-policy"], "same-origin");
      assert!(response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("img-src 'self'"));
      let body = text(response).await;
      assert!(body.contains(label), "{status}");
      assert!(!body.contains(API_KEY));
      assert!(!body.contains(KEY));
      assert!(!body.contains(PNG));
      assert!(!body.contains(&provider.origin));
      assert!(!body.contains("NEW_PRIVATE_STATE"));
      assert_eq!(body.contains("+15550100200"), status == "WORKING");
      assert_eq!(body.contains("<img"), status == "SCAN_QR_CODE");
      assert_eq!(
        body.contains("/settings/whatsapp/request-code"),
        status == "SCAN_QR_CODE",
        "{status}"
      );
      assert!(!body.contains("WXYZ-9876"), "{status}");
    }
    provider.status("WORKING");
    provider.fake.0.lock().unwrap().status["me"]["id"] = json!("<script>@c.us");
    let body = text(authenticated_get(&app, "/settings/whatsapp", &session).await).await;
    assert!(!body.contains("<script>"));
    assert!(!body.contains("Número conectado:"));
    assert!(provider
      .requests()
      .iter()
      .all(|r| r == "GET /api/sessions/default"));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn qr_is_protected_png_no_store_only_during_reported_qr_state() {
  use base64::Engine;
  use http_body_util::BodyExt;
  with_database(|db| async move {
    let provider=Provider::new().await;
    let (app,session)=configured(&db,&provider).await;
    assert_eq!(authenticated_get(&app,"/settings/whatsapp/qr",&session).await.status(),409);
    provider.status("SCAN_QR_CODE");
    let response=authenticated_get(&app,"/settings/whatsapp/qr",&session).await;
    assert_eq!(response.status(),200); assert_eq!(response.headers()["content-type"],"image/png"); assert_eq!(response.headers()["cache-control"],"no-store");
    assert_eq!(response.into_body().collect().await.unwrap().to_bytes().as_ref(),base64::engine::general_purpose::STANDARD.decode(PNG).unwrap());
    for qr in [json!({"mimetype":"image/jpeg","data":PNG}),json!({"mimetype":"image/png","data":"aW52YWxpZA=="}),json!({"mimetype":"image/png","data":"!!!"})] {
      provider.fake.0.lock().unwrap().qr=qr;
      assert_eq!(authenticated_get(&app,"/settings/whatsapp/qr",&session).await.status(),503);
    }
    assert_eq!(provider.requests().iter().filter(|r|r.as_str()=="GET /api/default/auth/qr?format=image").count(),4);
    let mut oversized=vec![0;1024*1024+1]; oversized[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    provider.fake.0.lock().unwrap().qr=json!({"mimetype":"image/png","data":base64::engine::general_purpose::STANDARD.encode(oversized)});
    assert_eq!(authenticated_get(&app,"/settings/whatsapp/qr",&session).await.status(),503);
    let before=provider.requests().len();
    assert_eq!(get(&app,"/settings/whatsapp/qr").await.status(),303);
    assert_eq!(provider.requests().len(),before);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn provider_errors_unknown_engine_bounds_and_redirects_fail_closed() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    let csrf = field(
      &text(authenticated_get(&app, "/dashboard", &session).await).await,
      "csrf_token",
    );
    for status in [301, 302, 307, 401, 404, 500] {
      provider.fake.0.lock().unwrap().http_status = status;
      let body = text(authenticated_get(&app, "/settings/whatsapp", &session).await).await;
      assert!(body.contains("No se pudo consultar"));
      assert!(!body.contains(API_KEY));
      assert_eq!(
        post(
          &app,
          "/settings/whatsapp/start",
          Some(&session),
          Some("https://practice.test"),
          &[("csrf_token", &csrf)]
        )
        .await
        .status(),
        503
      );
    }
    provider.fake.0.lock().unwrap().http_status = 200;
    provider.fake.0.lock().unwrap().oversized = true;
    assert!(
      text(authenticated_get(&app, "/settings/whatsapp", &session).await)
        .await
        .contains("No se pudo consultar")
    );
    provider.fake.0.lock().unwrap().oversized = false;
    for status in [
      json!({"name":"default","status":"WORKING","engine":{"engine":"WEBJS"}}),
      json!({"name":"foreign","status":"WORKING"}),
      json!({"error":API_KEY}),
    ] {
      provider.fake.0.lock().unwrap().status = status;
      assert!(
        text(authenticated_get(&app, "/settings/whatsapp", &session).await)
          .await
          .contains("No se pudo consultar")
      );
      assert_eq!(
        post(
          &app,
          "/settings/whatsapp/start",
          Some(&session),
          Some("https://practice.test"),
          &[("csrf_token", &csrf)]
        )
        .await
        .status(),
        503
      );
    }
    assert!(provider
      .requests()
      .iter()
      .all(|r| r == "GET /api/sessions/default"));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn successful_status_does_not_hide_start_or_qr_operation_failures() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    let csrf = field(
      &text(authenticated_get(&app, "/dashboard", &session).await).await,
      "csrf_token",
    );
    for qr in [false, true] {
      provider.status(if qr { "SCAN_QR_CODE" } else { "STOPPED" });
      for status in [307, 401, 500] {
        {
          let mut replies = provider.fake.0.lock().unwrap();
          if qr {
            replies.qr_http_status = Some(status);
          } else {
            replies.start_http_status = Some(status);
          }
        }
        let before = provider.requests().len();
        let response = if qr {
          authenticated_get(&app, "/settings/whatsapp/qr", &session).await
        } else {
          post(
            &app,
            "/settings/whatsapp/start",
            Some(&session),
            Some("https://practice.test"),
            &[("csrf_token", &csrf)],
          )
          .await
        };
        assert_eq!(response.status(), 503);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = text(response).await;
        for secret in [API_KEY, KEY, PNG, provider.origin.as_str()] {
          assert!(!body.contains(secret));
        }
        let requests = provider.requests();
        assert_eq!(requests.len(), before + 2);
        assert_eq!(requests[before], "GET /api/sessions/default");
        assert_eq!(
          requests[before + 1],
          if qr {
            "GET /api/default/auth/qr?format=image"
          } else {
            "POST /api/sessions/default/start"
          }
        );
      }
    }
    {
      let mut replies = provider.fake.0.lock().unwrap();
      replies.qr_http_status = None;
      replies.oversized_qr = true;
    }
    let before = provider.requests().len();
    assert_eq!(
      authenticated_get(&app, "/settings/whatsapp/qr", &session)
        .await
        .status(),
      503
    );
    assert_eq!(
      &provider.requests()[before..],
      [
        "GET /api/sessions/default",
        "GET /api/default/auth/qr?format=image"
      ]
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn stalled_qr_operation_fails_within_client_deadline() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    provider.status("SCAN_QR_CODE");
    let release = Arc::new(tokio::sync::Notify::new());
    provider.fake.0.lock().unwrap().stalled_qr = Some(release.clone());
    // A bounded guard, not an elapsed-time assertion: the real client's five-second
    // deadline must win before the application's fifteen-second outer timeout.
    let result = tokio::time::timeout(
      std::time::Duration::from_secs(10),
      authenticated_get(&app, "/settings/whatsapp/qr", &session),
    )
    .await;
    release.notify_one();
    let response = result.expect("stalled provider escaped its client deadline");
    assert_eq!(response.status(), 503);
    assert!(!text(response).await.contains(API_KEY));
    assert_eq!(
      provider.requests(),
      [
        "GET /api/sessions/default",
        "GET /api/default/auth/qr?format=image"
      ]
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn protected_status_is_fresh_and_start_never_infers_working() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    assert!(provider.requests().is_empty());
    let response = authenticated_get(&app, "/settings/whatsapp", &session).await;
    assert_eq!(response.status(), 200);
    let body = text(response).await;
    assert!(body.contains("Conexión detenida"));
    assert!(!body.contains(API_KEY));
    let csrf = field(&body, "csrf_token");
    let response = post(
      &app,
      "/settings/whatsapp/start",
      Some(&session),
      Some("https://practice.test"),
      &[("csrf_token", &csrf)],
    )
    .await;
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/settings/whatsapp");
    assert!(
      text(authenticated_get(&app, "/settings/whatsapp", &session).await)
        .await
        .contains("Conexión detenida")
    );
    assert_eq!(
      provider
        .requests()
        .iter()
        .filter(|p| p.starts_with("POST"))
        .count(),
      1
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn request_code_happy_path_shows_code_without_secrets() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    provider.status("SCAN_QR_CODE");
    let page = text(authenticated_get(&app, "/settings/whatsapp", &session).await).await;
    assert!(page.contains("/settings/whatsapp/request-code"));
    assert!(!page.contains("WXYZ-9876"));
    let csrf = field(&page, "csrf_token");
    let before = provider.requests().len();
    let response = post(
      &app,
      "/settings/whatsapp/request-code",
      Some(&session),
      Some("https://practice.test"),
      &[("csrf_token", &csrf), ("phone", "+52 123-456-7890")],
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("location"));
    let body = text(response).await;
    assert!(body.contains("Tu código de vinculación:"));
    assert!(body.contains("WXYZ-9876"));
    assert!(body.contains("Vincular con un código"));
    for secret in [
      API_KEY,
      KEY,
      "synthetic-decoy-never-in-HTML",
      provider.origin.as_str(),
    ] {
      assert!(!body.contains(secret), "{secret}");
    }
    assert_eq!(
      &provider.requests()[before..],
      [
        "GET /api/sessions/default",
        "POST /api/default/auth/request-code"
      ]
    );
    let bodies = provider.code_bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
      serde_json::from_str::<serde_json::Value>(&bodies[0]).unwrap(),
      json!({"phoneNumber": "521234567890"})
    );
    let page = text(authenticated_get(&app, "/settings/whatsapp", &session).await).await;
    assert!(!page.contains("WXYZ-9876"));
    assert!(page.contains("/settings/whatsapp/request-code"));
    assert_eq!(
      authenticated_get(&app, "/settings/whatsapp/qr", &session)
        .await
        .status(),
      200
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn request_code_rejects_bad_phone_without_provider_calls() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    provider.status("SCAN_QR_CODE");
    let csrf = field(
      &text(authenticated_get(&app, "/dashboard", &session).await).await,
      "csrf_token",
    );
    for (cookie, origin, token, expected) in [
      (None, Some("https://practice.test"), csrf.as_str(), 401),
      (Some(session.as_str()), None, csrf.as_str(), 403),
      (
        Some(session.as_str()),
        Some("https://evil.test"),
        csrf.as_str(),
        403,
      ),
      (
        Some(session.as_str()),
        Some("https://practice.test"),
        "incorrect",
        403,
      ),
    ] {
      assert_eq!(
        post(
          &app,
          "/settings/whatsapp/request-code",
          cookie,
          origin,
          &[("csrf_token", token), ("phone", "521234567890")]
        )
        .await
        .status(),
        expected
      );
    }
    assert!(provider.requests().is_empty());
    for bad in ["abc", "123", "+52 1 (234)", "0123456789"] {
      let response = post(
        &app,
        "/settings/whatsapp/request-code",
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", &csrf), ("phone", bad)],
      )
      .await;
      assert_eq!(response.status(), 422, "{bad}");
      let body = text(response).await;
      assert!(
        body.contains("Escribe el número con clave de país, solo dígitos"),
        "{bad}"
      );
      assert!(body.contains(&format!("value=\"{bad}\"")), "{bad}");
      assert!(body.contains("/settings/whatsapp/request-code"), "{bad}");
    }
    assert!(provider.requests().is_empty());
    assert!(provider.code_bodies().is_empty());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn request_code_wrong_state_makes_no_code_call() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    let csrf = field(
      &text(authenticated_get(&app, "/dashboard", &session).await).await,
      "csrf_token",
    );
    for (status, message) in [
      (
        "STOPPED",
        "Primero inicia la conexión con el botón Conectar WhatsApp.",
      ),
      (
        "WORKING",
        "Actualiza el estado antes de pedir un código de vinculación.",
      ),
      (
        "STARTING",
        "Actualiza el estado antes de pedir un código de vinculación.",
      ),
      (
        "PASSKEY_REQUIRED",
        "Actualiza el estado antes de pedir un código de vinculación.",
      ),
    ] {
      provider.status(status);
      let before = provider.requests().len();
      let response = post(
        &app,
        "/settings/whatsapp/request-code",
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", &csrf), ("phone", "521234567890")],
      )
      .await;
      assert_eq!(response.status(), 409, "{status}");
      let body = text(response).await;
      assert!(body.contains(message), "{status}");
      assert!(!body.contains("WXYZ-9876"), "{status}");
      if status == "PASSKEY_REQUIRED" {
        assert!(
          !body.contains("/settings/whatsapp/request-code"),
          "{status}"
        );
        assert!(!body.contains("Pedir código de vinculación"), "{status}");
      }
      assert_eq!(
        &provider.requests()[before..],
        ["GET /api/sessions/default"],
        "{status}"
      );
    }
    assert!(provider.code_bodies().is_empty());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn request_code_provider_failures_are_sanitized() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (app, session) = configured(&db, &provider).await;
    provider.status("SCAN_QR_CODE");
    let csrf = field(
      &text(authenticated_get(&app, "/dashboard", &session).await).await,
      "csrf_token",
    );
    for status in [500u16, 307, 422] {
      provider.fake.0.lock().unwrap().code_http_status = Some(status);
      let response = post(
        &app,
        "/settings/whatsapp/request-code",
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", &csrf), ("phone", "521234567890")],
      )
      .await;
      assert_eq!(response.status(), 503, "{status}");
      assert!(!response.headers().contains_key("location"));
      let body = text(response).await;
      assert!(
        body.contains("No se pudo pedir el código de vinculación."),
        "{status}"
      );
      for secret in [API_KEY, KEY, "synthetic-decoy-never-in-HTML", "WXYZ-9876"] {
        assert!(!body.contains(secret), "{status}");
      }
    }
    provider.fake.0.lock().unwrap().code_http_status = None;
    for code in [
      json!({"code": 123}),
      json!({"missing": "x"}),
      json!({"code": "ab"}),
      json!({"code": "bad code!"}),
      json!({"code": "  WXYZ-9876  ", "secret": "synthetic-decoy-never-in-HTML"}),
    ] {
      let expect_ok = code["code"]
        .as_str()
        .is_some_and(|c| c.trim() == "WXYZ-9876");
      provider.fake.0.lock().unwrap().code = code;
      let response = post(
        &app,
        "/settings/whatsapp/request-code",
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", &csrf), ("phone", "521234567890")],
      )
      .await;
      assert_eq!(response.status(), if expect_ok { 200 } else { 503 });
      let body = text(response).await;
      assert!(!body.contains("synthetic-decoy-never-in-HTML"));
      if !expect_ok {
        assert!(body.contains("No se pudo pedir el código de vinculación."));
      }
    }
    provider.fake.0.lock().unwrap().code =
      json!({"code":"WXYZ-9876","secret":"synthetic-decoy-never-in-HTML"});
    provider.fake.0.lock().unwrap().oversized_code = true;
    let response = post(
      &app,
      "/settings/whatsapp/request-code",
      Some(&session),
      Some("https://practice.test"),
      &[("csrf_token", &csrf), ("phone", "521234567890")],
    )
    .await;
    assert_eq!(response.status(), 503);
    assert!(!text(response).await.contains("WXYZ-9876"));
    assert!(provider.requests().chunks(2).all(|pair| pair
      == [
        "GET /api/sessions/default",
        "POST /api/default/auth/request-code"
      ]));
  })
  .await;
}
