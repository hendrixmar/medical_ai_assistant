use super::*;
use crate::modules::conversation::store::{enqueue_text, Message};
use sea_orm::TransactionTrait;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn takeover_cancels_conversation_head_without_blocking_essential_status() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &provider, true).await;
    enqueue(&db, &app, clock.as_ref(), &cfg, "TAKEOVER").await;
    let contact: uuid::Uuid = db
      .conn
      .query_one(sql("SELECT id FROM contacts"))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "id")
      .unwrap();
    let tx = db.conn.begin().await.unwrap();
    tx.execute(crate::modules::whatsapp::inbox::sql(
      "UPDATE contacts SET takeover_until=$2 WHERE id=$1",
      vec![contact.into(), (clock.now_millis() + 43_200_000).into()],
    ))
    .await
    .unwrap();
    enqueue_text(
      &tx,
      Message {
        contact,
        appointment: None,
        key: "takeover-essential-status",
        text: "Su solicitud sigue registrada.",
        purpose: "status",
        revision: None,
      },
      cfg.public_origin.as_deref().unwrap(),
      clock.now_millis(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Idle
    );
    assert_eq!(provider.count("/api/sendText"), 0);
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
    assert_eq!(provider.count("/api/sendText"), 1);
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE dedupe_key='takeover-essential-status' AND state='sent'"
      )
      .await,
      1
    );
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE purpose='conversation' AND state='cancelled'"
      )
      .await,
      1
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn text_deleted_after_typing_is_never_dispatched_from_a_cached_claim() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &provider, true).await;
    enqueue(&db, &app, clock.as_ref(), &cfg, "REDACT").await;
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Typing
    );
    db.conn
      .execute(crate::modules::whatsapp::inbox::sql(
        "UPDATE outbox_messages SET text_deleted_at=$1,rendered_text=''",
        vec![clock.now_millis().into()],
      ))
      .await
      .unwrap();
    clock.advance(6);
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Idle
    );
    assert_eq!(provider.count("/api/sendText"), 0);
    assert_eq!(
      count(&db, "outbox_messages WHERE state='cancelled'").await,
      1
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn doctor_binding_is_serialized_with_dispatch_fence_and_removed_destination_is_not_sent() {
  with_database(|db| async move {
    let provider = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &provider, true).await;
    enqueue(&db, &app, clock.as_ref(), &cfg, "DOCTORNOTICE").await;
    let row = db.conn.query_one(sql(
      "SELECT contact_id,wrote_first_inbox_id,original_target FROM outbox_messages"
    )).await.unwrap().unwrap();
    let contact: uuid::Uuid = row.try_get("", "contact_id").unwrap();
    let incoming: String = row.try_get("", "wrote_first_inbox_id").unwrap();
    let target: String = row.try_get("", "original_target").unwrap();
    let tx = db.conn.begin().await.unwrap();
    tx.execute(crate::modules::whatsapp::inbox::sql(
      "UPDATE doctor_number_state SET verified_phone=$1,original_target=$1,contact_id=$2,inbox_message_id=$3,verified_at=$4,practice_phone='15550100999@c.us' WHERE id=1",
      vec![target.into(), contact.into(), incoming.into(), clock.now_millis().into()],
    )).await.unwrap();
    tx.execute(sql("UPDATE outbox_messages SET state='cancelled'")).await.unwrap();
    enqueue_text(&tx, Message {
      contact,
      appointment: None,
      key: "private-doctor-notice",
      text: "Tiene una solicitud pendiente de revisión.",
      purpose: "doctor_notification",
      revision: None,
    }, cfg.public_origin.as_deref().unwrap(), clock.now_millis()).await.unwrap();
    tx.commit().await.unwrap();
    let claim = crate::modules::conversation::dispatch::claim(&db, clock.as_ref(), &cfg)
      .await.unwrap().unwrap();
    let fence = crate::modules::conversation::dispatch::fence(&db, clock.as_ref(), &claim)
      .await.unwrap().unwrap();
    let replacement = db.conn.begin().await.unwrap();
    replacement.execute_unprepared("SET LOCAL lock_timeout='50ms'").await.unwrap();
    let changed = replacement.execute(sql(
      "UPDATE doctor_number_state SET verified_phone=NULL,original_target=NULL,contact_id=NULL,inbox_message_id=NULL,verified_at=NULL,practice_phone=NULL WHERE id=1"
    )).await;
    assert!(changed.is_err(), "Doctor authority cannot change after its dispatch fence and before commitment");
    replacement.rollback().await.unwrap();
    fence.rollback().await.unwrap();
    db.conn.execute(sql(
      "UPDATE doctor_number_state SET verified_phone=NULL,original_target=NULL,contact_id=NULL,inbox_message_id=NULL,verified_at=NULL,practice_phone=NULL WHERE id=1"
    )).await.unwrap();
    clock.advance(31);
    assert_eq!(sender::run_once(&db, clock.as_ref(), &cfg, Some(&client)).await.unwrap(), sender::Outcome::Idle);
    assert_eq!(provider.count("/api/sendText"), 0);
    assert_eq!(count(&db, "outbox_messages WHERE dedupe_key='private-doctor-notice' AND state='cancelled'").await, 1);
  }).await;
}
