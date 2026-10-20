use super::{model::Appointment, store, timers};
use crate::{
  app::tests::with_database,
  database::Db,
  modules::{
    conversation::{jobs as conversation, sender},
    doctor::{
      tests::{authenticated_get, text},
      Clock,
    },
    google_calendar::{self, tests::fixture::Fixture},
    whatsapp::{self, client::Client, config::WahaConfig, inbox::sql},
  },
};
use axum::{
  body::{to_bytes, Body},
  extract::{Request, State},
  http::StatusCode,
  response::{IntoResponse, Response},
  Router,
};
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};
use sha2::Sha512;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

const KEY: &str = "synthetic-product-journey-HMAC-key-at-least-32";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\0\0\0\0\0";

async fn provider(State(sent): State<Arc<Mutex<Vec<String>>>>, request: Request) -> Response {
  let path = request.uri().path().to_owned();
  match path.as_str() {
    "/media/receipt.png" => ([("content-type", "image/png")], PNG).into_response(),
    "/v1/messages" => {
      to_bytes(request.into_body(), 16 * 1024).await.unwrap();
      (
        [("content-type", "application/json")],
        r#"{"content":[{"type":"text","text":"{\"intent\":\"schedule_in_person\",\"date_phrase\":\"pasado mañana\",\"time_phrase\":\"10 am\",\"uncertain\":false}"}]}"#,
      ).into_response()
    }
    "/api/default/new-message-id" => {
      axum::Json(json!({"id":Uuid::now_v7().simple().to_string()})).into_response()
    }
    "/api/sendSeen" | "/api/startTyping" | "/api/stopTyping" => {
      axum::Json(json!({"result":true})).into_response()
    }
    "/api/sendText" => {
      let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
      let body: Value = serde_json::from_slice(&bytes).unwrap();
      sent
        .lock()
        .await
        .push(body["text"].as_str().unwrap().to_owned());
      axum::Json(json!({"id":format!("true_{}_{}",body["chatId"].as_str().unwrap(),body["id"].as_str().unwrap())})).into_response()
    }
    _ => StatusCode::NOT_FOUND.into_response(),
  }
}

