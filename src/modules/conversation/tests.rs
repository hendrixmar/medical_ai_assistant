mod conversation;
mod dashboard;
mod ordering;
mod sender;

use crate::{
  app::tests::with_database,
  modules::{
    doctor::tests::{fixture, NOW},
    whatsapp::{config::WahaConfig, jobs as preparation},
  },
};
use axum::{body::Body, http::Request, Router};
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use sha2::Sha512;
use std::sync::Arc;
use tower::ServiceExt;

const KEY: &str = "synthetic-issue7-only-HMAC-key-32-characters";
fn sql(query: &str) -> Statement {
  Statement::from_string(DbBackend::Postgres, query)
}
async fn deliver(app: &Router, value: &Value) -> axum::response::Response {
  let raw = serde_json::to_vec(value).unwrap();
  let mut mac = Hmac::<Sha512>::new_from_slice(KEY.as_bytes()).unwrap();
  mac.update(&raw);
  let signature: String = mac
    .finalize()
    .into_bytes()
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect();
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
fn message(id: &str, body: &str) -> Value {
  json!({"id":format!("evt_{id}"),"event":"message","session":"default","engine":"GOWS","payload":{"id":format!("false_15550100101@c.us_{id}"),"from":"15550100101@c.us","fromMe":false,"timestamp":NOW,"body":body,"hasMedia":false,"_data":{"Message":{"conversation":body}}}})
}
async fn setup(
  db: &crate::database::Db,
) -> (
  crate::common::config::Config,
  Arc<crate::modules::doctor::tests::TestClock>,
  Router,
  totp_rs::Totp,
) {
  let (mut cfg, clock, app, totp) = fixture(db).await;
  drop(app);
  Arc::get_mut(&mut cfg).unwrap().waha = WahaConfig::parse(
    Some("http://127.0.0.1:1"),
    Some("synthetic-api-key"),
    Some(KEY),
  )
  .unwrap();
  let app = crate::app::router_with_clock(cfg.clone(), db.clone(), clock.clone());
  (cfg, clock, app, totp)
}
async fn count(db: &crate::database::Db, table: &str) -> i64 {
  db.conn
    .query_one(sql(&format!("SELECT count(*) AS n FROM {table}")))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap()
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn first_five_commits_one_followup_and_combined_privacy_reply_idempotently() {
  with_database(|db| async move {
    let (cfg, clock, app, _) = setup(&db).await;
    let input = message("FIRST", " 5 \n");
    assert_eq!(deliver(&app, &input).await.status(), 200);
    preparation::run_once(&db, clock.as_ref()).await.unwrap();
    super::jobs::run_once(&db, clock.as_ref(), &cfg, None)
      .await
      .unwrap();
    assert_eq!(count(&db, "contacts").await, 1);
    assert_eq!(
      count(&db, "followup_requests WHERE resolved_at IS NULL").await,
      1
    );
    assert_eq!(count(&db, "outbox_messages").await, 1);
    let row = db
      .conn
      .query_one(sql(
        "SELECT rendered_text, privacy_prefix FROM outbox_messages",
      ))
      .await
      .unwrap()
      .unwrap();
    let text: String = row.try_get("", "rendered_text").unwrap();
    assert!(text
      .starts_with("Aviso de privacidad PROVISIONAL: https://practice.test/aviso-de-privacidad"));
    assert!(text.contains("Su solicitud de atención personal quedó registrada"));
    assert!(text.ends_with(
      "1 Agendar\n2 Cambiar cita\n3 Cancelar\n4 Enviar comprobante\n5 Hablar con el doctor"
    ));
    assert!(row.try_get::<bool>("", "privacy_prefix").unwrap());
    assert_eq!(deliver(&app, &input).await.status(), 200);
    assert_eq!(
      super::jobs::run_once(&db, clock.as_ref(), &cfg, None)
        .await
        .unwrap(),
      super::jobs::Outcome::Idle
    );
    assert_eq!(count(&db, "outbox_messages").await, 1);
    assert_eq!(
      count(&db, "contacts WHERE privacy_first_sent_at IS NOT NULL").await,
      0
    );
  })
  .await;
}
