use super::*;
use crate::common::config::{parse_public_origin, parse_totp_key};

#[test]
fn auth_configuration_requires_canonical_https_origin_and_explicit_32_byte_key() {
  for origin in [
    "https://practice.test",
    "https://practice.test:8443",
    "https://127.0.0.1:8443",
  ] {
    assert_eq!(parse_public_origin(origin).unwrap(), origin);
  }
  for origin in [
    "http://practice.test",
    "https://practice.test/",
    "https://practice.test/path",
    "https://practice.test?query=secret",
    "https://practice.test#fragment",
    "https://user:secret@practice.test",
    "https://PRACTICE.test",
    "https://practice.test:443",
    "null",
    "https://",
    " https://practice.test",
  ] {
    let error = parse_public_origin(origin).unwrap_err();
    assert_eq!(error, "Invalid PUBLIC_ORIGIN");
  }
  use base64::Engine;
  let key = base64::engine::general_purpose::STANDARD.encode([42; 32]);
  assert_eq!(parse_totp_key(&key).unwrap(), [42; 32]);
  for key in [
    "".into(),
    "not a key".into(),
    base64::engine::general_purpose::STANDARD.encode([42; 31]),
    base64::engine::general_purpose::STANDARD.encode([42; 33]),
  ] {
    assert_eq!(parse_totp_key(&key).unwrap_err(), "Invalid DOCTOR_TOTP_KEY");
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn either_missing_setting_unenrolled_doctor_and_wrong_key_fail_closed() {
  with_database(|db| async move {
    db.run_migrations().await.unwrap();
    assert_eq!(
      get(&router(enabled_configuration(), db.clone()), "/login")
        .await
        .status(),
      503
    );
    let (cfg, clock, _, totp) = fixture(&db).await;
    for missing_origin in [true, false] {
      let mut cfg = enabled_configuration();
      if missing_origin {
        Arc::get_mut(&mut cfg).unwrap().public_origin = None;
      } else {
        Arc::get_mut(&mut cfg).unwrap().doctor_totp_key = None;
      }
      let app = router(cfg, db.clone());
      assert_eq!(get(&app, "/login").await.status(), 503);
      assert_eq!(get(&app, "/dashboard").await.status(), 503);
      assert_eq!(get(&app, "/api/v1/health").await.status(), 200);
    }
    let mut wrong = enabled_configuration();
    Arc::get_mut(&mut wrong).unwrap().doctor_totp_key = Some([1; 32]);
    let app = crate::app::router_with_clock(wrong, db.clone(), clock);
    let (preauth, csrf) = challenge(&app).await;
    let response = login(
      &app,
      &preauth,
      &csrf,
      PASSWORD,
      &totp.generate((NOW + 30) as u64).to_string(),
      "/dashboard",
    )
    .await;
    assert_eq!(response.status(), 503);
    assert_eq!(count(&db, "doctor_sessions").await, 0);
    drop(cfg);
  })
  .await;
}
