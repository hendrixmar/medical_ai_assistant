use super::*;
use crate::modules::doctor::Clock;
use sea_orm::ConnectionTrait;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn failed_session_insert_rolls_back_totp_consumption_and_challenge_deletion() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    let (preauth, csrf) = challenge(&app).await;
    let code = totp.generate(clock.now() as u64).to_string();
    // Fail the final write, after the real transaction has updated the Doctor
    // and deleted the challenge. This constraint exists only in this test schema.
    db.conn
      .execute(super::super::store::sql(
        "ALTER TABLE doctor_sessions ADD CONSTRAINT reject_test_sessions CHECK (false)",
        vec![],
      ))
      .await
      .unwrap();
    let response = login(&app, &preauth, &csrf, PASSWORD, &code, "/dashboard").await;
    assert_eq!(response.status(), 503);
    assert!(!response.headers().contains_key("set-cookie"));
    assert_eq!(count(&db, "doctor_sessions").await, 0);
    assert_eq!(count(&db, "doctor_login_challenges").await, 1);
    let row = db
      .conn
      .query_one(super::super::store::sql(
        "SELECT last_used_totp_step FROM doctor WHERE id = 1",
        vec![],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      row.try_get::<i64>("", "last_used_totp_step").unwrap(),
      NOW / 30
    );
    assert!(!text(response).await.contains("reject_test_sessions"));
    db.conn
      .execute(super::super::store::sql(
        "ALTER TABLE doctor_sessions DROP CONSTRAINT reject_test_sessions",
        vec![],
      ))
      .await
      .unwrap();
    // The exact same proof/challenge must remain usable after recovery.
    let response = login(&app, &preauth, &csrf, PASSWORD, &code, "/dashboard").await;
    assert_eq!(response.status(), 303);
    let session = cookie(&response, "__Host-doctor_session");
    assert_eq!(
      authenticated_get(&app, "/dashboard", &session)
        .await
        .status(),
      200
    );
    assert_eq!(count(&db, "doctor_sessions").await, 1);
    assert_eq!(count(&db, "doctor_login_challenges").await, 0);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn storage_lookup_failure_denies_access_without_exposing_errors_or_revoking_session() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, clock.now()).await;
    db.conn
      .execute(super::super::store::sql(
        "ALTER TABLE doctor_sessions RENAME TO unavailable_doctor_sessions",
        vec![],
      ))
      .await
      .unwrap();
    for path in ["/login", "/dashboard", "/settings"] {
      let response = authenticated_get(&app, path, &session).await;
      assert_eq!(response.status(), 503);
      assert!(!response.headers().contains_key("set-cookie"));
      let html = text(response).await;
      assert!(html.contains("El acceso todavía no está listo"));
      assert!(!html.contains("Tus próximas citas"));
      assert!(!html.contains("does not exist"));
    }
    assert_eq!(get(&app, "/api/v1/health").await.status(), 200);
    db.conn
      .execute(super::super::store::sql(
        "ALTER TABLE unavailable_doctor_sessions RENAME TO doctor_sessions",
        vec![],
      ))
      .await
      .unwrap();
    assert_eq!(
      authenticated_get(&app, "/dashboard", &session)
        .await
        .status(),
      200
    );
    assert_eq!(count(&db, "doctor_sessions").await, 1);
  })
  .await;
}
