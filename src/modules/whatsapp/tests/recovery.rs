use super::super::{client::Client, config::WahaConfig, inbox::sql, recovery};
use crate::{
  app::tests::with_database,
  modules::{
    conversation::identity,
    doctor::{
      tests::{fixture, NOW},
      Clock,
    },
  },
};
use axum::{
  body::Body,
  extract::{Request, State},
  response::Response,
  routing::any,
  Router,
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone)]
struct Fake(Arc<Mutex<Replies>>);
struct Replies {
  status: &'static str,
  messages: Vec<Value>,
  history_failures: u8,
}
pub(super) struct Provider {
  fake: Fake,
  pub(super) origin: String,
  task: tokio::task::JoinHandle<()>,
}
impl Drop for Provider {
  fn drop(&mut self) {
    self.task.abort();
  }
}
impl Provider {
  pub(super) async fn new(messages: Vec<Value>, history_failures: u8) -> Self {
    let fake = Fake(Arc::new(Mutex::new(Replies {
      status: "STOPPED",
      messages,
      history_failures,
    })));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
      .route("/{*path}", any(reply))
      .with_state(fake.clone());
    let task = tokio::spawn(async move {
      axum::serve(listener, app).await.unwrap();
    });
    Self { fake, origin, task }
  }

  async fn status(&self, status: &'static str) {
    self.fake.0.lock().await.status = status;
  }
}

async fn reply(State(fake): State<Fake>, request: Request) -> Response {
  let mut replies = fake.0.lock().await;
  let (status, body) = match request.uri().path() {
    "/api/sessions/default" => (
      200,
      json!({"name":"default","status":replies.status,"engine":{"engine":"GOWS"}}),
    ),
    "/api/messages" if replies.history_failures > 0 => {
      replies.history_failures -= 1;
      (503, json!({"error":"synthetic"}))
    }
    "/api/messages" => (200, Value::Array(replies.messages.clone())),
    _ => (404, json!({"error":"unexpected"})),
  };
  Response::builder()
    .status(status)
    .header("content-type", "application/json")
    .body(Body::from(body.to_string()))
    .unwrap()
}

async fn setup(
  db: &crate::database::Db,
  provider: &Provider,
) -> (
  crate::common::config::Config,
  Arc<crate::modules::doctor::tests::TestClock>,
  Client,
) {
  let (mut cfg, clock, app, _) = fixture(db).await;
  drop(app);
  Arc::get_mut(&mut cfg).unwrap().waha = WahaConfig::parse(
    Some(&provider.origin),
    Some("synthetic-api-key"),
    Some("synthetic-webhook-key-with-32-bytes"),
  )
  .unwrap();
  let client = Client::new(cfg.waha.as_ref().unwrap()).unwrap();
  (cfg, clock, client)
}

async fn seed_open_appointment(db: &crate::database::Db, chat: &str, now: i64) -> Uuid {
  let tx = db.conn.begin().await.unwrap();
  let seed_id = format!("false_{chat}_SEED{}", Uuid::now_v7().simple());
  let server_timestamp = now.div_euclid(1000).saturating_sub(10);
  tx.execute(sql(
    "INSERT INTO inbox_messages(message_id,original_chat_identity,resolved_phone_identity,server_timestamp,arrived_at,raw_envelope) VALUES ($1,$2,$2,$3,to_timestamp($4::double precision/1000),$5)",
    vec![seed_id.clone().into(), chat.into(), server_timestamp.into(), now.into(), b"{}".to_vec().into()],
  ))
  .await
  .unwrap();
  let contact = identity::assign(&tx, &seed_id, chat, Some(chat), now)
    .await
    .unwrap();
  tx.execute(sql(
    "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,created_at,updated_at) VALUES ($1,$2,'Synthetic patient',$3,'Consulta',30,0,'in_person',$4,$5,'UTC','Practice',0,0,'confirmed','not_required','pending',$6,$6)",
    vec![
      Uuid::now_v7().into(),
      contact.into(),
      Uuid::now_v7().into(),
      (now + 86_400_000).into(),
      (now + 88_200_000).into(),
      now.into(),
    ],
  ))
  .await
  .unwrap();
  tx.commit().await.unwrap();
  contact
}

