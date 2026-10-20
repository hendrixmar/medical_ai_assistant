use super::{fixture::Fixture, *};
use serde_json::json;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn only_google_settings_document_allows_native_google_authorization_redirect() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    let page = authenticated_get(&f.app, "/settings/google", &f.session).await;
    let policy = page.headers()["content-security-policy"].to_str().unwrap();
    assert_eq!(
      policy
        .split(';')
        .map(str::trim)
        .find(|s| s.starts_with("form-action ")),
      Some("form-action 'self' https://accounts.google.com")
    );
    for directive in [
      "default-src 'none'",
      "script-src 'none'",
      "base-uri 'none'",
      "frame-ancestors 'none'",
    ] {
      assert!(policy.split(';').map(str::trim).any(|s| s == directive));
    }
    assert_eq!(page.headers()["referrer-policy"], "same-origin");
    for path in [
      "/login",
      "/dashboard",
      "/settings",
      "/settings/doctor-number",
      "/settings/whatsapp",
      "/aviso-de-privacidad",
    ] {
      let response = authenticated_get(&f.app, path, &f.session).await;
      let policy = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
      assert_eq!(
        policy
          .split(';')
          .map(str::trim)
          .find(|s| s.starts_with("form-action ")),
        Some("form-action 'self'")
      );
      assert!(!policy.contains("accounts.google.com"));
    }
    let redirect = f.post("/settings/google/connect", &[]).await;
    assert_eq!(redirect.status(), 303);
    assert!(!redirect.headers()["content-security-policy"]
      .to_str()
      .unwrap()
      .contains("accounts.google.com"));
    assert!(f.provider.calls.lock().await.is_empty());
  })
  .await;
}

