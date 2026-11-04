use crate::{
  app::tests::reconnect,
  database::Db,
  modules::{
    appointments::booking::{self, PaymentInstructions},
    doctor::{
      tests::{self, TestClock},
      Clock,
    },
    mercado_pago::{client::Client, config::MercadoPagoConfig, store},
    whatsapp::inbox::sql,
  },
};
use axum::{
  body::{to_bytes, Body},
  extract::State,
  http::{Request, StatusCode},
  response::{IntoResponse, Response},
  Json, Router,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use std::{
  collections::HashMap,
  sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
  },
};
use tokio::sync::Mutex;
use uuid::Uuid;

pub(crate) const USER_ID: i64 = 4242;
pub(crate) const CHECKOUT_URL: &str =
  "https://www.mercadopago.com.mx/checkout/v1/redirect?pref_id=fixture";

#[derive(Clone, Debug)]
pub(crate) struct CapturedRequest {
  pub method: String,
  pub path: String,
  pub query: HashMap<String, String>,
  pub body: Value,
  pub idempotency_key: Option<String>,
}

#[derive(Default)]
pub(crate) struct Provider {
  pub requests: Mutex<Vec<CapturedRequest>>,
  /// external_reference -> payment ids returned by search.
  pub searches: Mutex<HashMap<String, Vec<i64>>>,
  /// payment id -> full payment body.
  pub payments: Mutex<HashMap<i64, Value>>,
  pub unavailable: AtomicBool,
  pub unauthorized: AtomicBool,
  pub refund_rejected: AtomicBool,
  preferences: AtomicUsize,
}

async fn handle(State(provider): State<Arc<Provider>>, request: Request<Body>) -> Response {
  let (parts, body) = request.into_parts();
  let bytes = to_bytes(body, 64 * 1024).await.unwrap();
  let query: HashMap<String, String> = reqwest::Url::parse(&format!(
    "http://fixture.invalid/?{}",
    parts.uri.query().unwrap_or_default()
  ))
  .unwrap()
  .query_pairs()
  .into_owned()
  .collect();
  let path = parts.uri.path().to_owned();
  let method = parts.method.to_string();
  provider.requests.lock().await.push(CapturedRequest {
    method: method.clone(),
    path: path.clone(),
    query: query.clone(),
    body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    idempotency_key: parts
      .headers
      .get("x-idempotency-key")
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned),
  });
  if provider.unavailable.load(Ordering::SeqCst) {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  }
  if provider.unauthorized.load(Ordering::SeqCst) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
  match (method.as_str(), segments.as_slice()) {
    ("GET", ["users", "me"]) => Json(json!({"id": USER_ID})).into_response(),
    ("POST", ["checkout", "preferences"]) => {
      let id = provider.preferences.fetch_add(1, Ordering::SeqCst) + 1;
      Json(json!({"id": format!("{USER_ID}-pref{id}"), "init_point": CHECKOUT_URL})).into_response()
    }
    ("PUT", ["checkout", "preferences", id]) => Json(json!({"id": id})).into_response(),
    ("GET", ["v1", "payments", "search"]) => {
      let ids = provider
        .searches
        .lock()
        .await
        .get(
          query
            .get("external_reference")
            .map(String::as_str)
            .unwrap_or_default(),
        )
        .cloned()
        .unwrap_or_default();
      let payments = provider.payments.lock().await;
      let results: Vec<Value> = ids
        .iter()
        .filter_map(|id| payments.get(id).cloned())
        .collect();
      Json(json!({"results": results})).into_response()
    }
    ("GET", ["v1", "payments", id]) => {
      let payment = match id.parse::<i64>() {
        Ok(id) => provider.payments.lock().await.get(&id).cloned(),
        Err(_) => None,
      };
      match payment {
        Some(payment) => Json(payment).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
      }
    }
    ("POST", ["v1", "payments", id, "refunds"]) => {
      if provider.refund_rejected.load(Ordering::SeqCst) {
        return StatusCode::BAD_REQUEST.into_response();
      }
      let id: i64 = id.parse().unwrap();
      if let Some(payment) = provider.payments.lock().await.get_mut(&id) {
        payment["status"] = json!("refunded");
      }
      Json(json!({"id": 900_000 + id, "status": "approved"})).into_response()
    }
    _ => StatusCode::NOT_FOUND.into_response(),
  }
}

pub(crate) struct Fixture {
  pub cfg: crate::common::config::Config,
  pub clock: Arc<TestClock>,
  pub app: Router,
  pub client: Option<Client>,
  pub google_client: Option<crate::modules::google_calendar::client::Client>,
  pub session: String,
  pub csrf: String,
  pub provider: Arc<Provider>,
  appointments_created: AtomicUsize,
  server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
  fn drop(&mut self) {
    self.server.abort();
  }
}

