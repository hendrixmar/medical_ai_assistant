use crate::{
  app::tests::reconnect,
  database::Db,
  modules::{
    appointments::booking::{self, PaymentInstructions},
    doctor::{
      tests::{self, TestClock},
      Clock,
    },
    stripe::{client::Client, config::StripeConfig},
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

const ACCOUNT_ID: &str = "acct_fixture";
const PAYMENT_URL: &str = "https://buy.stripe.com/test_link_fixture";

#[derive(Clone, Debug)]
pub(crate) struct CapturedRequest {
  pub method: String,
  pub path: String,
  pub query: HashMap<String, String>,
  pub form: HashMap<String, String>,
  pub stripe_account: Option<String>,
}

#[derive(Default)]
pub(crate) struct Provider {
  pub requests: Mutex<Vec<CapturedRequest>>,
  pub sessions: Mutex<HashMap<String, Vec<Value>>>,
  account_ready: Mutex<(bool, bool)>,
  pub unavailable: AtomicBool,
  prices: AtomicUsize,
  links: AtomicUsize,
}
impl Provider {
  pub async fn set_account_status(&self, charges_enabled: bool, details_submitted: bool) {
    *self.account_ready.lock().await = (charges_enabled, details_submitted);
  }
}

async fn handle(State(provider): State<Arc<Provider>>, request: Request<Body>) -> Response {
  let (parts, body) = request.into_parts();
  let bytes = to_bytes(body, 64 * 1024).await.unwrap();
  let query = pairs(parts.uri.query().unwrap_or_default());
  let form = pairs(std::str::from_utf8(&bytes).unwrap_or_default());
  let path = parts.uri.path().to_owned();
  let method = parts.method.to_string();
  let stripe_account = parts
    .headers
    .get("stripe-account")
    .and_then(|value| value.to_str().ok())
    .map(str::to_owned);
  provider.requests.lock().await.push(CapturedRequest {
    method: method.clone(),
    path: path.clone(),
    query: query.clone(),
    form: form.clone(),
    stripe_account,
  });
  if provider.unavailable.load(Ordering::SeqCst) {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  }
  match (method.as_str(), path.as_str()) {
    ("POST", "/v1/accounts") => Json(json!({"id":"acct_fixture"})).into_response(),
    ("POST", "/v1/account_links") => Json(json!({
      "url":"https://connect.stripe.com/setup/test_onboarding"
    }))
    .into_response(),
    ("GET", "/v1/accounts/acct_fixture") => {
      let (charges_enabled, details_submitted) = *provider.account_ready.lock().await;
      Json(json!({
        "charges_enabled":charges_enabled,
        "details_submitted":details_submitted
      }))
      .into_response()
    }
    ("POST", "/v1/prices") => {
      let id = provider.prices.fetch_add(1, Ordering::SeqCst) + 1;
      Json(json!({"id":format!("price_fixture{id}")})).into_response()
    }
    ("POST", "/v1/payment_links") => {
      let id = provider.links.fetch_add(1, Ordering::SeqCst) + 1;
      Json(json!({"id":format!("plink_fixture{id}"),"url":PAYMENT_URL})).into_response()
    }
    ("GET", "/v1/checkout/sessions") => {
      let payment_link = query
        .get("payment_link")
        .map(String::as_str)
        .unwrap_or_default();
      let data = provider
        .sessions
        .lock()
        .await
        .get(payment_link)
        .cloned()
        .unwrap_or_default();
      Json(json!({"data":data})).into_response()
    }
    ("POST", path) if path.starts_with("/v1/payment_links/") => {
      Json(json!({"id":path.rsplit('/').next().unwrap_or_default()})).into_response()
    }
    _ => StatusCode::NOT_FOUND.into_response(),
  }
}

fn pairs(encoded: &str) -> HashMap<String, String> {
  let url = reqwest::Url::parse(&format!("http://fixture.invalid/?{encoded}")).unwrap();
  url
    .query_pairs()
    .map(|(key, value)| (key.into_owned(), value.into_owned()))
    .collect()
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
  pub async fn new(db: &Db, stripe_enabled: bool) -> Self {
    let (mut cfg, clock, old_app, totp) = tests::fixture(db).await;
    let session = tests::signed_in(&old_app, &totp, clock.now()).await;
    drop(old_app);
    Self::with_session(db, &mut cfg, clock, session, stripe_enabled).await
  }

  pub async fn with_session(
    db: &Db,
    cfg: &mut crate::common::config::Config,
    clock: Arc<TestClock>,
    session: String,
    stripe_enabled: bool,
  ) -> Self {
    let provider = Arc::new(Provider::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stripe_router = Router::new().fallback(handle).with_state(provider.clone());
    let server = tokio::spawn(async move {
      axum::serve(listener, stripe_router).await.unwrap();
    });
    let mut stripe_config = StripeConfig::parse(
      Some("sk_test_synthetic_fixture"),
      cfg.public_origin.as_deref(),
    )
    .unwrap()
    .unwrap();
    stripe_config.test_endpoint = Some(format!("{base}/v1/").parse().unwrap());
    if stripe_enabled {
      Arc::get_mut(cfg).unwrap().stripe = Some(stripe_config.clone());
    }
    let (app, _, google_client, client) = crate::app::compose_with_stripe(
      cfg.clone(),
      db.clone(),
      clock.clone(),
      stripe_enabled.then_some(stripe_config),
    );
    let csrf = tests::field(
      &tests::text(tests::authenticated_get(&app, "/settings/stripe", &session).await).await,
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

  pub async fn post(&self, path: &str, origin: &str, csrf: &str) -> Response {
    tests::post(
      &self.app,
      path,
      Some(&self.session),
      Some(origin),
      &[("csrf_token", csrf)],
    )
    .await
  }

  pub async fn row(&self, db: &Db, appointment: Uuid) -> sea_orm::QueryResult {
    let pool = reconnect(db, 2).await;
    let row = pool
      .conn
      .query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM stripe_payment_links WHERE appointment_id=$1",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    pool.conn.close().await.unwrap();
    row
  }

  pub async fn set_ready(&self, db: &Db) {
    let now = self.clock.now_millis();
    db.conn
      .execute(sql(
        "INSERT INTO stripe_connection(id,account_id,charges_enabled,details_submitted,checked_at,created_at,updated_at) VALUES (1,$1,true,true,$2,$2,$2) ON CONFLICT (id) DO UPDATE SET account_id=EXCLUDED.account_id,charges_enabled=true,details_submitted=true,checked_at=EXCLUDED.checked_at,updated_at=EXCLUDED.updated_at",
        vec![ACCOUNT_ID.to_owned().into(), now.into()],
      ))
      .await
      .unwrap();
  }

  pub async fn create_contact(&self, db: &Db) -> (Uuid, String) {
    let now = self.clock.now_millis();
    let contact = Uuid::now_v7();
    let inbox_id = format!("stripe-test-{contact}");
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
    let reference = format!("STRIPE-{}", id.simple());
    db.conn
      .execute(sql(
        "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,revision,created_at,updated_at) VALUES ($1,$2,'Paciente Stripe privado',$3,'Consulta general',45,$4,'video',$5,$6,'America/Mexico_City','Consultorio de prueba',0,0,$7,$8,'not_yet','Banco de prueba','Beneficiario de prueba','012345678901234567',$9,$10,1,$11,$11)",
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

  pub async fn schedule_link(&self, db: &Db, appointment: &VideoAppointment) {
    let tx = db.conn.begin().await.unwrap();
    crate::modules::stripe::store::schedule_link(
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

  pub async fn seed_active_link(
    &self,
    db: &Db,
    appointment: &VideoAppointment,
    link_id: &str,
    deadline: i64,
  ) {
    db.conn
      .execute(sql(
        "INSERT INTO stripe_payment_links(appointment_id,revision,account_id,price_id,link_id,url,amount_centavos,created_at,deactivate_at,outcome,next_check_at) VALUES ($1,1,$2,'price_seed',$3,$4,$5,$6,$7,'active',$6)",
        vec![
          appointment.id.into(),
          ACCOUNT_ID.to_owned().into(),
          link_id.to_owned().into(),
          PAYMENT_URL.to_owned().into(),
          appointment.amount_centavos.into(),
          self.clock.now_millis().into(),
          deadline.into(),
        ],
      ))
      .await
      .unwrap();
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

  pub async fn set_sessions(&self, link_id: &str, sessions: Vec<Value>) {
    self
      .provider
      .sessions
      .lock()
      .await
      .insert(link_id.to_owned(), sessions);
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

pub(crate) fn paid_session(id: &str, amount_total: i64, currency: &str) -> Value {
  json!({
    "id":id,
    "status":"complete",
    "payment_status":"paid",
    "currency":currency,
    "amount_total":amount_total
  })
}

pub(crate) const ACCOUNT: &str = ACCOUNT_ID;
pub(crate) const CHECKOUT_URL: &str = PAYMENT_URL;