#[test]
fn operator_configuration_is_all_or_nothing_with_static_errors() {
  use super::super::config::GoogleConfig;
  let key = base64::engine::general_purpose::STANDARD.encode([7; 32]);
  assert!(GoogleConfig::parse(None, None, None, None)
    .unwrap()
    .is_none());
  for (id, secret, key, origin) in [
    (Some(""), None, None, None),
    (Some("CLIENT_SENTINEL"), None, None, None),
    (
      Some("client"),
      Some("SECRET_SENTINEL"),
      None,
      Some("https://practice.test"),
    ),
    (
      Some("client"),
      Some("secret"),
      Some("KEY_SENTINEL"),
      Some("https://practice.test"),
    ),
    (Some("client"), Some("secret"), Some(key.as_str()), None),
    (
      Some("client"),
      Some("secret"),
      Some(key.as_str()),
      Some("https://practice.test/"),
    ),
    (
      Some("client"),
      Some("secret"),
      Some(key.as_str()),
      Some("http://practice.test"),
    ),
  ] {
    let error = GoogleConfig::parse(id, secret, key, origin)
      .err()
      .expect("invalid operator configuration");
    assert!(!error.contains("SENTINEL"));
  }
  assert!(GoogleConfig::parse(
    Some("client"),
    Some("secret"),
    Some(&key),
    Some("https://practice.test")
  )
  .unwrap()
  .is_some());
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn list_traversal_fails_closed_on_cycles_conflicts_page_byte_and_aggregate_limits() {
  with_database(|db|async move {
    let f=Fixture::new(&db).await;f.connect().await;
    let cases=vec![
      vec![json!({"items":[],"nextPageToken":"1"}),json!({"items":[],"nextPageToken":"1"})],
      vec![json!({"items":[{"id":"same","summary":"a","accessRole":"owner"}],"nextPageToken":"1"}),json!({"items":[{"id":"same","summary":"b","accessRole":"owner"}]})],
      (0..11).map(|n|json!({"items":[],"nextPageToken":(n+1).to_string()})).collect(),
      vec![json!({"items":(0..251).map(|n|json!({"id":n.to_string(),"summary":"a","accessRole":"owner"})).collect::<Vec<_>>()})],
      (0..6).map(|page|json!({"items":(0..100).map(|n|json!({"id":format!("{page}-{n}"),"summary":"x".repeat(3800),"accessRole":"owner"})).collect::<Vec<_>>(),"nextPageToken":(page+1).to_string()})).collect(),
    ];
    for pages in cases {
      *f.provider.pages.lock().await=pages;
      let body=text(authenticated_get(&f.app,"/settings/google",&f.session).await).await;
      assert!(!body.contains("action=\"/settings/google/calendars\""));
      assert_eq!(f.row(&db).await.try_get::<i64>("","selection_version").unwrap(),1);
    }
    *f.provider.list_raw.lock().await=Some(vec![b'x';512*1024+1]);
    let body=text(authenticated_get(&f.app,"/settings/google",&f.session).await).await;
    assert!(!body.contains("action=\"/settings/google/calendars\""));
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn calendar_redirects_are_not_followed_and_persistent_401_refreshes_once_then_requires_reconnect(
) {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    *f.provider.list_status.lock().await = 302;
    authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert_eq!(f.provider.calls.lock().await.len(), 2);
    *f.provider.list_status.lock().await = 401;
    authenticated_get(&f.app, "/settings/google", &f.session).await;
    let row = f.row(&db).await;
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "reauthorize");
    let calls = f.provider.calls.lock().await;
    assert_eq!(calls.iter().filter(|(_, p, _)| p == "/token").count(), 2);
    drop(calls);
    let before = f.provider.calls.lock().await.len();
    authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert_eq!(f.provider.calls.lock().await.len(), before);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn copied_or_corrupted_credentials_cannot_cross_generation_aad_or_claim_connectivity() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    let before = f
      .row(&db)
      .await
      .try_get::<Vec<u8>>("", "credential_ciphertext")
      .unwrap();
    db.conn
      .execute(Statement::from_string(
        DbBackend::Postgres,
        "UPDATE google_connection SET generation=generation+1",
      ))
      .await
      .unwrap();
    let response = authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert_eq!(response.status(), 503);
    assert_eq!(
      f.row(&db)
        .await
        .try_get::<Vec<u8>>("", "credential_ciphertext")
        .unwrap(),
      before
    );
    assert_eq!(f.provider.calls.lock().await.len(), 1);
  })
  .await;
}

#[derive(Clone)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
  fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
    self.0.lock().unwrap().extend_from_slice(b);
    Ok(b.len())
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
async fn actual_oauth_success_errors_rejections_and_provider_traffic_have_secret_safe_captured_logs(
) {
  if crate::app::tests::isolate_tracing_test(concat!(module_path!(), "::actual_oauth_success_errors_rejections_and_provider_traffic_have_secret_safe_captured_logs").trim_start_matches("server::")) {
    return;
  }
  use tracing::instrument::WithSubscriber;
  use tracing_subscriber::prelude::*;
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    let capture = Capture(Arc::new(std::sync::Mutex::new(vec![])));
    let subscriber = tracing_subscriber::registry()
      .with(crate::common::config::telemetry::safe_filter())
      .with(
        tracing_subscriber::fmt::layer()
          .json()
          .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
          .with_writer(capture.clone()),
      );
    async {
      f.connect().await;
      authenticated_get(&f.app, "/settings/google", &f.session).await;
      *f.provider.token_status.lock().await = 400;
      *f.provider.grant.lock().await =
        json!({"error":"invalid_grant","error_description":"PROVIDER_ERROR_SENTINEL"});
      f.connect().await;
      authenticated_get(
        &f.app,
        "/settings/google/callback?code=CODE_SENTINEL&state=STATE_SENTINEL&%73tate=x",
        &f.session,
      )
      .await;
    }
    .with_subscriber(subscriber)
    .await;
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("google_callback"));
    assert!(logs.contains("google_settings"));
    for forbidden in [
      "SENTINEL",
      "SYNTHETIC",
      "secondary@example.invalid",
      "meet.google.com",
      "code=",
      "state=",
      "csrf_token",
    ] {
      assert!(!logs.contains(forbidden), "sensitive data in logs");
    }
  })
  .await;
}
