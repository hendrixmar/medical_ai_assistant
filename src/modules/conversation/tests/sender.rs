mod acknowledgements;
mod budget;
mod commit_loss;
mod domain;
mod resilience;

use super::*;
use crate::modules::{
  conversation::{jobs, sender},
  doctor::Clock,
  whatsapp::client::Client,
};
use axum::{
  extract::State,
  response::{IntoResponse, Response},
  Json,
};
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Mutex,
};

#[derive(Default)]
struct Wire {
  calls: Mutex<Vec<(String, Value)>>,
  ids: AtomicUsize,
  fail_preflight: AtomicUsize,
  send_mode: AtomicUsize,
  block_path: Mutex<Option<String>>,
  entered: tokio::sync::Notify,
  release: tokio::sync::Notify,
}
struct Provider {
  wire: Arc<Wire>,
  base: String,
  task: tokio::task::JoinHandle<()>,
}
impl Drop for Provider {
  fn drop(&mut self) {
    self.task.abort();
  }
}
impl Provider {
  async fn new() -> Self {
    let wire = Arc::new(Wire::default());
    let app = Router::new().fallback(fake).with_state(wire.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
      axum::serve(listener, app).await.unwrap();
    });
    Self { wire, base, task }
  }
  fn count(&self, path: &str) -> usize {
    self
      .wire
      .calls
      .lock()
      .unwrap()
      .iter()
      .filter(|(p, _)| p == path)
      .count()
  }
}
async fn fake(State(w): State<Arc<Wire>>, request: axum::extract::Request) -> Response {
  assert_eq!(request.headers()["x-api-key"], "synthetic-api-key");
  let path = request.uri().path().to_owned();
  let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
    .await
    .unwrap();
  let body = if bytes.is_empty() {
    Value::Null
  } else {
    serde_json::from_slice(&bytes).unwrap()
  };
  w.calls.lock().unwrap().push((path.clone(), body.clone()));
  let blocked = w.block_path.lock().unwrap().as_deref() == Some(path.as_str());
  if blocked {
    w.entered.notify_one();
    w.release.notified().await;
  }
  if path != "/api/sendText" && w.fail_preflight.load(Ordering::SeqCst) > 0 {
    return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
  }
  match path.as_str() {
    "/api/default/new-message-id" => {
      Json(json!({"id":format!("OUT{}",w.ids.fetch_add(1,Ordering::SeqCst))})).into_response()
    }
    "/api/sendSeen" => axum::http::StatusCode::NO_CONTENT.into_response(),
    "/api/startTyping" | "/api/stopTyping" => Json(json!({"result":true})).into_response(),
    "/api/sendText" => {
      w.entered.notify_one();
      let mode = w.send_mode.load(Ordering::SeqCst);
      if mode == 2 {
        w.release.notified().await;
      }
      if mode == 1 {
        return (
          axum::http::StatusCode::INTERNAL_SERVER_ERROR,
          Json(json!({"unknown":"synthetic"})),
        )
          .into_response();
      }
      if mode == 3 {
        return (
          axum::http::StatusCode::UNPROCESSABLE_ENTITY,
          Json(json!({"error":"Session \"default\" does not exist","session":"default"})),
        )
          .into_response();
      }
      Json(json!({"id":format!("true_{}_{}",body["chatId"].as_str().unwrap(),body["id"].as_str().unwrap()),"_data":null})).into_response()
    }
    _ => axum::http::StatusCode::NOT_FOUND.into_response(),
  }
}
async fn outgoing(
  db: &crate::database::Db,
  p: &Provider,
  enabled: bool,
) -> (
  crate::common::config::Config,
  Arc<crate::modules::doctor::tests::TestClock>,
  Router,
  Client,
  totp_rs::Totp,
) {
  let (mut cfg, clock, app, totp) = fixture(db).await;
  drop(app);
  let c = Arc::get_mut(&mut cfg).unwrap();
  c.waha = WahaConfig::parse(Some(&p.base), Some("synthetic-api-key"), Some(KEY)).unwrap();
  c.waha_outbound_enabled = enabled;
  let (app, client, _) = crate::app::compose(cfg.clone(), db.clone(), clock.clone());
  (cfg, clock, app, client.unwrap(), totp)
}
async fn enqueue(
  db: &crate::database::Db,
  app: &Router,
  clock: &dyn Clock,
  cfg: &crate::common::config::Config,
  id: &str,
) {
  assert_eq!(deliver(app, &message(id, "5")).await.status(), 200);
  preparation::run_once(db, clock).await.unwrap();
  assert_eq!(
    jobs::run_once(db, clock, cfg, None).await.unwrap(),
    jobs::Outcome::Processed
  );
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn disabled_zero_http_then_persisted_typing_and_one_wire_commitment() {
  with_database(|db|async move{
    let p=Provider::new().await;
    let (cfg,clock,app,client,_)=outgoing(&db,&p,false).await;
    enqueue(&db,&app,clock.as_ref(),&cfg,"WIRE").await;
    assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Idle);
    assert!(p.wire.calls.lock().unwrap().is_empty());
    drop(app);let mut cfg=cfg;Arc::get_mut(&mut cfg).unwrap().waha_outbound_enabled=true;
    assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Typing);
    let row=db.conn.query_one(sql("SELECT state,typing_delay_ms,typing_due_at,raw_provider_id FROM outbox_messages")).await.unwrap().unwrap();
    assert_eq!(row.try_get::<String>("","state").unwrap(),"typing");
    let delay=row.try_get::<i32>("","typing_delay_ms").unwrap();assert!((2000..=6000).contains(&delay));
    assert_eq!(row.try_get::<i64>("","typing_due_at").unwrap(),clock.now_millis()+i64::from(delay));
    assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Idle);
    let fresh=crate::app::tests::reconnect(&db,2).await;
    clock.advance(6);
    assert_eq!(sender::run_once(&fresh,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Dispatched);
    for _ in 0..3 {sender::run_once(&fresh,clock.as_ref(),&cfg,Some(&client)).await.unwrap();}
    assert_eq!(p.count("/api/sendText"),1);
    assert_eq!(count(&db,"outbox_messages WHERE state='sent' AND positive_ack=1 AND dispatch_committed_at IS NOT NULL").await,1);
    assert_eq!(count(&db,"contacts WHERE privacy_first_sent_at IS NOT NULL").await,1);
    let calls=p.wire.calls.lock().unwrap().clone();
    assert_eq!(calls.iter().map(|(path,_)|path.as_str()).collect::<Vec<_>>(),vec!["/api/default/new-message-id","/api/sendSeen","/api/startTyping","/api/stopTyping","/api/sendText"]);
    assert_eq!(calls[1].1,json!({"session":"default","chatId":"15550100101@c.us","messageIds":["false_15550100101@c.us_WIRE"]}));
    assert_eq!(calls[4].1["linkPreview"],false);
    fresh.conn.close().await.unwrap();
  }).await;
}
