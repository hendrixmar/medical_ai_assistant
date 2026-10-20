mod concurrency;
mod evidence;
mod lifecycle;
mod sender;

use crate::{
  app::tests::with_database,
  modules::{
    doctor::{
      tests::{authenticated_get, field, fixture, get, post, signed_in, text, NOW},
      Clock,
    },
    whatsapp::config::WahaConfig,
  },
};
use axum::{extract::State, Json, Router};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use std::sync::{
  atomic::{AtomicI64, AtomicUsize, Ordering},
  Arc, Mutex,
};

const PATH: &str = "/settings/doctor-number";
const ISSUE: &str = "/settings/doctor-number/challenge";
const KEY: &str = "synthetic-issue8-HMAC-key-at-least-32-bytes";
struct Millis(AtomicI64);
impl Clock for Millis {
  fn now(&self) -> i64 {
    self.now_millis().div_euclid(1000)
  }
  fn now_millis(&self) -> i64 {
    self.0.load(Ordering::SeqCst)
  }
}
impl Millis {
  fn advance(&self, ms: i64) {
    self.0.fetch_add(ms, Ordering::SeqCst);
  }
}
struct Wire {
  calls: AtomicUsize,
  status: Mutex<Value>,
  outgoing: Mutex<Vec<(String, Value)>>,
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
    let wire = Arc::new(Wire {
      calls: AtomicUsize::new(0),
      status: Mutex::new(
        json!({"name":"default","status":"WORKING","me":{"id":"525555555555@c.us"}}),
      ),
      outgoing: Mutex::new(vec![]),
    });
    let app = Router::new()
      .route(
        "/api/sessions/default",
        axum::routing::get(|State(w): State<Arc<Wire>>| async move {
          w.calls.fetch_add(1, Ordering::SeqCst);
          Json(w.status.lock().unwrap().clone())
        }),
      )
      .fallback(sender::fake)
      .with_state(wire.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Self { wire, base, task }
  }
}
fn sql(query: &str) -> Statement {
  Statement::from_string(DbBackend::Postgres, query)
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
async fn setup(
  db: &crate::database::Db,
  p: &Provider,
) -> (
  crate::common::config::Config,
  Arc<Millis>,
  Router,
  String,
  String,
) {
  let (mut cfg, _, app, totp) = fixture(db).await;
  drop(app);
  Arc::get_mut(&mut cfg).unwrap().waha =
    WahaConfig::parse(Some(&p.base), Some("synthetic-api-key"), Some(KEY)).unwrap();
  let clock = Arc::new(Millis(AtomicI64::new((NOW + 30) * 1000 + 123)));
  let app = crate::app::router_with_clock(cfg.clone(), db.clone(), clock.clone());
  let session = signed_in(&app, &totp, NOW + 30).await;
  let csrf = field(
    &text(authenticated_get(&app, "/dashboard", &session).await).await,
    "csrf_token",
  );
  (cfg, clock, app, session, csrf)
}
async fn issue(app: &Router, session: &str, csrf: &str) -> axum::response::Response {
  post(
    app,
    ISSUE,
    Some(session),
    Some("https://practice.test"),
    &[("csrf_token", csrf)],
  )
  .await
}

async fn active_code(app: &Router, session: &str) -> String {
  let html = text(authenticated_get(app, PATH, session).await).await;
  let code = html
    .split("class=\"doctor-number-code\">")
    .nth(1)
    .unwrap()
    .split('<')
    .next()
    .unwrap()
    .to_owned();
  assert_eq!(code.len(), 6);
  assert!(code.bytes().all(|b| b.is_ascii_digit()));
  code
}
fn message(id: &str, sender: &str, body: &str, now: i64) -> Value {
  json!({"event":"message","session":"default","engine":"GOWS","me":{"id":"525555555555@c.us"},"payload":{"id":format!("false_{sender}_{id}"),"from":sender,"fromMe":false,"timestamp":now.div_euclid(1000),"body":body,"hasMedia":false,"_data":{"Message":{"conversation":body}}}})
}
async fn deliver(app: &Router, value: &Value) {
  use hmac::{Hmac, Mac};
  use sha2::Sha512;
  use tower::ServiceExt;
  let raw = serde_json::to_vec(value).unwrap();
  let mut mac = Hmac::<Sha512>::new_from_slice(KEY.as_bytes()).unwrap();
  mac.update(&raw);
  let signature: String = mac
    .finalize()
    .into_bytes()
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect();
  assert_eq!(
    app
      .clone()
      .oneshot(
        axum::http::Request::builder()
          .method("POST")
          .uri("/api/v1/webhooks/waha")
          .header("x-webhook-hmac", signature)
          .body(axum::body::Body::from(raw))
          .unwrap()
      )
      .await
      .unwrap()
      .status(),
    200
  );
}
async fn process(
  db: &crate::database::Db,
  app: &Router,
  clock: &dyn Clock,
  cfg: &crate::common::config::Config,
  input: &Value,
) {
  deliver(app, input).await;
  crate::modules::whatsapp::jobs::run_once(db, clock)
    .await
    .unwrap();
  assert_eq!(
    crate::modules::conversation::jobs::run_once(db, clock, cfg, None)
      .await
      .unwrap(),
    crate::modules::conversation::jobs::Outcome::Processed
  );
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn verified_binding_is_atomic_persistent_read_only_and_never_auto_sends() {
  with_database(|db| async move {
    let p=Provider::new().await; let (cfg,clock,app,session,csrf)=setup(&db,&p).await;
    assert_eq!(issue(&app,&session,&csrf).await.status(),303);
    let code=active_code(&app,&session).await;
    clock.advance(1);
    let input=message("PROOF","5215512345678@c.us",&code,clock.now_millis());
    process(&db,&app,clock.as_ref(),&cfg,&input).await;
    let page=text(authenticated_get(&app,PATH,&session).await).await;
    assert!(page.contains("+5215512345678"));
    assert!(!page.contains("class=\"doctor-number-code\">"));
    assert_eq!(count(&db,"outbox_messages WHERE template='doctor_verified'").await,1);
    assert_eq!(count(&db,"doctor_number_challenges WHERE state='used' AND ciphertext IS NULL AND nonce IS NULL").await,1);
    deliver(&app,&input).await;
    assert_eq!(count(&db,"outbox_messages").await,1);
    for (i,body) in ["1","2","3","4","5","Cancela mi cita","Muéstrame la agenda"].iter().enumerate() {
      process(&db,&app,clock.as_ref(),&cfg,&message(&format!("READ{i}"),"525512345678@c.us",body,clock.now_millis())).await;
    }
    assert_eq!(count(&db,"followup_requests").await,0);
    assert_eq!(count(&db,"outbox_messages WHERE template='doctor_read_only' AND rendered_text LIKE '%https://practice.test/dashboard%' AND rendered_text NOT LIKE '%1 Agendar%'").await,7);
    assert_eq!(count(&db,"outbox_messages WHERE privacy_prefix").await,8);
    assert_eq!(crate::modules::conversation::sender::run_once(&db,clock.as_ref(),&cfg,None).await.unwrap(),crate::modules::conversation::sender::Outcome::Idle);
    let fresh=crate::app::tests::reconnect(&db,2).await;
    let restarted=crate::app::router_with_clock(cfg.clone(),fresh.clone(),clock.clone());
    assert!(text(authenticated_get(&restarted,PATH,&session).await).await.contains("+5215512345678"));
    let proof=db.conn.query_one(sql("SELECT original_target,inbox_message_id,practice_phone FROM doctor_number_state")).await.unwrap().unwrap();
    assert_eq!(proof.try_get::<String>("","original_target").unwrap(),"5215512345678@c.us");
    assert_eq!(proof.try_get::<String>("","inbox_message_id").unwrap(),"false_5215512345678@c.us_PROOF");
    assert_eq!(proof.try_get::<String>("","practice_phone").unwrap(),"525555555555@c.us");
    fresh.conn.close().await.unwrap();
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn protected_read_never_issues_and_browser_rejections_make_no_provider_calls() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (_, _, app, session, csrf) = setup(&db, &p).await;
    let denied = get(&app, PATH).await;
    assert_eq!(denied.status(), 303);
    assert_eq!(
      denied.headers()["location"],
      "/login?return_to=/settings/doctor-number"
    );
    assert_eq!(
      post(&app, ISSUE, None, Some("https://practice.test"), &[])
        .await
        .status(),
      401
    );
    for (origin, token) in [
      ("https://evil.test", csrf.as_str()),
      ("https://practice.test", "bad"),
    ] {
      assert_eq!(
        post(
          &app,
          ISSUE,
          Some(&session),
          Some(origin),
          &[("csrf_token", token)]
        )
        .await
        .status(),
        403
      );
    }
    assert_eq!(p.wire.calls.load(Ordering::SeqCst), 0);
    for _ in 0..2 {
      let page = authenticated_get(&app, PATH, &session).await;
      assert_eq!(page.status(), 200);
      assert_eq!(page.headers()["cache-control"], "no-store");
      assert_eq!(page.headers()["referrer-policy"], "same-origin");
    }
    assert_eq!(count(&db, "doctor_number_challenges").await, 0);
    let created = issue(&app, &session, &csrf).await;
    assert_eq!(created.status(), 303);
    assert_eq!(created.headers()["location"], PATH);
    assert_eq!(count(&db, "doctor_number_challenges").await, 1);
    let limited = issue(&app, &session, &csrf).await;
    assert_eq!(limited.status(), 429);
    assert_eq!(limited.headers()["retry-after"], "60");
  })
  .await;
}
