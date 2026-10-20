use super::{intake::PendingReceipt, ReceiveOutcome};
use crate::{
  app::tests::with_database,
  database::Db,
  modules::{
    doctor::{
      tests::{authenticated_get, get as unauthenticated_get, post as doctor_post, text},
      Clock,
    },
    google_calendar::tests::fixture::Fixture,
    whatsapp::{client::Client, config::WahaConfig, inbox::sql},
  },
};
use axum::{
  body::to_bytes,
  http::{header, StatusCode},
  routing::get,
  Router,
};
use sea_orm::ConnectionTrait;
use std::sync::Arc;
use tokio::task::JoinHandle;
use uuid::Uuid;

const HMAC_KEY: &str = "synthetic-payment-receipt-HMAC-key-at-least-32";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\0\0\0\0\0";
const ORIGIN: &str = "https://practice.test";
const DAY_MS: i64 = 86_400_000;

struct Harness {
  fixture: Fixture,
  waha: Client,
  media_origin: String,
  media_server: JoinHandle<()>,
}

impl Drop for Harness {
  fn drop(&mut self) {
    self.media_server.abort();
  }
}

impl Harness {
  async fn new(db: &Db) -> Self {
    let mut fixture = Fixture::new(db).await;
    drop(std::mem::take(&mut fixture.app));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let media_origin = format!("http://{}", listener.local_addr().unwrap());
    let provider = Router::new().route(
      "/media/receipt.png",
      get(|| async { ([("content-type", "image/png")], PNG) }),
    );
    let media_server = tokio::spawn(async move {
      axum::serve(listener, provider).await.unwrap();
    });

    Arc::get_mut(&mut fixture.cfg).unwrap().waha = WahaConfig::parse(
      Some(&media_origin),
      Some("synthetic-api-key"),
      Some(HMAC_KEY),
    )
    .unwrap();
    let (app, waha, _) =
      crate::app::compose(fixture.cfg.clone(), db.clone(), fixture.clock.clone());
    fixture.app = app;

    Self {
      fixture,
      waha: waha.unwrap(),
      media_origin,
      media_server,
    }
  }
}

struct Incoming {
  contact: Uuid,
  message_id: String,
  sent_at: i64,
  arrived_at: i64,
}

async fn incoming(db: &Db, now: i64, index: u32, sent_at: i64, arrived_at: i64) -> Incoming {
  let contact = Uuid::now_v7();
  let message_id = format!("receipt_{}", Uuid::now_v7().simple());
  let identity = format!("15550100{index:03}@c.us");
  db.conn
    .execute(sql(
      "INSERT INTO contacts(id,created_at) VALUES ($1,$2)",
      vec![contact.into(), now.into()],
    ))
    .await
    .unwrap();
  db.conn
    .execute(sql(
      "INSERT INTO inbox_messages(message_id,original_chat_identity,resolved_phone_identity,server_timestamp,arrived_at,raw_envelope,contact_id) VALUES ($1,$2,$2,$3,now(),$4,$5)",
      vec![
        message_id.clone().into(),
        identity.clone().into(),
        (sent_at / 1000).into(),
        b"{}".to_vec().into(),
        contact.into(),
      ],
    ))
    .await
    .unwrap();
  db.conn
    .execute(sql(
      "INSERT INTO contact_aliases(jid,contact_id,inbox_message_id) VALUES ($1,$2,$3)",
      vec![identity.into(), contact.into(), message_id.clone().into()],
    ))
    .await
    .unwrap();
  db.conn
    .execute(sql(
      "INSERT INTO receipt_intake_state(inbox_message_id,state,sent_at,arrived_at,updated_at) VALUES ($1,'pending',$2,$3,$4)",
      vec![
        message_id.clone().into(),
        sent_at.into(),
        arrived_at.into(),
        now.into(),
      ],
    ))
    .await
    .unwrap();

  Incoming {
    contact,
    message_id,
    sent_at,
    arrived_at,
  }
}