impl Fixture {
  pub async fn new(db: &Db, enabled: bool) -> Self {
    let (mut cfg, clock, old_app, totp) = tests::fixture(db).await;
    let session = tests::signed_in(&old_app, &totp, clock.now()).await;
    drop(old_app);
    Self::with_session(db, &mut cfg, clock, session, enabled).await
  }

  pub async fn with_session(
    db: &Db,
    cfg: &mut crate::common::config::Config,
    clock: Arc<TestClock>,
    session: String,
    enabled: bool,
  ) -> Self {
    let provider = Arc::new(Provider::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let router = Router::new().fallback(handle).with_state(provider.clone());
    let server = tokio::spawn(async move {
      axum::serve(listener, router).await.unwrap();
    });
    let mut config = MercadoPagoConfig::parse(
      Some("APP_USR-synthetic-fixture"),
      cfg.public_origin.as_deref(),
    )
    .unwrap()
    .unwrap();
    config.test_endpoint = Some(base.parse().unwrap());
    if enabled {
      Arc::get_mut(cfg).unwrap().mercado_pago = Some(config.clone());
    }
    let (app, _, google_client, client) = crate::app::compose_with_mercado_pago(
      cfg.clone(),
      db.clone(),
      clock.clone(),
      enabled.then_some(config),
    );
    let csrf = tests::field(
      &tests::text(tests::authenticated_get(&app, "/settings/mercado-pago", &session).await).await,
      "csrf_token",
    );
    Self {
      cfg: cfg.clone(),
      clock,
      app,
      client,
      session,
      csrf,
      provider,
      google_client,
      appointments_created: AtomicUsize::new(0),
      server,
    }
  }

  pub async fn post(&self, path: &str, csrf: &str) -> Response {
    tests::post(
      &self.app,
      path,
      Some(&self.session),
      Some("https://practice.test"),
      &[("csrf_token", csrf)],
    )
    .await
  }

  pub async fn preference(&self, db: &Db, appointment: Uuid) -> sea_orm::QueryResult {
    let pool = reconnect(db, 2).await;
    let row = pool
      .conn
      .query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM mercado_pago_preferences WHERE appointment_id=$1",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    pool.conn.close().await.unwrap();
    row
  }

  pub async fn payment_row(&self, db: &Db, payment_id: i64) -> sea_orm::QueryResult {
    db.conn
      .query_one(sql(
        "SELECT * FROM mercado_pago_payments WHERE payment_id=$1",
        vec![payment_id.into()],
      ))
      .await
      .unwrap()
      .unwrap()
  }

  /// The token was already checked and accepted.
  pub async fn set_ready(&self, db: &Db) {
    let now = self.clock.now_millis();
    db.conn
      .execute(sql(
        "INSERT INTO mercado_pago_account(id,user_id,checked_at,notice,created_at,updated_at) VALUES (1,$1,$2,NULL,$2,$2) ON CONFLICT (id) DO UPDATE SET user_id=EXCLUDED.user_id,checked_at=EXCLUDED.checked_at,notice=NULL",
        vec![USER_ID.into(), now.into()],
      ))
      .await
      .unwrap();
  }

  pub async fn create_contact(&self, db: &Db) -> (Uuid, String) {
    let now = self.clock.now_millis();
    let contact = Uuid::now_v7();
    let inbox_id = format!("mp-test-{contact}");
    let digits = format!("1555{:08}", contact.as_u128() % 100_000_000);
    let jid = format!("{digits}@c.us");
    db.conn
      .execute(sql(
        "INSERT INTO contacts(id,created_at) VALUES ($1,$2)",
        vec![contact.into(), now.into()],
      ))
      .await
      .unwrap();
    db.conn
      .execute(sql(
        "INSERT INTO inbox_messages(message_id,original_chat_identity,resolved_phone_identity,server_timestamp,arrived_at,raw_envelope,contact_id) VALUES ($1,$2,$2,$3,to_timestamp($4::double precision/1000.0),$5,$6)",
        vec![
          inbox_id.clone().into(),
          jid.clone().into(),
          now.div_euclid(1000).into(),
          now.into(),
          b"{}".to_vec().into(),
          contact.into(),
        ],
      ))
      .await
      .unwrap();
    db.conn
      .execute(sql(
        "INSERT INTO contact_aliases(jid,contact_id,inbox_message_id) VALUES ($1,$2,$3)",
        vec![jid.into(), contact.into(), inbox_id.clone().into()],
      ))
      .await
      .unwrap();
    (contact, inbox_id)
  }

  pub async fn video_appointment(
    &self,
    db: &Db,
    status: &str,
    payment: &str,
    deadline: i64,
  ) -> VideoAppointment {
    let now = self.clock.now_millis();
    let id = Uuid::now_v7();
    let (contact, _inbox_id) = self.create_contact(db).await;
    let start =
      now + (self.appointments_created.fetch_add(1, Ordering::SeqCst) + 3) as i64 * 86_400_000;
    let amount_centavos = 125_500;
    let reference = format!("MP-{}", id.simple());
    db.conn
      .execute(sql(
        "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,revision,created_at,updated_at) VALUES ($1,$2,'Paciente MP privado',$3,'Consulta general',45,$4,'video',$5,$6,'America/Mexico_City','Consultorio de prueba',0,0,$7,$8,'not_yet','Banco de prueba','Beneficiario de prueba','012345678901234567',$9,$10,1,$11,$11)",
        vec![
          id.into(),
          contact.into(),
          Uuid::now_v7().into(),
          amount_centavos.into(),
          start.into(),
          (start + 45 * 60_000).into(),
          status.to_owned().into(),
          payment.to_owned().into(),
          reference.clone().into(),
          deadline.into(),
          now.into(),
        ],
      ))
      .await
      .unwrap();
    db.conn
      .execute(sql(
        "INSERT INTO appointment_holds(id,appointment_id,starts_at,ends_at,active,expires_at) VALUES ($1,$2,$3,$4,true,$5)",
        vec![
          Uuid::now_v7().into(),
          id.into(),
          start.into(),
          (start + 45 * 60_000).into(),
          deadline.into(),
        ],
      ))
      .await
      .unwrap();
    VideoAppointment {
      id,
      contact,
      reference,
      amount_centavos,
      deadline,
    }
  }

  pub async fn queue_transfer_instructions(&self, db: &Db, appointment: &VideoAppointment) {
    let tx = db.conn.begin().await.unwrap();
    booking::enqueue_payment_instructions(
      &tx,
      PaymentInstructions {
        appointment: appointment.id,
        revision: 1,
        contact: appointment.contact,
        deadline: appointment.deadline,
        price_centavos: appointment.amount_centavos,
        bank: "Banco de prueba",
        beneficiary: "Beneficiario de prueba",
        clabe: "012345678901234567",
        reference: &appointment.reference,
        timezone: "America/Mexico_City",
      },
      "https://practice.test",
      self.clock.now_millis(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
  }

  pub async fn schedule_preference(&self, db: &Db, appointment: &VideoAppointment) {
    let tx = db.conn.begin().await.unwrap();
    store::schedule_preference(
      &tx,
      appointment.id,
      1,
      appointment.amount_centavos,
      appointment.deadline,
      self.clock.now_millis(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
  }

  pub async fn seed_active_preference(&self, db: &Db, appointment: &VideoAppointment) -> String {
    let preference_id = format!("{USER_ID}-seed-{}", appointment.id.simple());
    db.conn
      .execute(sql(
        "INSERT INTO mercado_pago_preferences(appointment_id,revision,external_reference,preference_id,url,amount_centavos,expires_at,outcome,next_check_at,created_at) VALUES ($1,1,$2,$3,$4,$5,$6,'active',$7,$7)",
        vec![
          appointment.id.into(),
          store::external_reference(appointment.id, 1).into(),
          preference_id.clone().into(),
          CHECKOUT_URL.into(),
          appointment.amount_centavos.into(),
          appointment.deadline.into(),
          self.clock.now_millis().into(),
        ],
      ))
      .await
      .unwrap();
    preference_id
  }

  pub async fn add_expiry_job(&self, db: &Db, appointment: Uuid, due: i64) {
    let tx = db.conn.begin().await.unwrap();
    crate::modules::appointments::store::job(
      &tx,
      appointment,
      "expiry",
      due,
      &format!("{appointment}:1:expiry:deadline"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
  }

  /// Makes `payment` visible both to search (by its listed reference) and to `GET /v1/payments/{id}`.
  pub async fn add_payment(&self, listed_under: &str, payment: Value) {
    let id = payment["id"].as_i64().unwrap();
    self
      .provider
      .searches
      .lock()
      .await
      .entry(listed_under.to_owned())
      .or_default()
      .push(id);
    self.provider.payments.lock().await.insert(id, payment);
  }

  pub async fn set_payment_status(&self, id: i64, status: &str) {
    self.provider.payments.lock().await.get_mut(&id).unwrap()["status"] = json!(status);
  }

  pub async fn requests_to(&self, method: &str, path: &str) -> Vec<CapturedRequest> {
    self
      .provider
      .requests
      .lock()
      .await
      .iter()
      .filter(|request| request.method == method && request.path == path)
      .cloned()
      .collect()
  }
}

#[derive(Clone)]
pub(crate) struct VideoAppointment {
  pub id: Uuid,
  pub contact: Uuid,
  pub reference: String,
  pub amount_centavos: i64,
  pub deadline: i64,
}

impl VideoAppointment {
  pub fn external_reference(&self) -> String {
    store::external_reference(self.id, 1)
  }
}

pub(crate) fn payment(
  id: i64,
  status: &str,
  amount: Value,
  currency: &str,
  external_reference: &str,
  collector: i64,
) -> Value {
  json!({
    "id": id,
    "status": status,
    "status_detail": "accredited",
    "currency_id": currency,
    "transaction_amount": amount,
    "external_reference": external_reference,
    "collector_id": collector,
  })
}