struct Journey {
  f: Fixture,
  waha: Client,
  base: String,
  sent: Arc<Mutex<Vec<String>>>,
  server: tokio::task::JoinHandle<()>,
}
impl Drop for Journey {
  fn drop(&mut self) {
    self.server.abort();
  }
}
impl Journey {
  async fn new(db: &Db) -> Self {
    let mut f = Fixture::new(db).await;
    let sent = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(provider).with_state(sent.clone());
    let server = tokio::spawn(async move {
      axum::serve(listener, app).await.unwrap();
    });
    drop(std::mem::take(&mut f.app));
    let cfg = Arc::get_mut(&mut f.cfg).unwrap();
    cfg.waha = WahaConfig::parse(Some(&base), Some("synthetic-api-key"), Some(KEY)).unwrap();
    cfg.waha_outbound_enabled = true;
    cfg.waha_contact_replies_per_hour = 1000;
    let (app, waha, _) = crate::app::compose(f.cfg.clone(), db.clone(), f.clock.clone());
    f.app = app;
    f.connect().await;
    assert_eq!(
      f.post(
        "/settings/google/calendars",
        &[
          ("generation", "1"),
          ("selection_version", "1"),
          ("booking_calendar", "secondary@example.invalid"),
          ("blocking_calendar", "secondary@example.invalid"),
        ]
      )
      .await
      .status(),
      303
    );
    db.conn.execute(sql(
      "UPDATE practice SET name='Consultorio de prueba',location='Consultorio de prueba',timezone='UTC',weekly_hours=$1,buffer_before_minutes=0,buffer_after_minutes=0,minimum_notice_minutes=60,grid_minutes=30,horizon_days=30,cancellation_policy='Confirme antes de cancelar.',bank='Banco de prueba',beneficiary='Beneficiario de prueba',clabe='012345678901234567',receipt_window_minutes=4320,review_margin_minutes=180,grace_minutes=10,reminder_2h_enabled=true WHERE id=1",
      vec![json!(vec![json!({"status":"open","ranges":[{"start":0,"end":1440}]} );7]).into()],
    )).await.unwrap();
    db.conn.execute(sql(
      "INSERT INTO services(id,name,duration_minutes,price_centavos) VALUES ($1,'Consulta general',30,150000)",
      vec![Uuid::now_v7().into()],
    )).await.unwrap();
    Self {
      f,
      waha: waha.unwrap(),
      base,
      sent,
      server,
    }
  }
  fn envelope(&self, body: &str, chat: &str) -> Value {
    let id = Uuid::now_v7().simple().to_string();
    json!({"id":format!("evt_{id}"),"event":"message","session":"default","engine":"GOWS","payload":{"id":format!("false_{chat}_{id}"),"from":chat,"fromMe":false,"timestamp":self.f.clock.now(),"body":body,"hasMedia":false,"_data":{"Message":{"conversation":body}}}})
  }
  async fn deliver(&self, value: &Value) -> Response {
    let bytes = serde_json::to_vec(value).unwrap();
    let mut mac = Hmac::<Sha512>::new_from_slice(KEY.as_bytes()).unwrap();
    mac.update(&bytes);
    let signature = mac
      .finalize()
      .into_bytes()
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect::<String>();
    self
      .f
      .app
      .clone()
      .oneshot(
        Request::builder()
          .method("POST")
          .uri("/api/v1/webhooks/waha")
          .header("x-webhook-hmac", signature)
          .body(Body::from(bytes))
          .unwrap(),
      )
      .await
      .unwrap()
  }
  async fn say(&self, db: &Db, value: &str) {
    assert_eq!(
      self
        .deliver(&self.envelope(value, "15550100101@c.us"))
        .await
        .status(),
      200
    );
    whatsapp::jobs::run_once(db, self.f.clock.as_ref())
      .await
      .unwrap();
    assert_eq!(
      conversation::run_once(db, self.f.clock.as_ref(), &self.f.cfg, Some(&self.f.client))
        .await
        .unwrap(),
      conversation::Outcome::Processed
    );
    self.flush(db).await;
  }
  async fn flush(&self, db: &Db) {
    for _ in 0..128 {
      let n: i64 = db
        .conn
        .query_one(sql(
          "SELECT count(*) AS n FROM outbox_messages WHERE state IN ('queued','typing')",
          vec![],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap();
      if n == 0 {
        return;
      }
      sender::run_once(db, self.f.clock.as_ref(), &self.f.cfg, Some(&self.waha))
        .await
        .unwrap();
      self.f.clock.advance(6);
    }
    panic!("Outgoing work did not drain through the paced sender");
  }
  async fn calendar(&self, db: &Db) {
    for _ in 0..16 {
      google_calendar::appointments::run_once(
        db,
        self.f.clock.as_ref(),
        &self.f.cfg,
        Some(&self.f.client),
      )
      .await
      .unwrap();
      self.f.clock.advance(2);
    }
    self.flush(db).await;
  }
  async fn enable_video(&self, db: &Db) {
    assert_eq!(
      self
        .f
        .post(
          "/settings/google/probe",
          &[
            ("generation", "1"),
            ("selection_version", "2"),
            ("expected_probe_id", "")
          ]
        )
        .await
        .status(),
      303
    );
    for _ in 0..8 {
      google_calendar::worker::run_once(
        db,
        self.f.clock.as_ref(),
        &self.f.cfg,
        Some(&self.f.client),
      )
      .await
      .unwrap();
      self.f.clock.advance(11);
    }
  }
  async fn latest(&self, db: &Db) -> Appointment {
    let id: Uuid = db
      .conn
      .query_one(sql(
        "SELECT id FROM appointments ORDER BY created_at DESC,id DESC LIMIT 1",
        vec![],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "id")
      .unwrap();
    store::get(&db.conn, id).await.unwrap().unwrap()
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn signed_menu_booking_reschedule_cancellation_receipt_verification_meet_and_reminder() {
  with_database(|db| async move {
    let j = Journey::new(&db).await;
    let day = chrono::DateTime::from_timestamp(j.f.clock.now(),0).unwrap().date_naive();
    let date = (day + chrono::Duration::days(3)).to_string();
    let replacement_date = (day + chrono::Duration::days(4)).to_string();
    for answer in ["1","Paciente <prueba>","1","1",&date] { j.say(&db,answer).await; }
    let offers = db.conn.query_one(sql("SELECT (SELECT count(*) FROM appointments) AS appointments,(SELECT count(*) FROM appointment_holds WHERE active) AS holds",vec![])).await.unwrap().unwrap();
    assert_eq!(offers.try_get::<i64>("","appointments").unwrap(),0,"An offer is not a booking");
    assert_eq!(offers.try_get::<i64>("","holds").unwrap(),0,"An offer must not reserve time");
    j.say(&db,"1").await;
    let first = j.latest(&db).await;
    assert_eq!(first.status,"confirmed");
    assert_eq!(first.payment,"not_required");
    let contact_page = text(authenticated_get(&j.f.app,&format!("/contacts/{}",first.contact_id),&j.f.session).await).await;
    assert!(contact_page.contains("Paciente &lt;prueba&gt;"));
    assert!(!contact_page.contains("Paciente <prueba>"));
    j.calendar(&db).await;
    let first = store::get(&db.conn,first.id).await.unwrap().unwrap();
    assert_eq!(first.calendar_sync,"synced");
    let original_event = first.event_id.clone();
    for answer in ["2","1",&replacement_date,"1"] { j.say(&db,answer).await; }
    j.calendar(&db).await;
    let moved = store::get(&db.conn,first.id).await.unwrap().unwrap();
    assert_eq!(moved.event_id,original_event,"Reschedule updates the same owned event");
    assert_eq!(moved.starts_at - first.starts_at,86_400_000);
    assert_eq!(moved.payment,"not_required");
    for answer in ["3","1","CONFIRMAR"] { j.say(&db,answer).await; }
    j.calendar(&db).await;
    let cancelled = store::get(&db.conn,first.id).await.unwrap().unwrap();
    assert_eq!(cancelled.status,"cancelled");
    assert!(j.f.provider.event.lock().await.is_none());

    j.enable_video(&db).await;
    for answer in ["1","Paciente de video","1","2",&date,"1"] { j.say(&db,answer).await; }
    let video = j.latest(&db).await;
    assert_eq!(video.format,"video");
    assert_eq!(video.status,"awaiting_payment");
    assert_eq!(video.payment,"awaiting_receipt");
    assert_eq!(video.receipt_deadline,Some(video.starts_at-180*60_000),"Review margin is measured before the start, not the end");
    assert!(video.event_id.is_none());
    assert!(video.meet_url.is_none());
    let before = j.sent.lock().await.clone();
    assert!(before.iter().all(|text| !text.contains("meet.google.com")),"No conference access before Doctor verification");
    let mut receipt = j.envelope("", "15550100101@c.us");
    receipt["payload"]["hasMedia"] = json!(true);
    receipt["payload"]["media"] = json!({"url":format!("{}/media/receipt.png",j.base),"mimetype":"image/png"});
    receipt["payload"]["_data"] = json!({"Message":{"imageMessage":{}}});
    assert_eq!(j.deliver(&receipt).await.status(),200);
    assert_eq!(j.deliver(&receipt).await.status(),200);
    whatsapp::jobs::run_once(&db,j.f.clock.as_ref()).await.unwrap();
    conversation::run_once(&db,j.f.clock.as_ref(),&j.f.cfg,Some(&j.f.client)).await.unwrap();
    j.flush(&db).await;
    let reviewing = store::get(&db.conn,video.id).await.unwrap().unwrap();
    assert_eq!(reviewing.status,"awaiting_payment");
    assert_eq!(reviewing.payment,"under_review");
    assert!(reviewing.meet_url.is_none());
    let evidence = db.conn.query_one(sql("SELECT count(*) AS n,min(id::text) AS id FROM receipts WHERE appointment_id=$1",vec![video.id.into()])).await.unwrap().unwrap();
    assert_eq!(evidence.try_get::<i64>("","n").unwrap(),1,"Duplicate webhook cannot duplicate evidence or review");
    let receipt_id:String = evidence.try_get("","id").unwrap();
    let hold = db.conn.query_one(sql("SELECT expires_at FROM appointment_holds WHERE appointment_id=$1 AND active",vec![video.id.into()])).await.unwrap().unwrap();
    assert!(hold.try_get::<Option<i64>>("","expires_at").unwrap().is_none());
    db.conn.execute(sql("UPDATE practice SET bank=NULL,beneficiary=NULL,clabe=NULL,receipt_window_minutes=NULL,review_margin_minutes=NULL,grace_minutes=NULL WHERE id=1",vec![])).await.unwrap();
    db.conn.execute(sql("UPDATE services SET duration_minutes=45,price_centavos=999900",vec![])).await.unwrap();
    for answer in ["2","1",&replacement_date,"1"] { j.say(&db,answer).await; }
    let rescheduled = store::get(&db.conn,video.id).await.unwrap().unwrap();
    assert_eq!(rescheduled.starts_at-video.starts_at,86_400_000);
    assert_eq!(rescheduled.ends_at-rescheduled.starts_at,video.ends_at-video.starts_at);
    assert_eq!(rescheduled.price_centavos,video.price_centavos);
    assert_eq!(rescheduled.receipt_deadline,video.receipt_deadline);
    assert_eq!(rescheduled.payment,"under_review");
    assert!(rescheduled.bank==video.bank && rescheduled.beneficiary==video.beneficiary && rescheduled.clabe==video.clabe,"Rescheduling preserves the offered transfer instructions, not current configuration");
    assert!(rescheduled.event_id.is_none() && rescheduled.meet_url.is_none());
    let protected = db.conn.query_one(sql("SELECT count(*) AS n,count(*) FILTER(WHERE expires_at IS NULL AND starts_at=$2) AS protected FROM appointment_holds WHERE appointment_id=$1 AND active",vec![video.id.into(),rescheduled.starts_at.into()])).await.unwrap().unwrap();
    assert_eq!(protected.try_get::<i64>("","n").unwrap(),1);
    assert_eq!(protected.try_get::<i64>("","protected").unwrap(),1,"A timely Receipt keeps its replacement Hold protected");
    let review_path = format!("/dashboard/payments/receipts/{receipt_id}");
    assert_eq!(authenticated_get(&j.f.app,&review_path,&j.f.session).await.status(),200);
    assert_eq!(j.f.post(&format!("{review_path}/verify"),&[]).await.status(),303);
    assert_eq!(j.f.post(&format!("{review_path}/verify"),&[]).await.status(),303);
    j.calendar(&db).await;
    let confirmed = store::get(&db.conn,video.id).await.unwrap().unwrap();
    assert_eq!(confirmed.status,"confirmed");
    assert_eq!(confirmed.payment,"verified");
    assert_eq!(confirmed.calendar_sync,"synced");
    let meet = confirmed.meet_url.as_deref().unwrap();
    assert!(j.sent.lock().await.iter().any(|text| text.contains(meet)));
    let dashboard_response = authenticated_get(&j.f.app,"/dashboard",&j.f.session).await;
    assert_eq!(dashboard_response.status(),200);
    let dashboard = text(dashboard_response).await;
    assert!(dashboard.contains("Paciente de video"));
    j.say(&db,"CONFIRMO").await;
    assert!(store::get(&db.conn,video.id).await.unwrap().unwrap().attendance_confirmed_at.is_some());
    let due = confirmed.starts_at-24*60*60_000;
    j.f.clock.advance((due-j.f.clock.now_millis())/1000);
    for _ in 0..16 { timers::run_once(&db,j.f.clock.as_ref(),&j.f.cfg).await.unwrap(); }
    j.flush(&db).await;
    let reminder = db.conn.query_one(sql("SELECT count(*) AS n FROM outbox_messages WHERE appointment_id=$1 AND purpose='reminder' AND state='sent'",vec![video.id.into()])).await.unwrap().unwrap();
    assert_eq!(reminder.try_get::<i64>("","n").unwrap(),1,"Explicit attendance reply must not invalidate the promised reminder");
    assert_eq!(j.f.post(&format!("/appointments/{}/cancel",video.id),&[]).await.status(),303);
    j.calendar(&db).await;
    let cancelled_paid = store::get(&db.conn,video.id).await.unwrap().unwrap();
    assert_eq!(cancelled_paid.status,"cancelled");
    assert_eq!(cancelled_paid.payment,"verified");
    let refund_path = format!("/appointments/{}/refund/resolve",video.id);
    assert_eq!(j.f.app.clone().oneshot(Request::builder().method("POST").uri(&refund_path).body(Body::empty()).unwrap()).await.unwrap().status(),401);
    assert_eq!(crate::modules::doctor::tests::post(&j.f.app,&refund_path,Some(&j.f.session),j.f.cfg.public_origin.as_deref(),&[]).await.status(),403);
    assert_eq!(crate::modules::doctor::tests::post(&j.f.app,&refund_path,Some(&j.f.session),Some("https://other.invalid"),&[("csrf_token",&j.f.csrf)]).await.status(),403);
    let pending = db.conn.query_one(sql("SELECT count(*) AS n FROM attention_items WHERE appointment_id=$1 AND kind='refund_due' AND resolved_at IS NULL",vec![video.id.into()])).await.unwrap().unwrap();
    assert_eq!(pending.try_get::<i64>("","n").unwrap(),1,"Only an authenticated Doctor can record the manual refund resolution");
    let (first_resolution, repeated_resolution) = tokio::join!(j.f.post(&refund_path,&[]),j.f.post(&refund_path,&[]));
    assert_eq!(first_resolution.status(),303);
    assert_eq!(repeated_resolution.status(),303);
    let refund = db.conn.query_one(sql("SELECT (SELECT count(*) FROM attention_items WHERE appointment_id=$1 AND kind='refund_due' AND resolved_at IS NULL) AS pending,(SELECT count(*) FROM appointment_history WHERE appointment_id=$1 AND kind='refund_resolved' AND actor='Doctor') AS resolutions",vec![video.id.into()])).await.unwrap().unwrap();
    assert_eq!(refund.try_get::<i64>("","pending").unwrap(),0);
    assert_eq!(refund.try_get::<i64>("","resolutions").unwrap(),1);
    let settled = store::get(&db.conn,video.id).await.unwrap().unwrap();
    assert_eq!(settled.status,"cancelled");
    assert_eq!(settled.payment,"verified");
    assert_eq!(settled.revision,cancelled_paid.revision,"Recording a refund must not invalidate pending Calendar cleanup");
    assert_eq!(settled.price_centavos,video.price_centavos);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn grace_receipt_keeps_original_expiry_and_does_not_strand_the_hold() {
  with_database(|db| async move {
    let j = Journey::new(&db).await;
    j.enable_video(&db).await;
    db.conn
      .execute_unprepared("UPDATE practice SET receipt_window_minutes=30 WHERE id=1")
      .await
      .unwrap();
    let day = chrono::DateTime::from_timestamp(j.f.clock.now(), 0)
      .unwrap()
      .date_naive();
    let date = (day + chrono::Duration::days(3)).to_string();
    for answer in ["1", "Paciente de gracia", "1", "2", &date, "1"] {
      j.say(&db, answer).await;
    }
    let appointment = j.latest(&db).await;
    let original_expiry: i64 = db
      .conn
      .query_one(sql(
        "SELECT expires_at FROM appointment_holds WHERE appointment_id=$1 AND active",
        vec![appointment.id.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "expires_at")
      .unwrap();
    let deadline = appointment.receipt_deadline.unwrap();
    j.f
      .clock
      .advance((deadline + 1000 - j.f.clock.now_millis()) / 1000);
    let mut receipt = j.envelope("", "15550100101@c.us");
    receipt["payload"]["hasMedia"] = json!(true);
    receipt["payload"]["media"] =
      json!({"url":format!("{}/media/receipt.png",j.base),"mimetype":"image/png"});
    receipt["payload"]["_data"] = json!({"Message":{"imageMessage":{}}});
    assert_eq!(j.deliver(&receipt).await.status(), 200);
    j.flush(&db).await;
    let received = store::get(&db.conn, appointment.id).await.unwrap().unwrap();
    assert_eq!(received.status, "awaiting_payment");
    assert_eq!(
      received.payment, "awaiting_receipt",
      "Grace is not timely-Receipt protection"
    );
    let evidence = db
      .conn
      .query_one(sql(
        "SELECT timely,late FROM receipts WHERE appointment_id=$1",
        vec![appointment.id.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert!(!evidence.try_get::<bool>("", "timely").unwrap());
    assert!(!evidence.try_get::<bool>("", "late").unwrap());
    j.f
      .clock
      .advance((original_expiry + 1000 - j.f.clock.now_millis()) / 1000);
    for _ in 0..16 {
      timers::run_once(&db, j.f.clock.as_ref(), &j.f.cfg)
        .await
        .unwrap();
    }
    let expired = store::get(&db.conn, appointment.id).await.unwrap().unwrap();
    assert_eq!(
      expired.status, "expired",
      "Intake revision must not invalidate the only expiry job"
    );
    let hold = db
      .conn
      .query_one(sql(
        "SELECT count(*) AS n FROM appointment_holds WHERE appointment_id=$1 AND active",
        vec![appointment.id.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(hold.try_get::<i64>("", "n").unwrap(), 0);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn one_contact_can_name_different_patients_but_only_three_open_appointments() {
  with_database(|db| async move {
    let j = Journey::new(&db).await;
    let day = chrono::DateTime::from_timestamp(j.f.clock.now(), 0).unwrap().date_naive();
    let date = (day + chrono::Duration::days(3)).to_string();
    for patient in ["Paciente uno", "Paciente dos", "Paciente tres", "Paciente cuatro"] {
      for answer in ["1", patient, "1", "1", &date, "1"] {
        j.say(&db, answer).await;
      }
    }
    let limits = db.conn.query_one(sql(
      "SELECT count(*) AS appointments,count(DISTINCT contact_id) AS contacts,count(DISTINCT patient_name) AS patients,(SELECT count(*) FROM appointment_holds WHERE active) AS holds FROM appointments WHERE status IN ('requested','awaiting_payment','confirmed')",
      vec![],
    )).await.unwrap().unwrap();
    assert_eq!(limits.try_get::<i64>("", "appointments").unwrap(), 3);
    assert_eq!(limits.try_get::<i64>("", "contacts").unwrap(), 1);
    assert_eq!(limits.try_get::<i64>("", "patients").unwrap(), 3);
    assert_eq!(limits.try_get::<i64>("", "holds").unwrap(), 3);
    for answer in ["3", "1", "CONFIRMAR", "1", "Paciente cuatro", "1", "1", &date, "1"] {
      j.say(&db, answer).await;
    }
    let replacement = db.conn.query_one(sql(
      "SELECT count(*) AS total,count(*) FILTER(WHERE status IN ('requested','awaiting_payment','confirmed')) AS open,count(*) FILTER(WHERE status='cancelled') AS cancelled FROM appointments",
      vec![],
    )).await.unwrap().unwrap();
    assert_eq!(replacement.try_get::<i64>("", "total").unwrap(), 4);
    assert_eq!(replacement.try_get::<i64>("", "open").unwrap(), 3);
    assert_eq!(replacement.try_get::<i64>("", "cancelled").unwrap(), 1);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn missing_current_timezone_blocks_new_bookings_but_preserves_existing_facts() {
  with_database(|db| async move {
    let j = Journey::new(&db).await;
    let day = chrono::DateTime::from_timestamp(j.f.clock.now(), 0).unwrap().date_naive();
    let date = (day + chrono::Duration::days(3)).to_string();
    for answer in ["1", "Paciente anterior", "1", "1", &date, "1"] {
      j.say(&db, answer).await;
    }
    let original = j.latest(&db).await;
    db.conn.execute(sql("UPDATE practice SET timezone=NULL WHERE id=1", vec![])).await.unwrap();
    for answer in ["1", "Paciente nuevo", "1", "1", &date, "1"] {
      j.say(&db, answer).await;
    }
    let stored = store::get(&db.conn, original.id).await.unwrap().unwrap();
    assert!(stored.timezone == original.timezone);
    assert_eq!(stored.starts_at, original.starts_at);
    assert_eq!(stored.ends_at, original.ends_at);
    assert_eq!(stored.price_centavos, original.price_centavos);
    let counts = db.conn.query_one(sql(
      "SELECT (SELECT count(*) FROM appointments) AS appointments,(SELECT count(*) FROM appointment_holds WHERE active) AS holds",
      vec![],
    )).await.unwrap().unwrap();
    assert_eq!(counts.try_get::<i64>("", "appointments").unwrap(), 1, "A missing timezone cannot be guessed for a new Appointment");
    assert_eq!(counts.try_get::<i64>("", "holds").unwrap(), 1);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn competing_prepared_choices_commit_only_one_appointment_and_hold() {
  use super::{booking, model::Format};
  with_database(|db| async move {
    let j = Journey::new(&db).await;
    j.say(&db, "Hola").await;
    let other = j.envelope("Hola", "15550100102@c.us");
    assert_eq!(j.deliver(&other).await.status(), 200);
    whatsapp::jobs::run_once(&db, j.f.clock.as_ref()).await.unwrap();
    conversation::run_once(&db, j.f.clock.as_ref(), &j.f.cfg, Some(&j.f.client)).await.unwrap();
    j.flush(&db).await;
    let inputs = db.conn.query_all(sql("SELECT contact_id,message_id FROM inbox_messages ORDER BY arrived_at,message_id", vec![])).await.unwrap();
    assert_eq!(inputs.len(), 2);
    let contact_a: Uuid = inputs[0].try_get("", "contact_id").unwrap();
    let contact_b: Uuid = inputs[1].try_get("", "contact_id").unwrap();
    let inbox_a: String = inputs[0].try_get("", "message_id").unwrap();
    let inbox_b: String = inputs[1].try_get("", "message_id").unwrap();
    assert_ne!(contact_a, contact_b);
    let service_id: Uuid = db.conn.query_one(sql("SELECT id FROM services", vec![])).await.unwrap().unwrap().try_get("", "id").unwrap();
    let day = chrono::DateTime::from_timestamp(j.f.clock.now(), 0).unwrap().date_naive();
    let starts_at = (day + chrono::Duration::days(3)).and_hms_opt(9, 0, 0).unwrap().and_utc().timestamp_millis();
    let request = |contact| booking::SelectionRequest {
      contact, service_id, format: Format::InPerson, starts_at,
      appointment_id: None, expected_revision: None, late_recovery: false,
    };
    let a = booking::prepare_selection(&db, j.f.clock.as_ref(), &j.f.cfg, Some(&j.f.client), request(contact_a), Some("Paciente A")).await.unwrap();
    let b = booking::prepare_selection(&db, j.f.clock.as_ref(), &j.f.cfg, Some(&j.f.client), request(contact_b), Some("Paciente B")).await.unwrap();
    let (booking::Prepared::Ready(a), booking::Prepared::Ready(b)) = (a, b) else {
      panic!("Both Contacts must initially see the same available time");
    };
    let origin = j.f.cfg.public_origin.as_deref().unwrap();
    let now = j.f.clock.now_millis();
    let (a, b) = tokio::join!(
      async {
        let tx = db.conn.begin().await.unwrap();
        let result = booking::commit_selection(&tx, a, &inbox_a, Some("Paciente A"), origin, now).await.unwrap();
        tx.commit().await.unwrap();
        result
      },
      async {
        let tx = db.conn.begin().await.unwrap();
        let result = booking::commit_selection(&tx, b, &inbox_b, Some("Paciente B"), origin, now).await.unwrap();
        tx.commit().await.unwrap();
        result
      },
    );
    let results = [a, b];
    assert_eq!(results.iter().filter(|result| matches!(result, booking::Commit::Applied { .. })).count(), 1);
    assert_eq!(results.iter().filter(|result| matches!(result, booking::Commit::Stale)).count(), 1);
    let counts = db.conn.query_one(sql("SELECT (SELECT count(*) FROM appointments) AS appointments,(SELECT count(*) FROM appointment_holds WHERE active) AS holds", vec![])).await.unwrap().unwrap();
    assert_eq!(counts.try_get::<i64>("", "appointments").unwrap(), 1);
    assert_eq!(counts.try_get::<i64>("", "holds").unwrap(), 1);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn natural_preference_and_explicit_offered_time_use_the_same_booking_rules() {
  with_database(|db| async move {
    let mut j = Journey::new(&db).await;
    drop(std::mem::take(&mut j.f.app));
    let cfg = Arc::get_mut(&mut j.f.cfg).unwrap();
    cfg.claude_api_key = Some("synthetic-parser-key".to_owned());
    cfg.claude_model = Some("synthetic-model".to_owned());
    cfg.claude_test_endpoint = Some(format!("{}/v1/messages", j.base));
    let (app, waha, _) = crate::app::compose(j.f.cfg.clone(), db.clone(), j.f.clock.clone());
    j.f.app = app;
    j.waha = waha.unwrap();
    let day = chrono::DateTime::from_timestamp(j.f.clock.now(), 0).unwrap().date_naive();
    let expected = (day + chrono::Duration::days(2)).and_hms_opt(10, 0, 0).unwrap().and_utc().timestamp_millis();
    for answer in ["Quiero agendar para pasado mañana a las 10 am", "Paciente de lenguaje natural", "1", "1"] {
      j.say(&db, answer).await;
    }
    let offered = db.conn.query_one(sql("SELECT (SELECT count(*) FROM appointments) AS appointments,(SELECT count(*) FROM appointment_holds WHERE active) AS holds", vec![])).await.unwrap().unwrap();
    assert_eq!(offered.try_get::<i64>("", "appointments").unwrap(), 0, "A parsed preference is not authorization to book");
    assert_eq!(offered.try_get::<i64>("", "holds").unwrap(), 0);
    j.say(&db, "10:00").await;
    let appointment = j.latest(&db).await;
    assert_eq!(appointment.starts_at, expected);
    assert_eq!(appointment.format, "in_person");
    assert_eq!(appointment.payment, "not_required");
    j.calendar(&db).await;
    assert_eq!(store::get(&db.conn, appointment.id).await.unwrap().unwrap().calendar_sync, "synced");
    j.say(&db, "Quiero agendar para mañana a las 10 am").await;
    let total = db.conn.query_one(sql("SELECT count(*) AS n FROM appointments", vec![])).await.unwrap().unwrap();
    assert_eq!(total.try_get::<i64>("", "n").unwrap(), 1, "An invented model date cannot book or mutate an Appointment");
  }).await;
}