async fn appointment(
  db: &Db,
  contact: Uuid,
  now: i64,
  slot: u32,
  status: &str,
  receipt_deadline: i64,
  hold_expiry: Option<i64>,
) -> Uuid {
  let id = Uuid::now_v7();
  let starts_at = now + (3 + i64::from(slot)) * DAY_MS;
  let ends_at = starts_at + 30 * 60_000;
  let expired_at = (status == "expired").then_some(now.saturating_sub(1));
  db.conn
    .execute(sql(
      "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,booking_calendar,google_generation,google_selection_version,event_id,event_etag,meet_url,revision,attendance_confirmed_at,cancelled_at,expired_at,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,30,150000,'video',$6,$7,'UTC','Consultorio de prueba',0,0,$8,'awaiting_receipt','not_yet','Banco de prueba','Beneficiario de prueba','012345678901234567',$9,$10,NULL,NULL,NULL,NULL,NULL,NULL,1,NULL,NULL,$11,$12,$12)",
      vec![
        id.into(),
        contact.into(),
        format!("Synthetic Patient {slot}").into(),
        Uuid::now_v7().into(),
        "Consulta de prueba".into(),
        starts_at.into(),
        ends_at.into(),
        status.to_owned().into(),
        format!("PAY{slot:03}").into(),
        receipt_deadline.into(),
        expired_at.into(),
        now.into(),
      ],
    ))
    .await
    .unwrap();

  if let Some(expires_at) = hold_expiry {
    db.conn
      .execute(sql(
        "INSERT INTO appointment_holds(id,appointment_id,starts_at,ends_at,expires_at) VALUES ($1,$2,$3,$4,$5)",
        vec![
          Uuid::now_v7().into(),
          id.into(),
          starts_at.into(),
          ends_at.into(),
          expires_at.into(),
        ],
      ))
      .await
      .unwrap();
  }
  id
}

async fn receive(harness: &Harness, db: &Db, incoming: &Incoming) -> ReceiveOutcome {
  super::receive(
    db,
    Some(&harness.waha),
    &incoming.message_id,
    &PendingReceipt {
      contact: Some(incoming.contact),
      url: Some(format!("{}/media/receipt.png", harness.media_origin)),
      mimetype: Some("image/png".into()),
      sent_at: incoming.sent_at,
      arrived_at: incoming.arrived_at,
    },
    harness.fixture.clock.now_millis(),
    Some(ORIGIN),
  )
  .await
  .unwrap()
}

fn attached(outcome: ReceiveOutcome) -> Uuid {
  match outcome {
    ReceiveOutcome::Attached { receipt_id, .. } => receipt_id,
    other => panic!("expected one Receipt attached to its Appointment, got {other:?}"),
  }
}

