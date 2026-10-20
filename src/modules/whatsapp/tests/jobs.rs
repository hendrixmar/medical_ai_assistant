use super::intake::*;
use crate::{
  app::tests::with_database,
  modules::{
    doctor::Clock,
    whatsapp::jobs::{run_once, Outcome},
  },
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn invalid_stored_input_is_retained_as_terminal_failure() {
  with_database(|db| async move {
    let (app, clock) = setup(&db).await;
    assert_eq!(
      deliver(&app, &envelope("invalid-stored")).await.status(),
      200
    );
    db.conn
      .execute_unprepared("UPDATE inbox_messages SET raw_envelope=convert_to('{}','UTF8')")
      .await
      .unwrap();
    assert_eq!(
      run_once(&db, clock.as_ref()).await.unwrap(),
      Outcome::Failed("invalid-stored".into())
    );
    let row = db
      .conn
      .query_one(sql("SELECT * FROM processing_jobs"))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      row.try_get::<String>("", "last_error_code").unwrap(),
      "invalid_stored_message"
    );
    assert_eq!(row.try_get::<i32>("", "attempts").unwrap(), 1);
    assert_eq!(count(&db, "inbox_messages").await, 1);
    assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::Idle);
  })
  .await;
}

async fn install_gate(db: &crate::database::Db) {
  db.conn.execute_unprepared("CREATE TABLE preparation_audit (id text PRIMARY KEY); CREATE FUNCTION gate_success() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.status='succeeded' THEN IF NEW.inbox_message_id='a' THEN PERFORM pg_advisory_xact_lock(hashtext(current_schema()), 6); END IF; INSERT INTO preparation_audit VALUES (NEW.inbox_message_id); END IF; RETURN NEW; END $$; CREATE TRIGGER gate_success BEFORE UPDATE ON processing_jobs FOR EACH ROW EXECUTE FUNCTION gate_success();").await.unwrap();
}
async fn wait_at_gate(db: &crate::database::Db) {
  tokio::time::timeout(std::time::Duration::from_secs(1), async {
    loop {
      let row = db.conn.query_one(sql("SELECT EXISTS (SELECT FROM pg_locks WHERE locktype='advisory' AND NOT granted AND objid=6 AND classid::bigint = (hashtext(current_schema())::bigint & 4294967295)) AS waiting")).await.unwrap().unwrap();
      if row.try_get::<bool>("", "waiting").unwrap() { break; }
      tokio::task::yield_now().await;
    }
  }).await.expect("Worker must reach real PostgreSQL advisory gate");
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn concurrent_workers_skip_locked_and_commit_each_effect_once() {
  crate::app::tests::with_database_connections(4, |db| async move {
    let (app, clock) = setup(&db).await;
    install_gate(&db).await;
    for id in ["a", "b"] {
      assert_eq!(deliver(&app, &envelope(id)).await.status(), 200);
    }
    let gate = db.conn.begin().await.unwrap();
    gate
      .execute(sql(
        "SELECT pg_advisory_xact_lock(hashtext(current_schema()), 6)",
      ))
      .await
      .unwrap();
    let worker = tokio::spawn({
      let db = db.clone();
      let clock = clock.clone();
      async move { run_once(&db, clock.as_ref()).await }
    });
    wait_at_gate(&db).await;
    assert_eq!(
      run_once(&db, clock.as_ref()).await.unwrap(),
      Outcome::Prepared("b".into())
    );
    assert_eq!(count(&db, "preparation_audit").await, 1);
    gate.commit().await.unwrap();
    assert_eq!(
      worker.await.unwrap().unwrap(),
      Outcome::Prepared("a".into())
    );
    assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::Idle);
    assert_eq!(count(&db, "preparation_audit").await, 2);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn rollback_to_savepoint_retains_claim_until_retry_diagnostics_commit() {
  crate::app::tests::with_database_connections(4, |db| async move {
    let (app,clock)=setup(&db).await;
    assert_eq!(deliver(&app,&envelope("a")).await.status(),200);
    db.conn.execute_unprepared("CREATE FUNCTION gate_retry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.status='succeeded' THEN RAISE EXCEPTION 'synthetic final failure'; END IF; IF NEW.attempts>0 THEN PERFORM pg_advisory_xact_lock(hashtext(current_schema()), 6); END IF; RETURN NEW; END $$; CREATE TRIGGER gate_retry BEFORE UPDATE ON processing_jobs FOR EACH ROW EXECUTE FUNCTION gate_retry();").await.unwrap();
    let gate=db.conn.begin().await.unwrap();
    gate.execute(sql("SELECT pg_advisory_xact_lock(hashtext(current_schema()), 6)")).await.unwrap();
    let worker=tokio::spawn({let db=db.clone();let clock=clock.clone();async move {run_once(&db,clock.as_ref()).await}});
    wait_at_gate(&db).await;
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(1),run_once(&db,clock.as_ref())).await.expect("Competing worker must skip the retained claim").unwrap(),Outcome::Idle);
    gate.commit().await.unwrap();
    assert_eq!(worker.await.unwrap().unwrap(),Outcome::RetryScheduled("a".into()));
    let row=db.conn.query_one(sql("SELECT preparation_state FROM inbox_messages")).await.unwrap().unwrap();
    assert_eq!(row.try_get::<String>("","preparation_state").unwrap(),"accepted");
    let row=db.conn.query_one(sql("SELECT attempts FROM processing_jobs")).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i32>("","attempts").unwrap(),1);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn interrupted_work_rolls_back_and_new_pool_recovers_one_committed_effect() {
  crate::app::tests::with_database_connections(4, |db| async move {
    let (app, clock) = setup(&db).await;
    install_gate(&db).await;
    assert_eq!(deliver(&app, &envelope("a")).await.status(), 200);
    let gate = db.conn.begin().await.unwrap();
    gate
      .execute(sql(
        "SELECT pg_advisory_xact_lock(hashtext(current_schema()), 6)",
      ))
      .await
      .unwrap();
    let worker = tokio::spawn({
      let db = db.clone();
      let clock = clock.clone();
      async move { run_once(&db, clock.as_ref()).await }
    });
    wait_at_gate(&db).await;
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    gate.commit().await.unwrap();
    let fresh = crate::app::tests::reconnect(&db, 4).await;
    drop(app);
    db.conn.close_by_ref().await.unwrap();
    let row = fresh
      .conn
      .query_one(sql(
        "SELECT preparation_state, prepared_at FROM inbox_messages",
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      row.try_get::<String>("", "preparation_state").unwrap(),
      "accepted"
    );
    assert!(row
      .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "prepared_at")
      .unwrap()
      .is_none());
    assert_eq!(count(&fresh, "preparation_audit").await, 0);
    assert_eq!(
      run_once(&fresh, clock.as_ref()).await.unwrap(),
      Outcome::Prepared("a".into())
    );
    assert_eq!(count(&fresh, "preparation_audit").await, 1);
    fresh.conn.close().await.unwrap();
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn final_success_failure_rolls_back_canonical_but_persists_retry_and_recovers() {
  with_database(|db| async move {
    let (app, clock) = setup(&db).await;
    assert_eq!(deliver(&app, &envelope("retry")).await.status(), 200);
    db.conn.execute_unprepared("CREATE FUNCTION fail_success() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.status='succeeded' THEN RAISE EXCEPTION 'synthetic private failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_success BEFORE UPDATE ON processing_jobs FOR EACH ROW EXECUTE FUNCTION fail_success();").await.unwrap();
    assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::RetryScheduled("retry".into()));
    let row = db.conn.query_one(sql("SELECT preparation_state, body, prepared_at FROM inbox_messages")).await.unwrap().unwrap();
    assert_eq!(row.try_get::<String>("", "preparation_state").unwrap(), "accepted");
    assert!(row.try_get::<Option<String>>("", "body").unwrap().is_none());
    let row = db.conn.query_one(sql("SELECT * FROM processing_jobs")).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i32>("", "attempts").unwrap(), 1);
    assert_eq!(row.try_get::<String>("", "last_error_code").unwrap(), "preparation_database_error");
    assert_eq!(row.try_get::<chrono::DateTime<chrono::Utc>>("", "available_at").unwrap().timestamp(), clock.now()+5);
    let fresh = crate::app::tests::reconnect(&db, 2).await;
    assert_eq!(run_once(&fresh, clock.as_ref()).await.unwrap(), Outcome::Idle);
    clock.advance(4); assert_eq!(run_once(&fresh, clock.as_ref()).await.unwrap(), Outcome::Idle);
    fresh.conn.execute_unprepared("DROP TRIGGER fail_success ON processing_jobs").await.unwrap();
    clock.advance(1); assert_eq!(run_once(&fresh, clock.as_ref()).await.unwrap(), Outcome::Prepared("retry".into()));
    fresh.conn.close().await.unwrap();
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn persistent_failure_has_bounded_retries_and_diagnostic_commit_failure_stays_pending() {
  with_database(|db| async move {
    let (app, clock) = setup(&db).await;
    assert_eq!(deliver(&app, &envelope("failure")).await.status(), 200);
    db.conn.execute_unprepared("CREATE FUNCTION fail_any_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic private failure'; END $$; CREATE TRIGGER fail_any BEFORE UPDATE ON processing_jobs FOR EACH ROW EXECUTE FUNCTION fail_any_update();").await.unwrap();
    assert!(run_once(&db, clock.as_ref()).await.is_err());
    let row=db.conn.query_one(sql("SELECT attempts,status FROM processing_jobs")).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i32>("", "attempts").unwrap(), 0);
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "pending");
    db.conn.execute_unprepared("DROP TRIGGER fail_any ON processing_jobs; CREATE TRIGGER fail_any BEFORE UPDATE ON processing_jobs FOR EACH ROW WHEN (NEW.status='succeeded') EXECUTE FUNCTION fail_any_update();").await.unwrap();
    for delay in [5,10,20,40] {
      assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::RetryScheduled("failure".into()));
      assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::Idle);
      clock.advance(delay);
    }
    assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::Failed("failure".into()));
    clock.advance(1000); assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::Idle);
    let row=db.conn.query_one(sql("SELECT * FROM processing_jobs")).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i32>("", "attempts").unwrap(), 5);
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "failed");
    assert!(row.try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at").unwrap().is_some());
    assert_eq!(count(&db, "inbox_messages").await, 1);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn prepare_text_media_and_other_without_conversation_effects() {
  with_database(|db| async move {
    let (app, clock) = setup(&db).await;
    let mut media = envelope("media");
    media["payload"]["hasMedia"] = json!(true);
    media["payload"]["media"] = json!({"url":"https://invalid.test/not-fetched", "mimetype":"application/pdf", "filename":"receipt.pdf", "error":{"secret":"discard projection"}});
    let mut other = envelope("other"); other["payload"]["body"] = Value::Null;
    other["payload"]["_data"] = json!({"Message":{"unfamiliarContent":{"preserve":"raw"}}});
    for message in [envelope("text"), media, other] { assert_eq!(deliver(&app, &message).await.status(), 200); }
    for id in ["media", "other", "text"] {
      assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::Prepared(id.into()));
    }
    assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::Idle);
    let mut duplicate=envelope("text"); duplicate["payload"]["body"]=json!("must not overwrite preparation");
    assert_eq!(deliver(&app,&duplicate).await.status(),200);
    assert_eq!(run_once(&db, clock.as_ref()).await.unwrap(), Outcome::Idle);
    let rows = db.conn.query_all(sql("SELECT * FROM inbox_messages ORDER BY message_id")).await.unwrap();
    for (row, kind) in rows.iter().zip(["media", "other", "text"]) {
      assert_eq!(row.try_get::<String>("", "preparation_state").unwrap(), "ready_for_conversation");
      assert_eq!(row.try_get::<String>("", "message_kind").unwrap(), kind);
      assert_eq!(row.try_get::<chrono::DateTime<chrono::Utc>>("", "prepared_at").unwrap().timestamp(), clock.now());
    }
    assert_eq!(rows[2].try_get::<String>("", "body").unwrap(), " Hola, cita sintética \n");
    assert_eq!(rows[0].try_get::<String>("", "body").unwrap(), " Hola, cita sintética \n");
    assert!(rows[1].try_get::<Option<String>>("", "body").unwrap().is_none());
    let raw:Value=serde_json::from_slice(&rows[1].try_get::<Vec<u8>>("", "raw_envelope").unwrap()).unwrap();
    assert_eq!(raw["payload"]["_data"]["Message"]["unfamiliarContent"]["preserve"],"raw");
    assert_eq!(rows[0].try_get::<Value>("", "media_metadata").unwrap(), json!({"url":"https://invalid.test/not-fetched", "mimetype":"application/pdf", "filename":"receipt.pdf"}));
    assert_eq!(db.conn.query_one(sql("SELECT count(*) AS count FROM processing_jobs WHERE status='succeeded' AND attempts=1")).await.unwrap().unwrap().try_get::<i64>("", "count").unwrap(), 3);
  }).await;
}
