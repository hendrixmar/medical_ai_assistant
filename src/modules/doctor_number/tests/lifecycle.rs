use super::*;

/// Controlled random-code boundary; all issuance, display and redemption still use real public paths.
pub(super) async fn set_active_code(
  db: &crate::database::Db,
  cfg: &crate::common::config::Config,
  code: &str,
) {
  let state = db
    .conn
    .query_one(sql("SELECT * FROM doctor_number_state WHERE id=1"))
    .await
    .unwrap()
    .unwrap();
  let id: uuid::Uuid = state.try_get("", "challenge_id").unwrap();
  let key = cfg.doctor_totp_key.as_ref().unwrap();
  let subkey = super::super::store::resolve_fingerprint_key(&state, key)
    .await
    .unwrap();
  let fingerprint = super::super::crypto::fingerprint(&subkey, code).unwrap();
  let (ciphertext, nonce) = super::super::crypto::encrypt(key, id, code).unwrap();
  db.conn
    .execute(crate::modules::whatsapp::inbox::sql(
      "UPDATE doctor_number_challenges SET fingerprint=$2,ciphertext=$3,nonce=$4 WHERE id=$1",
      vec![
        id.into(),
        fingerprint.into(),
        ciphertext.into(),
        nonce.into(),
      ],
    ))
    .await
    .unwrap();
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn fingerprint_collision_budget_fails_closed_and_preserves_successful_issuance_history() {
  with_database(|db|async move {
    let p=Provider::new().await; let(_,clock,app,session,csrf)=setup(&db,&p).await;
    assert_eq!(issue(&app,&session,&csrf).await.status(),303); let code=active_code(&app,&session).await;
    clock.advance(60_000);
    db.conn.execute_unprepared("CREATE SEQUENCE allocation_attempts; CREATE FUNCTION collide_code() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('allocation_attempts'); NEW.fingerprint=(SELECT fingerprint FROM doctor_number_challenges LIMIT 1); RETURN NEW; END $$; CREATE TRIGGER collide_code BEFORE INSERT ON doctor_number_challenges FOR EACH ROW EXECUTE FUNCTION collide_code()").await.unwrap();
    assert_eq!(issue(&app,&session,&csrf).await.status(),503);
    assert_eq!(active_code(&app,&session).await,code);
    assert_eq!(count(&db,"doctor_number_challenges").await,1);
    assert_eq!(db.conn.query_one(sql("SELECT last_value FROM allocation_attempts")).await.unwrap().unwrap().try_get::<i64>("","last_value").unwrap(),32);
    db.conn.execute_unprepared("DROP TRIGGER collide_code ON doctor_number_challenges").await.unwrap();
    assert_eq!(issue(&app,&session,&csrf).await.status(),303);
    assert_eq!(count(&db,"doctor_number_challenges").await,2);
    assert_eq!(count(&db,"doctor_number_challenges WHERE state='superseded' AND ciphertext IS NULL").await,1);
    assert_ne!(active_code(&app,&session).await,code);
  }).await;
}

#[test]
fn ciphertext_is_bound_to_purpose_challenge_and_key() {
  let id = uuid::Uuid::now_v7();
  let key = [42; 32];
  let (ciphertext, nonce) = super::super::crypto::encrypt(&key, id, "000007").unwrap();
  assert_eq!(
    super::super::crypto::decrypt(&key, id, &ciphertext, &nonce).unwrap(),
    "000007"
  );
  assert!(super::super::crypto::decrypt(&[43; 32], id, &ciphertext, &nonce).is_err());
  assert!(super::super::crypto::decrypt(&key, uuid::Uuid::now_v7(), &ciphertext, &nonce).is_err());
  assert!(super::super::crypto::decrypt(&key, id, &ciphertext, &nonce[..11]).is_err());
  let (mut tampered, _) = (ciphertext.clone(), nonce.clone());
  tampered[0] ^= 1;
  assert!(super::super::crypto::decrypt(&key, id, &tampered, &nonce).is_err());
  let subkey = super::super::crypto::fingerprint_key(&key).unwrap();
  let fingerprint = super::super::crypto::fingerprint(&subkey, "000007").unwrap();
  use hmac::{Hmac, Mac};
  use sha2::Sha256;
  let mut direct = Hmac::<Sha256>::new_from_slice(&key).unwrap();
  direct.update(b"000007");
  assert_ne!(fingerprint, direct.finalize().into_bytes().to_vec());
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn leading_zero_code_survives_restart_encrypted_and_tampering_hides_all_state() {
  with_database(|db| async move {
    let p=Provider::new().await; let(cfg,clock,app,session,csrf)=setup(&db,&p).await;
    assert_eq!(issue(&app,&session,&csrf).await.status(),303); set_active_code(&db,&cfg,"000007").await;
    assert_eq!(active_code(&app,&session).await,"000007");
    let row=db.conn.query_one(sql("SELECT ciphertext,nonce,fingerprint FROM doctor_number_challenges")).await.unwrap().unwrap();
    let ciphertext:Vec<u8>=row.try_get("","ciphertext").unwrap(); assert!(!ciphertext.windows(6).any(|s|s==b"000007"));
    assert_eq!(row.try_get::<Vec<u8>>("","nonce").unwrap().len(),12);
    assert_eq!(row.try_get::<Vec<u8>>("","fingerprint").unwrap().len(),32);
    let fresh=crate::app::tests::reconnect(&db,2).await;
    let restarted=crate::app::router_with_clock(cfg.clone(),fresh.clone(),clock.clone());
    assert_eq!(active_code(&restarted,&session).await,"000007");
    *p.wire.status.lock().unwrap()=json!({"name":"default","status":"FAILED"});
    // Saved destination is stable despite provider changes or outages.
    let unavailable=text(authenticated_get(&restarted,PATH,&session).await).await;
    assert!(unavailable.contains("000007")); assert!(unavailable.contains("+525555555555"));
    assert_eq!(issue(&app,&session,&csrf).await.status(),503);
    assert_eq!(count(&db,"doctor_number_challenges").await,1);
    db.conn.execute_unprepared("UPDATE doctor_number_challenges SET ciphertext=set_byte(ciphertext,0,get_byte(ciphertext,0)#1)").await.unwrap();
    let page=authenticated_get(&restarted,PATH,&session).await; assert_eq!(page.status(),503);
    let body=text(page).await; assert!(!body.contains("000007")); assert!(!body.contains("action=\"/settings/doctor-number/challenge\""));
    fresh.conn.close().await.unwrap();
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn replacement_throttles_survive_restart_and_failed_provider_reads_do_not_replace() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let first = active_code(&app, &session).await;
    clock.advance(1);
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("BIND", "525512345678@c.us", &first, clock.now_millis()),
    )
    .await;
    clock.advance(59_999);
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let second = active_code(&app, &session).await;
    assert_ne!(first, second);
    assert!(text(authenticated_get(&app, PATH, &session).await)
      .await
      .contains("+525512345678"));
    clock.advance(1);
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("REPLAY", "525512345679@c.us", &first, clock.now_millis()),
    )
    .await;
    assert_eq!(
      count(&db, "outbox_messages WHERE template='doctor_verified'").await,
      1
    );
    for _ in 0..3 {
      clock.advance(60_000);
      assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    }
    let fresh = crate::app::tests::reconnect(&db, 2).await;
    let restarted = crate::app::router_with_clock(cfg.clone(), fresh.clone(), clock.clone());
    clock.advance(60_000);
    let limited = issue(&restarted, &session, &csrf).await;
    assert_eq!(limited.status(), 429);
    assert_eq!(limited.headers()["retry-after"], "3300");
    assert_eq!(count(&db, "doctor_number_challenges").await, 5);
    assert_eq!(
      count(
        &db,
        "doctor_number_challenges WHERE state<>'active' AND ciphertext IS NULL AND nonce IS NULL"
      )
      .await,
      4
    );
    clock.advance(3_300_000);
    assert_eq!(issue(&restarted, &session, &csrf).await.status(), 303);
    assert_eq!(count(&db, "doctor_number_challenges").await, 6);
    assert_eq!(
      count(
        &db,
        "doctor_number_state WHERE verified_phone='525512345678@c.us'"
      )
      .await,
      1
    );
    fresh.conn.close().await.unwrap();
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn exactly_five_eligible_wrong_guesses_exhaust_without_exposing_auth_state() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(1);
    for (i, body) in [
      "12345",
      "1234567",
      "１２３４５６",
      "mi número 525512345678",
      "123 456",
    ]
    .iter()
    .enumerate()
    {
      process(
        &db,
        &app,
        clock.as_ref(),
        &cfg,
        &message(
          &format!("INELIGIBLE{i}"),
          "525512345678@c.us",
          body,
          clock.now_millis(),
        ),
      )
      .await;
    }
    assert_eq!(
      count(&db, "doctor_number_challenges WHERE failures=0").await,
      1
    );
    let wrong = if code == "000000" { "000001" } else { "000000" };
    for i in 0..5 {
      let input = message(
        &format!("WRONG{i}"),
        "525512345679@c.us",
        wrong,
        clock.now_millis(),
      );
      process(&db, &app, clock.as_ref(), &cfg, &input).await;
      deliver(&app, &input).await;
      assert_eq!(
        count(
          &db,
          &format!("doctor_number_challenges WHERE failures={}", i + 1)
        )
        .await,
        1
      );
    }
    assert_eq!(
      count(
        &db,
        "doctor_number_challenges WHERE state='exhausted' AND ciphertext IS NULL AND nonce IS NULL"
      )
      .await,
      1
    );
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("EXHAUSTED", "525512345678@c.us", &code, clock.now_millis()),
    )
    .await;
    assert_eq!(
      count(&db, "doctor_number_state WHERE verified_phone IS NOT NULL").await,
      0
    );
    assert_eq!(
      count(&db, "outbox_messages WHERE template='menu'").await,
      11
    );
    let rows = db
      .conn
      .query_all(sql("SELECT rendered_text FROM outbox_messages"))
      .await
      .unwrap();
    for row in rows {
      let body: String = row.try_get("", "rendered_text").unwrap();
      assert!(!body.contains(&code));
      assert!(!body.contains("intentos"));
      assert!(!body.contains("código"));
    }
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn exact_expiry_uses_processing_clock_even_for_timely_arrival_and_get_never_mutates() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(1);
    deliver(
      &app,
      &message("QUEUED", "525512345678@c.us", &code, clock.now_millis()),
    )
    .await;
    crate::modules::whatsapp::jobs::run_once(&db, clock.as_ref())
      .await
      .unwrap();
    clock.advance(599_999);
    let page = text(authenticated_get(&app, PATH, &session).await).await;
    assert!(!page.contains("class=\"doctor-number-code\">"));
    assert_eq!(
      count(
        &db,
        "doctor_number_challenges WHERE state='active' AND ciphertext IS NOT NULL"
      )
      .await,
      1
    );
    crate::modules::conversation::jobs::run_once(&db, clock.as_ref(), &cfg, None)
      .await
      .unwrap();
    assert_eq!(
      count(
        &db,
        "doctor_number_challenges WHERE state='expired' AND ciphertext IS NULL"
      )
      .await,
      1
    );
    assert_eq!(
      count(&db, "doctor_number_state WHERE verified_phone IS NOT NULL").await,
      0
    );
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn failed_enqueue_and_deferred_commit_preserve_challenge_binding_and_pending_job() {
  with_database(|db| async move {
    let p=Provider::new().await; let(cfg,clock,app,session,csrf)=setup(&db,&p).await;
    assert_eq!(issue(&app,&session,&csrf).await.status(),303); let code=active_code(&app,&session).await; clock.advance(1);
    deliver(&app,&message("ROLLBACK","525512345678@c.us",&code,clock.now_millis())).await;
    crate::modules::whatsapp::jobs::run_once(&db,clock.as_ref()).await.unwrap();
    db.conn.execute_unprepared("CREATE FUNCTION reject_verification() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic fault'; END $$; CREATE TRIGGER reject_verification BEFORE INSERT ON outbox_messages FOR EACH ROW EXECUTE FUNCTION reject_verification()").await.unwrap();
    assert!(crate::modules::conversation::jobs::run_once(&db,clock.as_ref(),&cfg,None).await.is_err());
    assert_eq!(count(&db,"doctor_number_state WHERE verified_phone IS NOT NULL").await,0);
    assert_eq!(count(&db,"doctor_number_challenges WHERE state='active'").await,1);
    assert_eq!(count(&db,"outbox_messages").await,0);
    db.conn.execute_unprepared("DROP TRIGGER reject_verification ON outbox_messages; CREATE CONSTRAINT TRIGGER reject_commit AFTER UPDATE ON doctor_number_state DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_verification()").await.unwrap();
    assert!(crate::modules::conversation::jobs::run_once(&db,clock.as_ref(),&cfg,None).await.is_err());
    assert_eq!(count(&db,"doctor_number_state WHERE verified_phone IS NOT NULL").await,0);
    assert_eq!(count(&db,"doctor_number_challenges WHERE state='active'").await,1);
    assert_eq!(count(&db,"processing_jobs WHERE kind='process_conversation' AND status='pending'").await,1);
    assert_eq!(count(&db,"outbox_messages").await,0);
    db.conn.execute_unprepared("DROP TRIGGER reject_commit ON doctor_number_state").await.unwrap();
    crate::modules::conversation::jobs::run_once(&db, clock.as_ref(), &cfg, None).await.unwrap();
    assert_eq!(count(&db,"outbox_messages WHERE template='doctor_verified'").await,1);
  }).await;
}
