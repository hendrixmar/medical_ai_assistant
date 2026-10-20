use super::*;
use std::io::Write;
use std::sync::Mutex;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::fmt::format::FmtSpan;

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
  fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
    self.0.lock().unwrap().extend_from_slice(bytes);
    Ok(bytes.len())
  }
  fn flush(&mut self) -> std::io::Result<()> {
    Ok(())
  }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
  type Writer = Self;
  fn make_writer(&'a self) -> Self {
    self.clone()
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn request_logs_never_contain_caller_paths_queries_or_headers() {
  if isolate_tracing_test(
    concat!(
      module_path!(),
      "::request_logs_never_contain_caller_paths_queries_or_headers"
    )
    .trim_start_matches("server::"),
  ) {
    return;
  }
  with_database(|db| async move {
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
      .json()
      .with_max_level(tracing::Level::TRACE)
      .with_span_events(FmtSpan::FULL)
      .with_writer(capture.clone())
      .finish();
    async {
      let app = router(configuration(Environment::Production, "/docs"), db);
      for path in [
        "/api/v1/health?code=CODE_SENTINEL&state=STATE_SENTINEL",
        "/settings/google/callback?error_description=ERROR_SENTINEL",
        "/CALENDAR_ID_SENTINEL/MEET_LINK_SENTINEL",
      ] {
        let _ = app
          .clone()
          .oneshot(
            Request::builder()
              .uri(path)
              .header("cookie", "COOKIE_SENTINEL")
              .header("authorization", "Bearer TOKEN_SENTINEL")
              .header("x-request-id", "REQUEST_ID_SENTINEL")
              .body(Body::empty())
              .unwrap(),
          )
          .await
          .unwrap();
      }
    }
    .with_subscriber(subscriber)
    .await;
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("request"), "must capture actual HTTP spans");
    assert!(
      !output.contains("SENTINEL"),
      "caller-controlled data escaped into HTTP logs"
    );
  })
  .await;
}

#[test]
fn production_filter_blocks_dependency_credentials_even_with_unsafe_rust_log() {
  use tracing_subscriber::prelude::*;
  let capture = Capture(Arc::new(Mutex::new(Vec::new())));
  // safe_filter deliberately has no environment-dependent branch. Simulate the
  // permissive outer filter: the production layer still rejects dependency logs.
  let subscriber = tracing_subscriber::registry()
    .with(tracing_subscriber::EnvFilter::new("trace"))
    .with(
      tracing_subscriber::fmt::layer()
        .with_writer(capture.clone())
        .with_filter(crate::common::config::telemetry::safe_filter()),
    );
  tracing::subscriber::with_default(subscriber, || {
    tracing::info!(target: "server", "safe operation");
    tracing::trace!(target: "reqwest", "ACCESS_SENTINEL");
    tracing::debug!(target: "hyper", "CODE_SENTINEL");
    tracing::info!(target: "sqlx::query", "CIPHERTEXT_SENTINEL");
    tracing::error!(target: "reqwest", "ERROR_SENTINEL");
  });
  let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
  assert!(output.contains("safe operation"));
  assert!(!output.contains("SENTINEL"));
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn callback_envelope_is_outside_auth_headers_and_rejections() {
  with_database(|db| async move {
    let app = router(configuration(Environment::Production, "/docs"), db);
    for path in [
      "/settings/google/callback?code=CODE&state=STATE",
      "/settings/google/callback/?error=denied&state=STATE",
      "/settings/google/callback?state=one&%73tate=two&code=CODE",
    ] {
      for method in ["GET", "POST", "PUT"] {
        let response = app
          .clone()
          .oneshot(
            Request::builder()
              .method(method)
              .uri(path)
              .body(Body::empty())
              .unwrap(),
          )
          .await
          .unwrap();
        assert_eq!(response.status(), 303);
        assert_eq!(response.headers()["location"], "/settings/google");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert!(response
          .into_body()
          .collect()
          .await
          .unwrap()
          .to_bytes()
          .is_empty());
      }
    }
    let response = get(&app, "/login").await;
    assert_eq!(response.headers()["referrer-policy"], "same-origin");
  })
  .await;
}
