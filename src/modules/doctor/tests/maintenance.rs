use super::*;
use crate::modules::doctor::{maintenance, Clock};
use sea_orm::ConnectionTrait;

fn sql(query: &str, values: Vec<sea_orm::Value>) -> sea_orm::Statement {
  super::super::store::sql(query, values)
}

/// Independent original-domain fingerprint: HMAC subkey derived from the
/// original master, then HMAC of the code. Never prints key/code bytes.
fn original_fingerprint(code: &str) -> Vec<u8> {
  use hmac::{Hmac, Mac};
  use sha2::Sha256;
  let mut mac = Hmac::<Sha256>::new_from_slice(&[42; 32]).unwrap();
  mac.update(b"medical-assistant:doctor-number:fingerprint:v1");
  let subkey = mac.finalize().into_bytes();
  let mut mac = Hmac::<Sha256>::new_from_slice(&subkey).unwrap();
  mac.update(code.as_bytes());
  mac.finalize().into_bytes().to_vec()
}

fn with_key(key: [u8; 32]) -> crate::common::config::Config {
  let mut cfg = enabled_configuration();
  Arc::get_mut(&mut cfg).unwrap().doctor_totp_key = Some(key);
  cfg
}

async fn snapshot(db: &crate::database::Db, query: &str) -> String {
  db.conn
    .query_one(sql(query, vec![]))
    .await
    .unwrap()
    .unwrap()
    .try_get::<String>("", "snapshot")
    .unwrap()
}

