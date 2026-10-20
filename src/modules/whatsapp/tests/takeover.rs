use super::super::{config::WahaConfig, inbox::sql, takeover};
use crate::{
  app::{router_with_clock, tests::with_database},
  modules::{
    conversation::identity,
    doctor::{tests::fixture, Clock},
  },
};
use axum::{body::Body, http::Request, Router};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

const API_KEY: &str = "takeover-synthetic-api-key";
const HMAC_KEY: &str = "takeover-synthetic-webhook-key-with-32-bytes";
const TAKEOVER_MS: i64 = 12 * 60 * 60 * 1000;

async fn setup(
  db: &crate::database::Db,
  provider: &super::recovery::Provider,
  enabled: bool,
) -> (Arc<crate::modules::doctor::tests::TestClock>, Router) {
  let (mut cfg, clock, app, _) = fixture(db).await;
  drop(app);
  Arc::get_mut(&mut cfg).unwrap().waha =
    WahaConfig::parse(Some(&provider.origin), Some(API_KEY), Some(HMAC_KEY)).unwrap();
  Arc::get_mut(&mut cfg).unwrap().waha_takeover_enabled = enabled;
  let app = router_with_clock(cfg.clone(), db.clone(), clock.clone());
  (clock, app)
}

async fn seed_contact(db: &crate::database::Db, chat: &str, now: i64) -> Uuid {
  let tx = db.conn.begin().await.unwrap();
  let id = format!("seed-{chat}");
  tx.execute(sql(
    "INSERT INTO inbox_messages(message_id,original_chat_identity,resolved_phone_identity,server_timestamp,arrived_at,raw_envelope) VALUES ($1,$2,$2,$3,to_timestamp($4::double precision/1000),$5)",
    vec![id.clone().into(), chat.into(), (now.div_euclid(1000)-1).into(), now.into(), b"{}".to_vec().into()],
  ))
  .await
  .unwrap();
  let contact = identity::assign(&tx, &id, chat, Some(chat), now)
    .await
    .unwrap();
  tx.commit().await.unwrap();
  contact
}

fn phone_event(chat: &str, raw_id: &str, timestamp: i64) -> Value {
  json!({
    "event":"message",
    "session":"default",
    "engine":"GOWS",
    "payload":{
      "id":format!("true_{chat}_{raw_id}"),
      "timestamp":timestamp,
      "from":chat,
      "to":null,
      "fromMe":true,
      "source":"app",
      "body":"El doctor confirma su horario.",
      "_data":{"Info":{
        "IsFromMe":true,
        "IsGroup":false,
        "Chat":chat,
        "ID":raw_id,
        "RecipientAlt":chat.replace("@c.us","@s.whatsapp.net")
      }}
    }
  })
}

