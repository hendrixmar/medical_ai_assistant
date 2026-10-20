use super::*;
use sea_orm::TransactionTrait;

async fn wait_gate(db: &crate::database::Db) {
  tokio::time::timeout(std::time::Duration::from_secs(2),async{loop{
    let row=db.conn.query_one(sql("SELECT EXISTS (SELECT FROM pg_locks WHERE locktype='advisory' AND NOT granted AND objid=7 AND classid::bigint=(hashtext(current_schema())::bigint & 4294967295)) AS waiting")).await.unwrap().unwrap();
    if row.try_get::<bool>("","waiting").unwrap(){break;}tokio::task::yield_now().await;
  }}).await.expect("Sender must reach actual PostgreSQL gate");
}
fn spawn(
  db: &crate::database::Db,
  clock: &Arc<crate::modules::doctor::tests::TestClock>,
  cfg: &crate::common::config::Config,
  client: &Client,
) -> tokio::task::JoinHandle<Result<sender::Outcome, sender::Unavailable>> {
  let (db, clock, cfg, client) = (db.clone(), clock.clone(), cfg.clone(), client.clone());
  tokio::spawn(async move { sender::run_once(&db, clock.as_ref(), &cfg, Some(&client)).await })
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn two_workers_and_stale_lease_cannot_commit_typing_or_a_second_post() {
  crate::app::tests::with_database_connections(4, |db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &p, true).await;
    enqueue(&db, &app, clock.as_ref(), &cfg, "STALE").await;
    *p.wire.block_path.lock().unwrap() = Some("/api/startTyping".into());
    let first = spawn(&db, &clock, &cfg, &client);
    p.wire.entered.notified().await;
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Idle
    );
    clock.advance(31);
    *p.wire.block_path.lock().unwrap() = None;
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Typing
    );
    p.wire.release.notify_one();
    assert_eq!(first.await.unwrap().unwrap(), sender::Outcome::Idle);
    clock.advance(6);
    let (a, b) = tokio::join!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client)),
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    assert_eq!(
      outcomes
        .iter()
        .filter(|o| **o == sender::Outcome::Dispatched)
        .count(),
      1
    );
    assert_eq!(p.count("/api/sendText"), 1);
    assert_eq!(p.count("/api/default/new-message-id"), 1);
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE dispatch_committed_at IS NOT NULL"
      )
      .await,
      1
    );
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn stale_generator_error_preserves_replacement_claim_and_preflight_state() {
  crate::app::tests::with_database_connections(4, |db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &p, true).await;
    let eligible_at = clock.now_millis();
    enqueue(&db, &app, clock.as_ref(), &cfg, "STALEERROR").await;
    *p.wire.block_path.lock().unwrap() = Some("/api/default/new-message-id".into());
    let old = spawn(&db, &clock, &cfg, &client);
    tokio::time::timeout(std::time::Duration::from_secs(2), p.wire.entered.notified())
      .await.expect("Old worker must block in the ID generator");
    let old_claim = db.conn.query_one(sql("SELECT claim_token,claim_until FROM contacts")).await.unwrap().unwrap();
    let old_token: uuid::Uuid = old_claim.try_get("", "claim_token").unwrap();
    let old_until: i64 = old_claim.try_get("", "claim_until").unwrap();
    assert_eq!(p.count("/api/default/new-message-id"), 1);
    assert_eq!(count(&db, "outbox_messages WHERE raw_provider_id IS NULL").await, 1);

    clock.advance(31);
    *p.wire.block_path.lock().unwrap() = Some("/api/startTyping".into());
    let replacement = spawn(&db, &clock, &cfg, &client);
    tokio::time::timeout(std::time::Duration::from_secs(2), p.wire.entered.notified())
      .await.expect("Replacement must save its ID and block in startTyping");
    let snapshot_sql = "SELECT row_to_json(snapshot)::text AS snapshot FROM (
      SELECT o.state,o.preflight_failures,o.eligible_at,o.raw_provider_id,
        o.typing_due_at,o.last_error_code,c.claim_token,c.claim_until,
        j.status AS job_status,j.attempts AS job_attempts,j.last_error_code AS job_error
      FROM outbox_messages o JOIN contacts c ON c.id=o.contact_id
      JOIN processing_jobs j ON j.outbox_message_id=o.id AND j.kind='send_outbox'
    ) snapshot";
    let before: String = db.conn.query_one(sql(snapshot_sql)).await.unwrap().unwrap().try_get("", "snapshot").unwrap();
    let saved: Value = serde_json::from_str(&before).unwrap();
    assert_eq!(saved["state"], "queued");
    assert_eq!(saved["preflight_failures"], 0);
    assert_eq!(saved["eligible_at"], json!(eligible_at));
    assert_eq!(saved["raw_provider_id"], "OUT0");
    let replacement_token: uuid::Uuid = saved["claim_token"].as_str().unwrap().parse().unwrap();
    assert_ne!(replacement_token, old_token);
    assert!(saved["claim_until"].as_i64().unwrap() > old_until);
    assert_eq!(saved["claim_until"], json!(clock.now_millis() + 30_000));
    assert_eq!(p.count("/api/default/new-message-id"), 2);
    assert_eq!(p.count("/api/startTyping"), 1);

    // Notify wakes waiters FIFO: the old generator entered before replacement typing.
    // Keep typing blocked until the old 503 has traversed the production failure fence.
    p.wire.fail_preflight.store(1, Ordering::SeqCst);
    p.wire.release.notify_one();
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(2), old).await.unwrap().unwrap().unwrap(), sender::Outcome::RetryScheduled);
    assert!(!replacement.is_finished());
    assert_eq!(db.conn.query_one(sql(snapshot_sql)).await.unwrap().unwrap().try_get::<String>("", "snapshot").unwrap(), before);
    assert_eq!(p.count("/api/sendText"), 0);

    p.wire.fail_preflight.store(0, Ordering::SeqCst);
    *p.wire.block_path.lock().unwrap() = None;
    p.wire.release.notify_one();
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(2), replacement).await.unwrap().unwrap().unwrap(), sender::Outcome::Typing);
    clock.advance(6);
    assert_eq!(sender::run_once(&db, clock.as_ref(), &cfg, Some(&client)).await.unwrap(), sender::Outcome::Dispatched);
    assert_eq!(sender::run_once(&db, clock.as_ref(), &cfg, Some(&client)).await.unwrap(), sender::Outcome::Idle);
    assert_eq!(p.count("/api/sendText"), 1);
    assert_eq!(count(&db, "outbox_messages WHERE state='sent' AND raw_provider_id='OUT0' AND preflight_failures=0 AND dispatch_committed_at IS NOT NULL").await, 1);
    assert_eq!(count(&db, "processing_jobs WHERE kind='send_outbox' AND status='succeeded' AND attempts=0 AND last_error_code IS NULL").await, 1);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn cancellation_before_typing_and_wire_commit_rolls_back_and_recovers() {
  crate::app::tests::with_database_connections(4,|db|async move{
    let p=Provider::new().await;let (cfg,clock,app,client,_)=outgoing(&db,&p,true).await;
    enqueue(&db,&app,clock.as_ref(),&cfg,"CANCEL").await;
    for phase in ["typing","uncertain"] {
      db.conn.execute_unprepared(&format!("CREATE FUNCTION gate_phase() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='{phase}' THEN PERFORM pg_advisory_xact_lock(hashtext(current_schema()),7); END IF; RETURN NEW; END $$; CREATE TRIGGER gate_phase BEFORE UPDATE ON outbox_messages FOR EACH ROW EXECUTE FUNCTION gate_phase()" )).await.unwrap();
      let gate=db.conn.begin().await.unwrap();gate.execute(sql("SELECT pg_advisory_xact_lock(hashtext(current_schema()),7)")).await.unwrap();
      let worker=spawn(&db,&clock,&cfg,&client);wait_gate(&db).await;
      worker.abort();assert!(worker.await.unwrap_err().is_cancelled());gate.commit().await.unwrap();
      assert_eq!(p.count("/api/sendText"),0);
      assert_eq!(count(&db,"outbox_messages WHERE dispatch_committed_at IS NOT NULL").await,0);
      db.conn.execute_unprepared("DROP TRIGGER gate_phase ON outbox_messages; DROP FUNCTION gate_phase()").await.unwrap();
      clock.advance(31);
      if phase=="typing" {
        assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Typing);clock.advance(6);
      }
    }
    assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Dispatched);assert_eq!(p.count("/api/sendText"),1);
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn stale_stopped_typing_worker_is_fenced_before_wire_commit() {
  crate::app::tests::with_database_connections(4, |db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &p, true).await;
    enqueue(&db, &app, clock.as_ref(), &cfg, "CAS").await;
    sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    clock.advance(6);
    *p.wire.block_path.lock().unwrap() = Some("/api/stopTyping".into());
    let first = spawn(&db, &clock, &cfg, &client);
    p.wire.entered.notified().await;
    clock.advance(31);
    *p.wire.block_path.lock().unwrap() = None;
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Dispatched
    );
    p.wire.release.notify_one();
    assert_eq!(first.await.unwrap().unwrap(), sender::Outcome::Idle);
    assert_eq!(p.count("/api/sendText"), 1);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn aborted_committed_send_never_reposts_and_slow_send_does_not_block_intake_or_dashboard() {
  crate::app::tests::with_database_connections(4, |db| async move {
    use crate::modules::doctor::tests::{authenticated_get, signed_in};
    let p = Provider::new().await;
    let (cfg, clock, app, client, totp) = outgoing(&db, &p, true).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    enqueue(&db, &app, clock.as_ref(), &cfg, "ABORTED").await;
    sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    clock.advance(6);
    p.wire.send_mode.store(2, Ordering::SeqCst);
    let worker = spawn(&db, &clock, &cfg, &client);
    p.wire.entered.notified().await;
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE state='uncertain' AND dispatch_committed_at IS NOT NULL"
      )
      .await,
      1
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
      assert_eq!(
        authenticated_get(&app, "/dashboard", &session)
          .await
          .status(),
        200
      );
      enqueue(&db, &app, clock.as_ref(), &cfg, "DISTINCT").await;
    })
    .await
    .expect("Database and Doctor routes must not wait for send HTTP");
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Idle
    );
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    clock.advance(126);
    p.wire.send_mode.store(0, Ordering::SeqCst);
    p.wire.release.notify_one();
    let fresh = crate::app::tests::reconnect(&db, 4).await;
    assert_eq!(
      sender::run_once(&fresh, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Typing
    );
    clock.advance(6);
    sender::run_once(&fresh, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert_eq!(
      count(&db, "outbox_messages WHERE state='uncertain'").await,
      1
    );
    assert_eq!(count(&db, "outbox_messages WHERE state='sent'").await, 1);
    let bodies: Vec<_> = p
      .wire
      .calls
      .lock()
      .unwrap()
      .iter()
      .filter(|(path, _)| path == "/api/sendText")
      .map(|(_, v)| v["id"].clone())
      .collect();
    assert_eq!(bodies, vec![json!("OUT0"), json!("OUT1")]);
    fresh.conn.close().await.unwrap();
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn failed_deferred_database_commit_never_dispatches() {
  with_database(|db|async move{
    let p=Provider::new().await;let (cfg,clock,app,client,_)=outgoing(&db,&p,true).await;
    enqueue(&db,&app,clock.as_ref(),&cfg,"COMMITFAIL").await;
    sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap();clock.advance(6);
    db.conn.execute_unprepared("CREATE FUNCTION reject_wire_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='uncertain' THEN RAISE EXCEPTION 'synthetic commit failure'; END IF; RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER reject_wire_commit AFTER UPDATE ON outbox_messages DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_wire_commit()").await.unwrap();
    assert!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.is_err());
    assert_eq!(p.count("/api/sendText"),0);assert_eq!(count(&db,"outbox_messages WHERE dispatch_committed_at IS NOT NULL").await,0);
    db.conn.execute_unprepared("DROP TRIGGER reject_wire_commit ON outbox_messages").await.unwrap();clock.advance(31);
    sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap();assert_eq!(p.count("/api/sendText"),1);
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn preflight_backoff_is_bounded_and_terminal_retained_without_text() {
  with_database(|db|async move{
    let p=Provider::new().await;let (cfg,clock,app,client,_)=outgoing(&db,&p,true).await;
    enqueue(&db,&app,clock.as_ref(),&cfg,"RETRY").await;p.wire.fail_preflight.store(1,Ordering::SeqCst);
    for delay in [5,10,20,40] {
      assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::RetryScheduled);
      assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Idle);clock.advance(delay);
    }
    sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap();clock.advance(1000);
    assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Idle);
    assert_eq!(count(&db,"outbox_messages WHERE state='failed' AND preflight_failures=5 AND dispatch_committed_at IS NULL").await,1);
    assert_eq!(count(&db,"processing_jobs WHERE kind='send_outbox' AND status='failed' AND attempts=5").await,1);
    assert_eq!(p.count("/api/default/new-message-id"),5);assert_eq!(p.count("/api/sendText"),0);
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn unknown_http_failure_stays_uncertain_and_known_rejection_is_not_retried() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &p, true).await;
    for (mode, name) in [(1, "UNKNOWN"), (3, "REJECTED")] {
      enqueue(&db, &app, clock.as_ref(), &cfg, name).await;
      p.wire.send_mode.store(mode, Ordering::SeqCst);
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap();
      clock.advance(6);
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap();
      clock.advance(126);
      for _ in 0..3 {
        assert_eq!(
          sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
            .await
            .unwrap(),
          sender::Outcome::Idle
        );
      }
    }
    assert_eq!(p.count("/api/sendText"), 2);
    assert_eq!(
      count(&db, "outbox_messages WHERE state='uncertain'").await,
      1
    );
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE state='failed' AND last_error_code='provider_rejected'"
      )
      .await,
      1
    );
    assert_eq!(
      count(&db, "contacts WHERE privacy_first_sent_at IS NOT NULL").await,
      0
    );
  })
  .await;
}
