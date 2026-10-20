use super::*;
use crate::modules::doctor::{crypto, enrollment, store, Error};
use sea_orm::ConnectionTrait;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn enrollment_rejects_invalid_proof_and_passwords_and_cannot_replace_singleton() {
  with_database(|db| async move {
    db.run_migrations().await.unwrap();
    let cfg = enabled_configuration();
    let totp = crypto::totp(SECRET).unwrap();
    let code = totp.generate(NOW as u64).to_string();
    assert!(matches!(enrollment::enroll(&db, &cfg, PASSWORD.into(), SECRET, "not-a-code", NOW).await, Err(Error::Invalid)));
    assert_eq!(count(&db, "doctor").await, 0);
    for password in ["short".to_owned(), "p".repeat(73), "á".repeat(37)] {
      assert!(matches!(enrollment::enroll(&db, &cfg, password, SECRET, &code, NOW).await, Err(Error::Invalid)));
      assert_eq!(count(&db, "doctor").await, 0);
    }
    enrollment::enroll(&db, &cfg, PASSWORD.into(), SECRET, &code, NOW).await.unwrap();
    assert!(matches!(enrollment::enroll(&db, &cfg, "replacement password".into(), [99;20], &code, NOW).await, Err(Error::Exists)));
    let stored = store::doctor(&db).await.unwrap().unwrap();
    let hash: String = stored.try_get("", "password_hash").unwrap();
    assert_ne!(hash, PASSWORD);
    assert!(bcrypt::verify(PASSWORD, &hash).unwrap());
    assert!(!bcrypt::verify("replacement password", &hash).unwrap());
    let encrypted: Vec<u8> = stored.try_get("", "totp_ciphertext").unwrap();
    let nonce: Vec<u8> = stored.try_get("", "totp_nonce").unwrap();
    assert_ne!(encrypted, SECRET);
    assert_eq!(crypto::decrypt(&[42;32], &encrypted, &nonce).unwrap(), SECRET);
    assert!(crypto::decrypt(&[41;32], &encrypted, &nonce).is_err());
    let mut tampered = encrypted.clone(); tampered[0] ^= 1;
    assert!(crypto::decrypt(&[42;32], &tampered, &nonce).is_err());
    let second = db.conn.execute(store::sql("INSERT INTO doctor SELECT 2, password_hash, totp_ciphertext, totp_nonce, last_used_totp_step FROM doctor", vec![])).await;
    assert!(second.is_err(), "Database must enforce the singleton, not only application code");
    let clock = Arc::new(TestClock(std::sync::atomic::AtomicI64::new(NOW)));
    let app = crate::app::router_with_clock(cfg, db.clone(), clock);
    let (preauth, csrf) = challenge(&app).await;
    assert_eq!(login(&app, &preauth, &csrf, PASSWORD, &code, "/dashboard").await.status(), 401, "Enrollment proof is already consumed");
    assert_eq!(count(&db, "doctor").await, 1);
    assert_eq!(count(&db, "doctor_sessions").await, 0);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn concurrent_bootstrap_cannot_replace_another_doctor() {
  with_database(|db| async move {
    db.run_migrations().await.unwrap();
    let cfg = enabled_configuration();
    let code = crypto::totp(SECRET)
      .unwrap()
      .generate(NOW as u64)
      .to_string();
    let other_code = crypto::totp([19; 20])
      .unwrap()
      .generate(NOW as u64)
      .to_string();
    let (a, b) = tokio::join!(
      enrollment::enroll(&db, &cfg, PASSWORD.into(), SECRET, &code, NOW),
      enrollment::enroll(
        &db,
        &cfg,
        "other long password".into(),
        [19; 20],
        &other_code,
        NOW
      )
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(a, Err(Error::Exists)) || matches!(b, Err(Error::Exists)));
    assert_eq!(count(&db, "doctor").await, 1);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn operator_bootstrap_fails_before_prompt_without_key_migrations_or_when_already_enrolled() {
  with_database(|db| async move {
    let dir = std::env::temp_dir().join(format!("doctor-cli-failure-{}", uuid::Uuid::now_v7()));
    let path = dir.join("must-not-be-created");
    assert!(matches!(
      enrollment::bootstrap(&db, &configuration(), &path).await,
      Err(Error::Unavailable)
    ));
    assert!(matches!(
      enrollment::bootstrap(&db, &enabled_configuration(), &path).await,
      Err(Error::Unavailable)
    ));
    let (cfg, _, _, _) = fixture(&db).await;
    assert!(matches!(
      enrollment::bootstrap(&db, &cfg, &path).await,
      Err(Error::Exists)
    ));
    assert!(!dir.exists());
    assert_eq!(count(&db, "doctor").await, 1);
  })
  .await;
}

#[test]
fn encryption_uses_fresh_nonces_and_authenticates_secret() {
  let (a, nonce_a) = crypto::encrypt(&[42; 32], &SECRET).unwrap();
  let (b, nonce_b) = crypto::encrypt(&[42; 32], &SECRET).unwrap();
  assert_ne!(nonce_a, nonce_b);
  assert_ne!(a, b);
  assert!(crypto::decrypt(&[42; 32], &a, &nonce_b).is_err());
  assert!(crypto::decrypt(&[42; 32], &a, &[0; 11]).is_err());
}
