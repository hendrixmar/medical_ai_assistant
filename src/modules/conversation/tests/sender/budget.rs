use super::*;
use crate::modules::doctor::tests::{authenticated_get, field, post, signed_in, text};

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn twenty_rolling_hour_commitments_include_failed_sends_and_doctor_notices_across_restart() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, client, totp) = outgoing(&db, &p, true).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    p.wire.send_mode.store(3, Ordering::SeqCst);
    for n in 0..20 {
      enqueue(&db, &app, clock.as_ref(), &cfg, &format!("BUDGET{n}")).await;
      assert_eq!(
        sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
          .await
          .unwrap(),
        sender::Outcome::Typing
      );
      clock.advance(6);
      assert_eq!(
        sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
          .await
          .unwrap(),
        sender::Outcome::Dispatched
      );
    }
    assert_eq!(p.count("/api/sendText"), 20);
    let csrf = field(
      &text(authenticated_get(&app, "/dashboard", &session).await).await,
      "csrf_token",
    );
    let id: uuid::Uuid = db
      .conn
      .query_one(sql("SELECT id FROM followup_requests"))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "id")
      .unwrap();
    assert_eq!(
      post(
        &app,
        &format!("/followups/{id}/resolve"),
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", &csrf)]
      )
      .await
      .status(),
      303
    );
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Idle
    );
    assert_eq!(p.count("/api/default/new-message-id"), 20);
    let row = db
      .conn
      .query_one(sql(
        "SELECT eligible_at FROM outbox_messages WHERE resolved_followup_id IS NOT NULL",
      ))
      .await
      .unwrap()
      .unwrap();
    let due: i64 = row.try_get("", "eligible_at").unwrap();
    assert_eq!(due, (NOW + 30 + 6 + 3600) * 1000);
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE state='queued' AND rate_deferred_until IS NOT NULL"
      )
      .await,
      1
    );
    let fresh = crate::app::tests::reconnect(&db, 2).await;
    assert_eq!(
      sender::run_once(&fresh, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Idle
    );
    clock.advance((due - clock.now_millis()) / 1000 - 1);
    assert_eq!(
      sender::run_once(&fresh, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Idle
    );
    clock.advance(1);
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
    assert_eq!(p.count("/api/sendText"), 21);
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE dispatch_committed_at IS NOT NULL"
      )
      .await,
      21
    );
    fresh.conn.close().await.unwrap();
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn missing_prerequisites_and_disabled_between_phases_preserve_pending_work() {
  with_database(|db|async move{
    let p=Provider::new().await;let (cfg,clock,app,client,_)=outgoing(&db,&p,true).await;
    enqueue(&db,&app,clock.as_ref(),&cfg,"DISABLE").await;
    sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap();clock.advance(6);
    drop(app);let mut cfg=cfg;Arc::get_mut(&mut cfg).unwrap().waha_outbound_enabled=false;
    let before=p.wire.calls.lock().unwrap().len();
    assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Idle);
    Arc::get_mut(&mut cfg).unwrap().waha_outbound_enabled=true;Arc::get_mut(&mut cfg).unwrap().public_origin=None;
    assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Idle);
    Arc::get_mut(&mut cfg).unwrap().public_origin=Some("https://practice.test".into());Arc::get_mut(&mut cfg).unwrap().waha=None;
    assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Idle);
    assert_eq!(p.wire.calls.lock().unwrap().len(),before);
    assert_eq!(count(&db,"outbox_messages WHERE state='typing' AND preflight_failures=0 AND dispatch_committed_at IS NULL").await,1);
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn invalid_seen_context_and_identity_conflict_cause_zero_provider_calls() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &p, true).await;
    let mut wrong = message("WRONGPEER", "5");
    wrong["payload"]["id"] = json!("false_15550100999@c.us_WRONGPEER");
    assert_eq!(deliver(&app, &wrong).await.status(), 200);
    preparation::run_once(&db, clock.as_ref()).await.unwrap();
    jobs::run_once(&db, clock.as_ref(), &cfg, None)
      .await
      .unwrap();
    sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert!(p.wire.calls.lock().unwrap().is_empty());
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE state='failed' AND last_error_code='invalid_incoming_id'"
      )
      .await,
      1
    );
    enqueue(&db, &app, clock.as_ref(), &cfg, "CONFLICTBLOCK").await;
    db.conn
      .execute_unprepared("UPDATE contacts SET identity_conflict=true")
      .await
      .unwrap();
    sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert!(p.wire.calls.lock().unwrap().is_empty());
  })
  .await;
}