fn receipt_path(receipt: Uuid) -> String {
  format!("/dashboard/payments/receipts/{receipt}")
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn receipt_review_and_download_require_doctor_session_csrf_and_exact_origin() {
  with_database(|db| async move {
    let harness = Harness::new(&db).await;
    let now = harness.fixture.clock.now_millis();
    let incoming = incoming(&db, now, 1, now - 2_000, now - 1_000).await;
    let appointment = appointment(
      &db,
      incoming.contact,
      now,
      1,
      "awaiting_payment",
      now + 60 * 60_000,
      Some(now + 2 * 60 * 60_000),
    )
    .await;
    let receipt = attached(receive(&harness, &db, &incoming).await);
    let path = receipt_path(receipt);
    let file_path = format!("{path}/file");

    for target in [&path, &file_path] {
      let response = unauthenticated_get(&harness.fixture.app, target).await;
      assert_eq!(response.status(), StatusCode::SEE_OTHER);
      assert_eq!(response.headers()[header::LOCATION].to_str().unwrap().split('?').next(), Some("/login"));
      assert!(to_bytes(response.into_body(), 1024).await.unwrap().is_empty(), "An unauthenticated redirect must not carry Receipt data");
    }
    assert_eq!(
      doctor_post(&harness.fixture.app, &format!("{path}/verify"), None, Some(ORIGIN), &[])
        .await
        .status(),
      StatusCode::UNAUTHORIZED
    );
    assert_eq!(
      doctor_post(
        &harness.fixture.app,
        &format!("{path}/verify"),
        Some(&harness.fixture.session),
        Some(ORIGIN),
        &[],
      )
      .await
      .status(),
      StatusCode::FORBIDDEN,
      "a Doctor session without the form CSRF token is insufficient"
    );
    assert_eq!(
      doctor_post(
        &harness.fixture.app,
        &format!("{path}/verify"),
        Some(&harness.fixture.session),
        Some("https://other.test"),
        &[("csrf_token", &harness.fixture.csrf)],
      )
      .await
      .status(),
      StatusCode::FORBIDDEN,
      "a valid CSRF token from another Origin is insufficient"
    );

    let page = authenticated_get(&harness.fixture.app, &path, &harness.fixture.session).await;
    assert_eq!(page.status(), StatusCode::OK);

    let file = authenticated_get(&harness.fixture.app, &file_path, &harness.fixture.session).await;
    assert_eq!(file.status(), StatusCode::OK);
    assert_eq!(file.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(
      file.headers()[header::CONTENT_DISPOSITION],
      "attachment; filename=\"comprobante\""
    );
    assert_eq!(file.headers()["x-content-type-options"], "nosniff");
    assert_eq!(file.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(to_bytes(file.into_body(), 1024).await.unwrap().as_ref(), PNG);
    let unchanged = db.conn.query_one(sql(
      "SELECT a.payment,r.reviewed_at FROM appointments a JOIN receipts r ON r.appointment_id=a.id WHERE r.id=$1",
      vec![receipt.into()],
    )).await.unwrap().unwrap();
    assert_eq!(unchanged.try_get::<String>("", "payment").unwrap(), "under_review");
    assert!(unchanged.try_get::<Option<i64>>("", "reviewed_at").unwrap().is_none(), "Rejected authentication, CSRF and Origin attempts cannot verify a payment");

    assert_eq!(
      harness.fixture.post(&format!("{path}/verify"), &[]).await.status(),
      StatusCode::SEE_OTHER
    );
    let row = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.payment,r.review_outcome FROM appointments a JOIN receipts r ON r.appointment_id=a.id WHERE r.id=$1",
        vec![receipt.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "confirmed");
    assert_eq!(row.try_get::<String>("", "payment").unwrap(), "verified");
    assert_eq!(
      row.try_get::<Option<String>>("", "review_outcome").unwrap().as_deref(),
      Some("verified")
    );
    let meet: Option<String> = db
      .conn
      .query_one(sql("SELECT meet_url FROM appointments WHERE id=$1", vec![appointment.into()]))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "meet_url")
      .unwrap();
    assert!(meet.is_none(), "Payment Verification alone does not invent a Meet URL");
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn competing_verify_and_reject_keep_one_completed_review() {
  with_database(|db| async move {
    let harness = Harness::new(&db).await;
    let now = harness.fixture.clock.now_millis();
    let incoming = incoming(&db, now, 2, now - 2_000, now - 1_000).await;
    let appointment = appointment(
      &db,
      incoming.contact,
      now,
      2,
      "awaiting_payment",
      now + 60 * 60_000,
      Some(now + 2 * 60 * 60_000),
    )
    .await;
    let receipt = attached(receive(&harness, &db, &incoming).await);
    let path = receipt_path(receipt);
    let verify_path = format!("{path}/verify");
    let reject_path = format!("{path}/reject");
    let verify_fields = [("csrf_token", harness.fixture.csrf.as_str())];
    let reject_fields = [("csrf_token", harness.fixture.csrf.as_str()), ("choice", "cancel")];
    let verify = doctor_post(
      &harness.fixture.app,
      &verify_path,
      Some(&harness.fixture.session),
      Some(ORIGIN),
      &verify_fields,
    );
    let reject = doctor_post(
      &harness.fixture.app,
      &reject_path,
      Some(&harness.fixture.session),
      Some(ORIGIN),
      &reject_fields,
    );
    let (verified, rejected) = tokio::join!(verify, reject);
    assert_eq!(verified.status(), StatusCode::SEE_OTHER);
    assert_eq!(rejected.status(), StatusCode::SEE_OTHER);

    let row = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.payment,r.review_outcome FROM appointments a JOIN receipts r ON r.appointment_id=a.id WHERE r.id=$1",
        vec![receipt.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    let status: String = row.try_get("", "status").unwrap();
    let payment: String = row.try_get("", "payment").unwrap();
    let outcome: String = row.try_get("", "review_outcome").unwrap();
    assert!(
      (status == "confirmed" && payment == "verified" && outcome == "verified")
        || (status == "cancelled" && payment == "rejected" && outcome == "cancelled"),
      "the winning Doctor decision must leave its matching terminal state"
    );

    let decisions = db
      .conn
      .query_one(sql(
        "SELECT count(*) AS n FROM appointment_history WHERE appointment_id=$1 AND kind IN ('payment_verified','payment_rejected')",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get::<i64>("", "n")
      .unwrap();
    assert_eq!(decisions, 1);
    let outbox_before = db
      .conn
      .query_one(sql(
        "SELECT count(*) AS n FROM outbox_messages WHERE appointment_id=$1 AND purpose='status'",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get::<i64>("", "n")
      .unwrap();
    assert_eq!(outbox_before, 2, "one receipt notice and one winning decision notice");

    let opposite = if outcome == "verified" {
      harness
        .fixture
        .post(&format!("{path}/reject"), &[("choice", "cancel")])
        .await
    } else {
      harness.fixture.post(&format!("{path}/verify"), &[]).await
    };
    assert_eq!(opposite.status(), StatusCode::SEE_OTHER);
    let unchanged = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.payment,r.review_outcome FROM appointments a JOIN receipts r ON r.appointment_id=a.id WHERE r.id=$1",
        vec![receipt.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(unchanged.try_get::<String>("", "status").unwrap(), status);
    assert_eq!(unchanged.try_get::<String>("", "payment").unwrap(), payment);
    assert_eq!(unchanged.try_get::<String>("", "review_outcome").unwrap(), outcome);
    let outbox_after = db
      .conn
      .query_one(sql(
        "SELECT count(*) AS n FROM outbox_messages WHERE appointment_id=$1 AND purpose='status'",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get::<i64>("", "n")
      .unwrap();
    assert_eq!(outbox_after, outbox_before, "a later decision cannot enqueue a second outcome");
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn receipt_selection_cannot_attach_a_different_contacts_appointment() {
  with_database(|db| async move {
    let harness = Harness::new(&db).await;
    let now = harness.fixture.clock.now_millis();
    let owner = incoming(&db, now, 3, now - 2_000, now - 1_000).await;
    let first = appointment(
      &db,
      owner.contact,
      now,
      3,
      "awaiting_payment",
      now + 60 * 60_000,
      Some(now + 2 * 60 * 60_000),
    )
    .await;
    appointment(
      &db,
      owner.contact,
      now,
      4,
      "awaiting_payment",
      now + 60 * 60_000,
      Some(now + 2 * 60 * 60_000),
    )
    .await;
    let other = incoming(&db, now, 4, now - 2_000, now - 1_000).await;
    let foreign_appointment = appointment(
      &db,
      other.contact,
      now,
      5,
      "awaiting_payment",
      now + 60 * 60_000,
      Some(now + 2 * 60 * 60_000),
    )
    .await;

    let receipt = match receive(&harness, &db, &owner).await {
      ReceiveOutcome::AwaitingSelection {
        receipt_id,
        candidate_count: 2,
      } => receipt_id,
      outcome => panic!("expected an explicit two-Appointment selection, got {outcome:?}"),
    };
    let path = receipt_path(receipt);
    let page = authenticated_get(&harness.fixture.app, &path, &harness.fixture.session).await;
    assert_eq!(page.status(), StatusCode::OK);
    let html = text(page).await;
    assert!(html.contains("El comprobante no se asociará hasta que usted seleccione"));
    assert!(!html.contains("Synthetic Patient 5"));

    let foreign_id = foreign_appointment.to_string();
    assert_eq!(
      harness
        .fixture
        .post(
          &format!("{path}/association"),
          &[("appointment_id", &foreign_id)],
        )
        .await
        .status(),
      StatusCode::SEE_OTHER
    );
    let row = db
      .conn
      .query_one(sql("SELECT appointment_id FROM receipts WHERE id=$1", vec![receipt.into()]))
      .await
      .unwrap()
      .unwrap();
    assert!(row.try_get::<Option<Uuid>>("", "appointment_id").unwrap().is_none());
    let foreign_state = db
      .conn
      .query_one(sql(
        "SELECT status,payment FROM appointments WHERE id=$1",
        vec![foreign_appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(foreign_state.try_get::<String>("", "status").unwrap(), "awaiting_payment");
    assert_eq!(foreign_state.try_get::<String>("", "payment").unwrap(), "awaiting_receipt");
    let candidates = db
      .conn
      .query_one(sql(
        "SELECT count(*) AS n FROM receipt_candidates WHERE receipt_id=$1 AND appointment_id IN ($2,$3)",
        vec![receipt.into(), first.into(), foreign_appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get::<i64>("", "n")
      .unwrap();
    assert_eq!(candidates, 1, "only the Contact's eligible Appointment remains a candidate");
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn late_receipts_need_an_explicit_outcome_and_never_restore_the_expired_hold() {
  with_database(|db| async move {
    let harness = Harness::new(&db).await;
    let now = harness.fixture.clock.now_millis();
    let mut late_cases = Vec::new();
    for index in 6..=8 {
      let incoming = incoming(&db, now, index, now - 120_000, now - 1_000).await;
      let appointment = appointment(
        &db,
        incoming.contact,
        now,
        index,
        "expired",
        now - 60_000,
        None,
      )
      .await;
      let receipt = attached(receive(&harness, &db, &incoming).await);
      late_cases.push((appointment, receipt));
    }
    let (clarification, refund, recovery) = (late_cases[0], late_cases[1], late_cases[2]);
    let path = receipt_path(clarification.1);
    let page = authenticated_get(&harness.fixture.app, &path, &harness.fixture.session).await;
    assert_eq!(page.status(), StatusCode::OK);
    let html = text(page).await;
    assert!(html.contains("Enviado a tiempo según WhatsApp"));
    assert!(html.contains("Llegó después del vencimiento del horario"));
    assert!(html.contains("Pago verificado: ofrecer nuevos horarios"));
    assert!(html.contains("Pago verificado: mantener vencida y gestionar reembolso manual"));
    assert!(html.contains("Fondos ausentes o poco claros: pedir aclaración"));
    assert!(!html.contains("Confirmar pago recibido"));

    assert_eq!(
      harness.fixture.post(&format!("{path}/verify"), &[]).await.status(),
      StatusCode::SEE_OTHER
    );
    let unresolved = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.payment,a.event_id,a.meet_url,r.late,r.reviewed_at,r.review_outcome FROM appointments a JOIN receipts r ON r.appointment_id=a.id WHERE r.id=$1",
        vec![clarification.1.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(unresolved.try_get::<String>("", "status").unwrap(), "expired");
    assert_eq!(unresolved.try_get::<String>("", "payment").unwrap(), "awaiting_receipt");
    assert!(unresolved.try_get::<Option<String>>("", "event_id").unwrap().is_none());
    assert!(unresolved.try_get::<Option<String>>("", "meet_url").unwrap().is_none());
    assert!(unresolved.try_get::<Option<i64>>("", "reviewed_at").unwrap().is_none());
    assert!(unresolved.try_get::<Option<String>>("", "review_outcome").unwrap().is_none());
    assert_eq!(unresolved.try_get::<Option<bool>>("", "late").unwrap(), Some(true));

    assert_eq!(
      harness
        .fixture
        .post(
          &format!("{path}/late-outcome"),
          &[("outcome", "clarification")],
        )
        .await
        .status(),
      StatusCode::SEE_OTHER
    );
    let clarified = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.payment,r.reviewed_at,r.clarification_requested_at FROM appointments a JOIN receipts r ON r.appointment_id=a.id WHERE r.id=$1",
        vec![clarification.1.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(clarified.try_get::<String>("", "status").unwrap(), "expired");
    assert_eq!(clarified.try_get::<String>("", "payment").unwrap(), "awaiting_receipt");
    assert!(clarified.try_get::<Option<i64>>("", "reviewed_at").unwrap().is_none());
    assert!(clarified
      .try_get::<Option<i64>>("", "clarification_requested_at")
      .unwrap()
      .is_some());

    let refund_path = receipt_path(refund.1);
    assert_eq!(
      harness
        .fixture
        .post(
          &format!("{refund_path}/late-outcome"),
          &[("outcome", "verified_refund")],
        )
        .await
        .status(),
      StatusCode::SEE_OTHER
    );
    let refunded = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.payment,a.event_id,a.meet_url,r.review_outcome FROM appointments a JOIN receipts r ON r.appointment_id=a.id WHERE r.id=$1",
        vec![refund.1.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(refunded.try_get::<String>("", "status").unwrap(), "expired");
    assert_eq!(refunded.try_get::<String>("", "payment").unwrap(), "verified");
    assert_eq!(
      refunded.try_get::<Option<String>>("", "review_outcome").unwrap().as_deref(),
      Some("late_verified_refund")
    );
    assert!(refunded.try_get::<Option<String>>("", "event_id").unwrap().is_none());
    assert!(refunded.try_get::<Option<String>>("", "meet_url").unwrap().is_none());
    let refund_attention = db
      .conn
      .query_one(sql(
        "SELECT count(*) AS n FROM attention_items WHERE appointment_id=$1 AND kind='refund_due' AND resolved_at IS NULL",
        vec![refund.0.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get::<i64>("", "n")
      .unwrap();
    assert_eq!(refund_attention, 1);

    let recovery_path = receipt_path(recovery.1);
    assert_eq!(
      harness
        .fixture
        .post(
          &format!("{recovery_path}/late-outcome"),
          &[("outcome", "verified_recovery")],
        )
        .await
        .status(),
      StatusCode::SEE_OTHER
    );
    let recovered = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.payment,a.event_id,a.meet_url,r.review_outcome,(SELECT count(*) FROM appointment_holds h WHERE h.appointment_id=a.id AND h.active) AS active_holds,(SELECT stage FROM conversation_state c WHERE c.contact_id=a.contact_id) AS stage,(SELECT data->>'appointment_id' FROM conversation_state c WHERE c.contact_id=a.contact_id) AS recovery_appointment FROM appointments a JOIN receipts r ON r.appointment_id=a.id WHERE r.id=$1",
        vec![recovery.1.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(recovered.try_get::<String>("", "status").unwrap(), "expired");
    assert_eq!(recovered.try_get::<String>("", "payment").unwrap(), "verified");
    assert_eq!(
      recovered.try_get::<Option<String>>("", "review_outcome").unwrap().as_deref(),
      Some("late_verified_recovery")
    );
    assert!(recovered.try_get::<Option<String>>("", "event_id").unwrap().is_none());
    assert!(recovered.try_get::<Option<String>>("", "meet_url").unwrap().is_none());
    assert_eq!(recovered.try_get::<i64>("", "active_holds").unwrap(), 0);
    assert_eq!(recovered.try_get::<Option<String>>("", "stage").unwrap().as_deref(), Some("late_recovery_date"));
    assert_eq!(
      recovered.try_get::<Option<String>>("", "recovery_appointment").unwrap(),
      Some(recovery.0.to_string())
    );
  })
  .await;
}