fn history_message(chat: &str, timestamp: i64) -> Value {
  json!({
    "id":format!("false_{chat}_history1"),
    "timestamp":timestamp,
    "from":chat,
    "fromMe":false,
    "to":null,
    "body":"Necesito confirmar mi cita",
    "hasMedia":false
  })
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn reconnect_backfills_via_deduplicated_intake_and_persists_checkpoint() {
  with_database(|db| async move {
    let chat = "15550100101@c.us";
    let message = history_message(chat, NOW + 40);
    let provider = Provider::new(vec![message], 0).await;
    let (cfg, clock, client) = setup(&db, &provider).await;
    let _contact = seed_open_appointment(&db, chat, clock.now_millis()).await;

    assert!(recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap());
    let disconnected = db
      .conn
      .query_one(sql("SELECT disconnected_at FROM provider_recovery_state WHERE singleton=true", vec![]))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(disconnected.try_get::<Option<i64>>("", "disconnected_at").unwrap(), Some(NOW * 1000 + 30_000));

    provider.status("WORKING").await;
    clock.advance(15);
    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert_eq!(count(&db, "inbox_messages WHERE message_id='false_15550100101@c.us_history1' AND intake_source='provider_history' AND server_timestamp=1800000040").await, 1);
    assert_eq!(count(&db, "processing_jobs WHERE inbox_message_id='false_15550100101@c.us_history1' AND kind='prepare_inbox'").await, 1);
    assert_eq!(count(&db, "provider_recovery_checkpoints WHERE chat_identity='15550100101@c.us'").await, 1);
    assert_eq!(count(&db, "attention_items WHERE kind='whatsapp_disconnected' AND resolved_at IS NOT NULL").await, 1);

    clock.advance(15);
    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    let state = db
      .conn
      .query_one(sql("SELECT recovery_status,disconnected_at FROM provider_recovery_state WHERE singleton=true", vec![]))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(state.try_get::<String>("", "recovery_status").unwrap(), "complete");
    assert_eq!(state.try_get::<Option<i64>>("", "disconnected_at").unwrap(), None);

    // A subsequent worker run does not replay completed history or duplicate intake.
    clock.advance(15);
    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert_eq!(count(&db, "inbox_messages WHERE message_id='false_15550100101@c.us_history1'").await, 1);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn failed_history_page_keeps_checkpoint_unchanged_until_retry_commits() {
  with_database(|db| async move {
    let chat = "15550100102@c.us";
    let provider = Provider::new(vec![history_message(chat, NOW + 40)], 1).await;
    let (cfg, clock, client) = setup(&db, &provider).await;
    seed_open_appointment(&db, chat, clock.now_millis()).await;
    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();

    provider.status("WORKING").await;
    clock.advance(15);
    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert_eq!(count(&db, "provider_recovery_checkpoints").await, 0);
    assert_eq!(count(&db, "inbox_messages WHERE intake_source='provider_history'").await, 0);
    assert_eq!(count(&db, "attention_items WHERE kind='whatsapp_recovery_incomplete' AND resolved_at IS NULL").await, 1);

    // Backoff survives a worker restart; replaying the page commits one durable intake first.
    clock.advance(6);
    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert_eq!(count(&db, "inbox_messages WHERE message_id='false_15550100102@c.us_history1' AND intake_source='provider_history'").await, 1);
    assert_eq!(count(&db, "provider_recovery_checkpoints").await, 1);
    assert_eq!(count(&db, "attention_items WHERE kind='whatsapp_recovery_incomplete' AND resolved_at IS NULL").await, 0);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn hold_expiring_during_recovery_extends_history_before_completion() {
  with_database(|db| async move {
    let chat = "15550100103@c.us";
    let provider = Provider::new(vec![history_message(chat, NOW + 40)], 0).await;
    let (cfg, clock, client) = setup(&db, &provider).await;
    let contact = seed_open_appointment(&db, chat, clock.now_millis()).await;
    let row = db
      .conn
      .query_one(sql(
        "SELECT id FROM appointments WHERE contact_id=$1",
        vec![contact.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    let appointment: Uuid = row.try_get("", "id").unwrap();
    let tx = db.conn.begin().await.unwrap();
    tx.execute(sql(
      "UPDATE appointments SET format='video',status='awaiting_payment',payment='awaiting_receipt' WHERE id=$1",
      vec![appointment.into()],
    ))
    .await
    .unwrap();
    tx.execute(sql(
      "INSERT INTO appointment_holds(id,appointment_id,starts_at,ends_at,active,expires_at) VALUES ($1,$2,$3,$4,true,$5)",
      vec![
        Uuid::now_v7().into(),
        appointment.into(),
        (clock.now_millis() + 86_400_000).into(),
        (clock.now_millis() + 88_200_000).into(),
        (clock.now_millis() + 50_000).into(),
      ],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    provider.status("WORKING").await;
    clock.advance(15);
    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    let first_end = clock.now_millis();

    clock.advance(5);
    let expired_at = clock.now_millis();
    let tx = db.conn.begin().await.unwrap();
    tx.execute(sql(
      "UPDATE appointments SET status='expired',expired_at=$2 WHERE id=$1",
      vec![appointment.into(), expired_at.into()],
    ))
    .await
    .unwrap();
    tx.execute(sql(
      "UPDATE appointment_holds SET active=false WHERE appointment_id=$1",
      vec![appointment.into()],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    clock.advance(10);
    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert_eq!(
      count(
        &db,
        "inbox_messages WHERE message_id='false_15550100103@c.us_history1' AND intake_source='provider_history'",
      )
      .await,
      1
    );
    let hold = db
      .conn
      .query_one(sql(
        "SELECT active FROM appointment_holds WHERE appointment_id=$1",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert!(!hold.try_get::<bool>("", "active").unwrap());
    let checkpoint = db
      .conn
      .query_one(sql(
        "SELECT covered_through FROM provider_recovery_checkpoints WHERE contact_id=$1 AND chat_identity=$2",
        vec![contact.into(), chat.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    let covered: i64 = checkpoint.try_get("", "covered_through").unwrap();
    assert!(covered > first_end.div_euclid(1000) * 1000);
    let target = db
      .conn
      .query_one(sql(
        "SELECT state FROM provider_recovery_targets WHERE contact_id=$1 AND chat_identity=$2",
        vec![contact.into(), chat.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(target.try_get::<String>("", "state").unwrap(), "complete");

    clock.advance(15);
    recovery::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert_eq!(
      count(&db, "provider_recovery_state WHERE singleton=true AND recovery_status='complete'")
        .await,
      1
    );
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