async fn deliver(app: &Router, event: &Value) -> axum::response::Response {
  let raw = serde_json::to_vec(event).unwrap();
  let signature = super::intake::signature(&raw, HMAC_KEY);
  app
    .clone()
    .oneshot(
      Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/waha")
        .header("x-webhook-hmac", signature)
        .body(Body::from(raw))
        .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn authenticated_phone_text_is_contact_scoped_idempotent_and_ends() {
  with_database(|db| async move {
    let provider = super::recovery::Provider::new(vec![], 0).await;
    let (clock, app) = setup(&db, &provider, true).await;
    let now = clock.now_millis();
    let chat = "15550100111@c.us";
    let contact = seed_contact(&db, chat, now).await;
    let other = seed_contact(&db, "15550100112@c.us", now).await;
    let event = phone_event(chat, "phone-text-1", now.div_euclid(1000));

    assert_eq!(deliver(&app, &event).await.status(), 200);
    assert_eq!(deliver(&app, &event).await.status(), 200);
    assert!(takeover::active(&db.conn, contact, now).await.unwrap());
    assert!(!takeover::active(&db.conn, other, now).await.unwrap());
    let state = db
      .conn
      .query_one(sql(
        "SELECT takeover_until FROM contacts WHERE id=$1",
        vec![contact.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(state.try_get::<Option<i64>>("", "takeover_until").unwrap(), Some(now + TAKEOVER_MS));
    assert_eq!(count(&db, "phone_takeover_events WHERE attribution_status='attributed' AND contact_id IS NOT NULL AND body='El doctor confirma su horario.'").await, 1);

    let app_echo = json!({"event":"message","session":"default","engine":"GOWS","payload":{
      "id":"true_15550100111@c.us_app-echo","timestamp":now.div_euclid(1000),"from":chat,"to":null,"fromMe":true,"source":"api"
    }});
    assert_eq!(deliver(&app, &app_echo).await.status(), 200);
    assert_eq!(count(&db, "phone_takeover_events").await, 1);

    let ack = json!({
      "event":"message.ack",
      "session":"default",
      "engine":"GOWS",
      "id":"ack-envelope",
      "payload":{
        "fromMe":true,
        "from":chat,
        "to":null,
        "participant":null,
        "id":"true_15550100111@c.us_acknowledged",
        "ack":2
      }
    });
    assert_eq!(deliver(&app, &ack).await.status(), 200);
    assert_eq!(count(&db, "phone_takeover_events").await, 1);

    let mut wrong_chat = phone_event(chat, "wrong-chat", now.div_euclid(1000));
    wrong_chat["payload"]["_data"]["Info"]["Chat"] = json!("15550100112@c.us");
    assert_eq!(deliver(&app, &wrong_chat).await.status(), 200);
    assert_eq!(count(&db, "phone_takeover_events WHERE unavailable_reason='wrong_chat' AND contact_id IS NULL").await, 1);
    assert!(!takeover::active(&db.conn, other, now).await.unwrap());
    let malformed = json!({
      "event":"message","session":"default","engine":"GOWS",
      "payload":{"timestamp":now.div_euclid(1000),"from":chat,"to":null,"fromMe":true,"source":"app","body":"Hola"}
    });
    assert_eq!(deliver(&app, &malformed).await.status(), 200);
    assert_eq!(deliver(&app, &malformed).await.status(), 200);
    assert_eq!(count(&db, "phone_takeover_events WHERE unavailable_reason='invalid_message' AND body IS NULL").await, 1);
    assert!(takeover::active(&db.conn, contact, now).await.unwrap(), "Malformed events cannot change an already attributed takeover");

    let unknown = phone_event("15550999999@c.us", "unmapped", now.div_euclid(1000));
    assert_eq!(deliver(&app, &unknown).await.status(), 200);
    assert_eq!(count(&db, "phone_takeover_events WHERE attribution_status='unavailable' AND contact_id IS NULL AND unavailable_reason='unknown_chat' AND body IS NULL").await, 1);
    assert_eq!(count(&db, "attention_items WHERE kind='whatsapp_identity_missing' AND contact_id IS NULL AND resolved_at IS NULL").await, 1);
    assert!(!takeover::active(&db.conn, other, now).await.unwrap());

    assert!(takeover::active(&db.conn, contact, now + TAKEOVER_MS - 1).await.unwrap());
    assert!(!takeover::active(&db.conn, contact, now + TAKEOVER_MS).await.unwrap());
    let tx = db.conn.begin().await.unwrap();
    takeover::end(&tx, contact, now + 1).await.unwrap();
    tx.commit().await.unwrap();
    assert!(!takeover::active(&db.conn, contact, now + 1).await.unwrap());
    assert!(!takeover::active(&db.conn, contact, now + TAKEOVER_MS).await.unwrap());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn disabled_takeover_and_recipient_mismatch_never_assign_or_suppress() {
  with_database(|db| async move {
    let provider = super::recovery::Provider::new(vec![], 0).await;
    let (clock, app) = setup(&db, &provider, false).await;
    let now = clock.now_millis();
    let chat = "15550100121@c.us";
    let contact = seed_contact(&db, chat, now).await;

    let disabled = phone_event(chat, "disabled-phone-text", now.div_euclid(1000));
    assert_eq!(deliver(&app, &disabled).await.status(), 200);
    assert!(!takeover::active(&db.conn, contact, now).await.unwrap());
    assert_eq!(count(&db, "phone_takeover_events WHERE attribution_status='unavailable' AND contact_id IS NULL AND unavailable_reason='operator_disabled' AND body IS NULL").await, 1);

    let mut mismatch = phone_event(chat, "mismatched-recipient", now.div_euclid(1000));
    mismatch["payload"]["_data"]["Info"]["RecipientAlt"] = json!("15550100122@s.whatsapp.net");
    assert_eq!(deliver(&app, &mismatch).await.status(), 200);
    assert!(!takeover::active(&db.conn, contact, now).await.unwrap());
    assert_eq!(count(&db, "phone_takeover_events WHERE unavailable_reason='recipient_mismatch' AND contact_id IS NULL").await, 1);
    assert_eq!(count(&db, "phone_takeover_events WHERE contact_id IS NOT NULL").await, 0);
  })
  .await;
}

async fn count(db: &crate::database::Db, predicate: &str) -> i64 {
  db.conn
    .query_one(sql(
      &format!("SELECT count(*) AS n FROM {predicate}"),
      vec![],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap()
}