const DOCTOR_SNAPSHOT: &str = "SELECT encode(totp_ciphertext,'hex')||':'||encode(totp_nonce,'hex')||':'||last_used_totp_step::text AS snapshot FROM doctor WHERE id=1";
const STATE_SNAPSHOT: &str = "SELECT coalesce(verified_phone,'')||':'||coalesce(original_target,'')||':'||coalesce(contact_id::text,'')||':'||coalesce(inbox_message_id,'')||':'||coalesce(verified_at::text,'')||':'||coalesce(practice_phone,'')||':'||coalesce(encode(fingerprint_key_ciphertext,'hex'),'')||':'||coalesce(encode(fingerprint_key_nonce,'hex'),'') AS snapshot FROM doctor_number_state WHERE id=1";
const VERIFIED_SNAPSHOT: &str = "SELECT coalesce(verified_phone,'')||':'||coalesce(original_target,'')||':'||coalesce(contact_id::text,'')||':'||coalesce(inbox_message_id,'')||':'||coalesce(verified_at::text,'')||':'||coalesce(practice_phone,'') AS snapshot FROM doctor_number_state WHERE id=1";
const CHALLENGES_SNAPSHOT: &str = "SELECT coalesce(string_agg(id::text||':'||state||':'||encode(fingerprint,'hex')||':'||coalesce(encode(ciphertext,'hex'),'')||':'||issued_at::text, ',' ORDER BY issued_at),'') AS snapshot FROM doctor_number_challenges";

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn doctor_rotation_twice_preserves_login_binding_and_original_fingerprints() {
  with_database(|db| async move {
    let (cfg, clock, _app, totp) = fixture(&db).await;
    // Verified binding fixture (all-or-none columns, real FK targets).
    let contact = uuid::Uuid::now_v7();
    db.conn.execute(sql("INSERT INTO contacts(id,created_at) VALUES($1,1)", vec![contact.into()])).await.unwrap();
    db.conn.execute(sql("INSERT INTO inbox_messages(message_id,original_chat_identity,server_timestamp,arrived_at,raw_envelope) VALUES('synthetic-binding-1','521234567890@c.us',1,to_timestamp(1),'\\x00')", vec![])).await.unwrap();
    db.conn.execute(sql("UPDATE doctor_number_state SET verified_phone='521234567890@c.us',original_target='521234567890@c.us',contact_id=$1,inbox_message_id='synthetic-binding-1',verified_at=123,practice_phone='525555555555@c.us' WHERE id=1", vec![contact.into()])).await.unwrap();

    // Pre-rotation challenge in the original fingerprint domain.
    crate::modules::doctor_number::store::issue(
      &db,
      clock.as_ref(),
      &[42; 32],
      "525555555555@c.us",
    )
    .await
    .unwrap();
    let first_code = crate::modules::doctor_number::store::load(&db, clock.as_ref(), &[42; 32])
      .await
      .unwrap()
      .code
      .unwrap();
    let first_fingerprint = db.conn.query_one(sql("SELECT fingerprint FROM doctor_number_challenges", vec![])).await.unwrap().unwrap().try_get::<Vec<u8>>("", "fingerprint").unwrap();
    assert!(first_fingerprint == original_fingerprint(&first_code));
    let binding_before = snapshot(&db, VERIFIED_SNAPSHOT).await;
    let doctor_before = snapshot(&db, DOCTOR_SNAPSHOT).await;

    // First rotation: [42] -> [43].
    let cfg1 = with_key([43; 32]);
    assert_eq!(maintenance::rotate_key(&db, &cfg, &[43; 32]).await.unwrap(), 1);
    assert_eq!(snapshot(&db, VERIFIED_SNAPSHOT).await, binding_before);
    assert_eq!(snapshot(&db, DOCTOR_SNAPSHOT).await.rsplit(':').next(), doctor_before.rsplit(':').next());
    let retired = db.conn.query_one(sql("SELECT state,ciphertext FROM doctor_number_challenges", vec![])).await.unwrap().unwrap();
    assert_eq!(retired.try_get::<String>("", "state").unwrap(), "superseded");
    assert!(retired.try_get::<Option<Vec<u8>>>("", "ciphertext").unwrap().is_none());
    let hist = db.conn.query_one(sql("SELECT fingerprint FROM doctor_number_challenges", vec![])).await.unwrap().unwrap().try_get::<Vec<u8>>("", "fingerprint").unwrap();
    assert!(hist == first_fingerprint);

    // Same password/authenticator under the new key; replay still rejected; old key 503.
    let app1 = crate::app::router_with_clock(cfg1.clone(), db.clone(), clock.clone());
    let code = totp.generate(clock.now() as u64).to_string();
    let _session = signed_in(&app1, &totp, clock.now()).await;
    let (preauth, csrf) = challenge(&app1).await;
    assert_eq!(login(&app1, &preauth, &csrf, PASSWORD, &code, "/dashboard").await.status(), 401);
    let app_old = crate::app::router_with_clock(cfg.clone(), db.clone(), clock.clone());
    let (preauth, csrf) = challenge(&app_old).await;
    let fresh = totp.generate((clock.now() + 30) as u64).to_string();
    assert_eq!(login(&app_old, &preauth, &csrf, PASSWORD, &fresh, "/dashboard").await.status(), 503);

    // Real post-rotation issuance still lands in the original fingerprint domain.
    clock.advance(61);
    crate::modules::doctor_number::store::issue(&db, clock.as_ref(), &[43; 32], "525555555555@c.us").await.unwrap();
    let second_code = crate::modules::doctor_number::store::load(&db, clock.as_ref(), &[43; 32]).await.unwrap().code.unwrap();
    let second_fp = db.conn.query_one(sql("SELECT fingerprint FROM doctor_number_challenges WHERE state='active'", vec![])).await.unwrap().unwrap().try_get::<Vec<u8>>("", "fingerprint").unwrap();
    assert!(second_fp == original_fingerprint(&second_code));

    // Second rotation: [43] -> [44]; same guarantees.
    let cfg2 = with_key([44; 32]);
    assert_eq!(maintenance::rotate_key(&db, &cfg1, &[44; 32]).await.unwrap(), 1);
    let app2 = crate::app::router_with_clock(cfg2.clone(), db.clone(), clock.clone());
    let _session = signed_in(&app2, &totp, clock.now()).await;
    clock.advance(61);
    crate::modules::doctor_number::store::issue(&db, clock.as_ref(), &[44; 32], "525555555555@c.us").await.unwrap();
    let third_code = crate::modules::doctor_number::store::load(&db, clock.as_ref(), &[44; 32]).await.unwrap().code.unwrap();
    let third_fp = db.conn.query_one(sql("SELECT fingerprint FROM doctor_number_challenges WHERE state='active'", vec![])).await.unwrap().unwrap().try_get::<Vec<u8>>("", "fingerprint").unwrap();
    assert!(third_fp == original_fingerprint(&third_code));
    // Lifetime history (3 challenges) retains original fingerprints.
    assert_eq!(count(&db, "doctor_number_challenges").await, 3);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn doctor_rotation_failures_leave_everything_unchanged() {
  with_database(|db| async move {
    let (cfg, clock, _app, _totp) = fixture(&db).await;
    crate::modules::doctor_number::store::issue(&db, clock.as_ref(), &[42; 32], "525555555555@c.us").await.unwrap();
    let doctor_before = snapshot(&db, DOCTOR_SNAPSHOT).await;
    let state_before = snapshot(&db, STATE_SNAPSHOT).await;
    let challenges_before = snapshot(&db, CHALLENGES_SNAPSHOT).await;

    // Wrong current key: no write.
    let wrong = with_key([1; 32]);
    assert!(maintenance::rotate_key(&db, &wrong, &[43; 32]).await.is_err());
    assert_eq!(snapshot(&db, DOCTOR_SNAPSHOT).await, doctor_before);
    assert_eq!(snapshot(&db, STATE_SNAPSHOT).await, state_before);
    assert_eq!(snapshot(&db, CHALLENGES_SNAPSHOT).await, challenges_before);

    // New key equal to old or to the Google key is rejected before writes.
    assert!(maintenance::rotate_key(&db, &cfg, &[42; 32]).await.is_err());
    assert_eq!(snapshot(&db, CHALLENGES_SNAPSHOT).await, challenges_before);

    // Late failure (Doctor-row update) rolls back envelope + retirement together.
    db.conn.execute_unprepared("CREATE FUNCTION deny_doctor_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic late failure'; END $$; CREATE TRIGGER deny_doctor_update BEFORE UPDATE ON doctor FOR EACH ROW EXECUTE FUNCTION deny_doctor_update()").await.unwrap();
    assert!(maintenance::rotate_key(&db, &cfg, &[43; 32]).await.is_err());
    db.conn.execute_unprepared("DROP TRIGGER deny_doctor_update ON doctor").await.unwrap();
    assert_eq!(snapshot(&db, DOCTOR_SNAPSHOT).await, doctor_before);
    assert_eq!(snapshot(&db, STATE_SNAPSHOT).await, state_before);
    assert_eq!(snapshot(&db, CHALLENGES_SNAPSHOT).await, challenges_before);

    // Successful rotation, then corrupted envelope: rotation and issuance fail closed.
    let cfg1 = with_key([43; 32]);
    assert_eq!(maintenance::rotate_key(&db, &cfg, &[43; 32]).await.unwrap(), 1);
    db.conn.execute_unprepared("UPDATE doctor_number_state SET fingerprint_key_ciphertext=set_byte(fingerprint_key_ciphertext,0,get_byte(fingerprint_key_ciphertext,0)#1)").await.unwrap();
    let doctor_after = snapshot(&db, DOCTOR_SNAPSHOT).await;
    let state_after = snapshot(&db, STATE_SNAPSHOT).await;
    let challenges_after = snapshot(&db, CHALLENGES_SNAPSHOT).await;
    assert!(maintenance::rotate_key(&db, &cfg1, &[44; 32]).await.is_err());
    clock.advance(61);
    assert!(crate::modules::doctor_number::store::issue(&db, clock.as_ref(), &[43; 32], "525555555555@c.us").await.is_err());
    assert_eq!(snapshot(&db, DOCTOR_SNAPSHOT).await, doctor_after);
    assert_eq!(snapshot(&db, STATE_SNAPSHOT).await, state_after);
    assert_eq!(snapshot(&db, CHALLENGES_SNAPSHOT).await, challenges_after);
  })
  .await;
}
