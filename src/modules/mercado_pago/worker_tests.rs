#[path = "tests/fixture.rs"]
mod fixture;

use crate::{
  app::tests::with_database,
  modules::{
    appointments::{
      self,
      booking::{self, Commit, Prepared, SelectionRequest},
      model::Format,
    },
    doctor::{tests, Clock},
    mercado_pago::worker,
    whatsapp::inbox::sql,
  },
};
use fixture::{payment, Fixture, CHECKOUT_URL, USER_ID};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::json;
use std::{process::Command, sync::atomic::Ordering};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn real_video_booking_creates_a_preference_after_commit_and_sends_its_link() {
  with_database(|db| async move {
    let (f, _google, service_id, start) = video_booking_context(&db).await;
    let patient = "Paciente MP privado";
    let (contact, inbox) = f.create_contact(&db).await;
    let appointment_id = commit_video_booking(&db, &f, video_selection(contact, service_id, start, None), &inbox, patient).await;
    let appointment = appointments::store::get(&db.conn, appointment_id).await.unwrap().unwrap();
    assert_eq!((appointment.status.as_str(), appointment.payment.as_str()), ("awaiting_payment", "awaiting_receipt"));
    assert_eq!(count(&db, "SELECT count(*) AS n FROM appointment_jobs WHERE appointment_id=$1 AND kind='mp_preference' AND finished_at IS NULL", appointment_id).await, 1);
    let pending = f.preference(&db, appointment_id).await;
    assert_eq!(pending.try_get::<String>("", "outcome").unwrap(), "pending");
    assert!(pending.try_get::<Option<String>>("", "preference_id").unwrap().is_none());
    assert!(f.provider.requests.lock().await.is_empty(), "no provider call inside the booking transaction");
    let transfer = outbox_for(&db, appointment_id).await;
    assert!(transfer.iter().any(|(purpose, _, text)| purpose == "status" && text.contains("Banco de prueba")));

    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    let created = f.preference(&db, appointment_id).await;
    assert_eq!(created.try_get::<String>("", "outcome").unwrap(), "active");
    assert_eq!(created.try_get::<Option<String>>("", "url").unwrap().as_deref(), Some(CHECKOUT_URL));
    let request = f.requests_to("POST", "/checkout/preferences").await.remove(0);
    let body = &request.body;
    assert_eq!(body["items"][0]["unit_price"], json!(1255.0));
    assert_eq!(body["items"][0]["currency_id"], "MXN");
    assert_eq!(body["items"][0]["quantity"], 1);
    assert!(body["items"][0]["title"].as_str().unwrap().starts_with("Consulta por video - "));
    assert_eq!(body["external_reference"], format!("ma1:{appointment_id}:1"));
    assert_eq!(body["expires"], true);
    assert_eq!(body["binary_mode"], true);
    assert_eq!(body["payment_methods"]["installments"], 1);
    assert_eq!(body["payment_methods"]["excluded_payment_types"], json!([{"id":"ticket"},{"id":"atm"},{"id":"bank_transfer"}]));
    assert!(body.get("payer").is_none() && body.get("metadata").is_none() && body.get("notification_url").is_none());
    let text = body.to_string();
    assert!(!text.contains(patient) && !text.contains(&contact.to_string()));
    assert!(request.idempotency_key.is_some());
    let outbox = outbox_for(&db, appointment_id).await;
    assert!(outbox.iter().any(|(purpose, state, text)| purpose == "payment_details" && state == "queued" && text == &format!("Pague con tarjeta o saldo de Mercado Pago aquí: {CHECKOUT_URL}")));

    // A provider outage neither blocks booking nor removes bank transfer.
    f.provider.unavailable.store(true, Ordering::SeqCst);
    let (second_contact, second_inbox) = f.create_contact(&db).await;
    let second_id = commit_video_booking(&db, &f, video_selection(second_contact, service_id, start + 60 * 60_000, None), &second_inbox, patient).await;
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    assert_eq!(f.requests_to("POST", "/checkout/preferences").await.len(), 2);
    let retry = f.preference(&db, second_id).await;
    assert_eq!(retry.try_get::<String>("", "outcome").unwrap(), "pending");
    assert_eq!(count(&db, "SELECT count(*) AS n FROM appointment_jobs WHERE appointment_id=$1 AND kind='mp_preference' AND finished_at IS NULL", second_id).await, 1);
    assert_eq!(appointment_state(&db, second_id).await.0, "awaiting_payment");
    assert!(outbox_for(&db, second_id).await.iter().any(|(purpose, _, text)| purpose == "status" && text.contains("Banco de prueba")));
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn video_reschedule_keeps_the_existing_preference() {
  with_database(|db| async move {
    let (f, _google, service_id, start) = video_booking_context(&db).await;
    let (contact, inbox) = f.create_contact(&db).await;
    let initial = commit_video_booking(
      &db,
      &f,
      video_selection(contact, service_id, start, None),
      &inbox,
      "Paciente MP privado",
    )
    .await;
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref())
      .await
      .unwrap();
    let before = f.preference(&db, initial).await;
    let preference_id: String = before.try_get("", "preference_id").unwrap();
    let rescheduled = commit_video_booking(
      &db,
      &f,
      video_selection(contact, service_id, start + 60 * 60_000, Some((initial, 1))),
      &inbox,
      "Paciente MP privado",
    )
    .await;
    assert_eq!(rescheduled, initial);
    f.clock.advance(31);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref())
      .await
      .unwrap();
    let after = f.preference(&db, initial).await;
    assert_eq!(
      after.try_get::<String>("", "preference_id").unwrap(),
      preference_id
    );
    assert_eq!(after.try_get::<String>("", "outcome").unwrap(), "active");
    assert_eq!(
      f.requests_to("POST", "/checkout/preferences").await.len(),
      1
    );
    assert!(f
      .provider
      .requests
      .lock()
      .await
      .iter()
      .all(|request| request.method != "PUT"));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn polling_confirms_only_matching_payments_once_and_flags_later_reversals() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    f.set_ready(&db).await;
    let deadline = f.clock.now_millis() + 2 * 86_400_000;
    let mut appointments = Vec::new();
    for _ in 0..6 {
      let appointment = f.video_appointment(&db, "awaiting_payment", "awaiting_receipt", deadline).await;
      f.seed_active_preference(&db, &appointment).await;
      appointments.push(appointment);
    }
    let [matching, wrong_amount, wrong_currency, wrong_collector, wrong_reference, pending] = appointments.as_slice() else { unreachable!() };
    f.add_payment(&matching.external_reference(), payment(11, "approved", json!(1255.0), "MXN", &matching.external_reference(), USER_ID)).await;
    f.add_payment(&wrong_amount.external_reference(), payment(12, "approved", json!(1255.01), "MXN", &wrong_amount.external_reference(), USER_ID)).await;
    f.add_payment(&wrong_currency.external_reference(), payment(13, "approved", json!(1255.0), "USD", &wrong_currency.external_reference(), USER_ID)).await;
    f.add_payment(&wrong_collector.external_reference(), payment(14, "approved", json!(1255.0), "MXN", &wrong_collector.external_reference(), 999)).await;
    f.add_payment(&wrong_reference.external_reference(), payment(15, "approved", json!(1255.0), "MXN", &matching.external_reference(), USER_ID)).await;
    f.add_payment(&pending.external_reference(), payment(16, "pending", json!(1255.0), "MXN", &pending.external_reference(), USER_ID)).await;

    for _ in 0..2 {
      worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    }
    assert_eq!(appointment_state(&db, matching.id).await, ("confirmed".into(), "verified".into(), "pending".into()));
    let history = db.conn.query_one(sql("SELECT actor,kind FROM appointment_history WHERE appointment_id=$1 ORDER BY created_at DESC LIMIT 1", vec![matching.id.into()])).await.unwrap().unwrap();
    assert_eq!(history.try_get::<String>("", "actor").unwrap(), "System");
    assert_eq!(history.try_get::<String>("", "kind").unwrap(), "payment_verified_mercado_pago");
    assert_eq!(count(&db, "SELECT count(*) AS n FROM appointment_jobs WHERE appointment_id=$1 AND kind='calendar_sync' AND finished_at IS NULL", matching.id).await, 1);
    assert_eq!(f.payment_row(&db, 11).await.try_get::<String>("", "outcome").unwrap(), "confirmed");
    let closed = f.preference(&db, matching.id).await;
    assert_eq!(closed.try_get::<String>("", "outcome").unwrap(), "closed");
    let puts = f.provider.requests.lock().await.iter().filter(|request| request.method == "PUT").cloned().collect::<Vec<_>>();
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].path, format!("/checkout/preferences/{}", closed.try_get::<String>("", "preference_id").unwrap()));
    assert_eq!(puts[0].body["expires"], true);

    for (appointment, payment_id) in [(wrong_amount, 12), (wrong_currency, 13), (wrong_collector, 14), (wrong_reference, 15)] {
      assert_eq!(appointment_state(&db, appointment.id).await.1, "awaiting_receipt");
      assert_eq!(f.payment_row(&db, payment_id).await.try_get::<String>("", "outcome").unwrap(), "mismatch");
      assert_eq!(unresolved(&db, appointment.id, "online_payment_mismatch").await, 1);
    }
    assert_eq!(appointment_state(&db, pending.id).await.1, "awaiting_receipt");
    assert_eq!(count(&db, "SELECT count(*) AS n FROM mercado_pago_payments WHERE appointment_id=$1", pending.id).await, 0);
    assert!(f.requests_to("GET", "/v1/payments/16").await.is_empty(), "only approved payments are re-read");

    // Polling again never re-reads or re-applies a recorded payment.
    let reads = f.requests_to("GET", "/v1/payments/11").await.len();
    f.clock.advance(31);
    for _ in 0..2 {
      worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    }
    assert_eq!(f.requests_to("GET", "/v1/payments/11").await.len(), reads);
    assert_eq!(count(&db, "SELECT count(*) AS n FROM appointment_history WHERE appointment_id=$1 AND kind='payment_verified_mercado_pago'", matching.id).await, 1);

    // A chargeback made outside the app is flagged; Payment Verification stays as it was.
    f.set_payment_status(11, "charged_back").await;
    f.clock.advance(86_401);
    for _ in 0..3 {
      worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    }
    assert_eq!(unresolved(&db, matching.id, "online_payment_reversed").await, 1);
    assert_eq!(appointment_state(&db, matching.id).await.1, "verified");
    assert_eq!(f.payment_row(&db, 11).await.try_get::<String>("", "provider_status").unwrap(), "charged_back");
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn late_and_duplicate_payments_are_refunded_only_on_the_doctors_request() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    f.set_ready(&db).await;
    let now = f.clock.now_millis();
    let expired = f.video_appointment(&db, "expired", "awaiting_receipt", now - 1).await;
    let transfer_verified = f.video_appointment(&db, "confirmed", "verified", now + 86_400_000).await;
    for appointment in [&expired, &transfer_verified] {
      f.seed_active_preference(&db, appointment).await;
    }
    f.add_payment(&expired.external_reference(), payment(21, "approved", json!(1255), "MXN", &expired.external_reference(), USER_ID)).await;
    f.add_payment(&transfer_verified.external_reference(), payment(22, "approved", json!(1255.0), "MXN", &transfer_verified.external_reference(), USER_ID)).await;

    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    assert_eq!(appointment_state(&db, expired.id).await.0, "expired");
    assert_eq!(appointment_state(&db, transfer_verified.id).await.1, "verified");
    assert_eq!(unresolved(&db, expired.id, "online_payment_late").await, 1);
    assert_eq!(unresolved(&db, transfer_verified.id, "online_payment_duplicate").await, 1);
    assert_eq!(f.payment_row(&db, 21).await.try_get::<String>("", "outcome").unwrap(), "late");
    assert_eq!(f.payment_row(&db, 22).await.try_get::<String>("", "outcome").unwrap(), "duplicate");
    assert!(f.requests_to("POST", "/v1/payments/21/refunds").await.is_empty(), "nothing is refunded automatically");

    let detail = tests::text(tests::authenticated_get(&f.app, &format!("/appointments/{}", expired.id), &f.session).await).await;
    assert!(detail.contains("Reembolsar con Mercado Pago"));
    let refund_path = format!("/appointments/{}/mercado-pago/21/refund", expired.id);
    assert_eq!(f.post(&refund_path, "forged").await.status(), 403);
    assert_eq!(f.post(&format!("/appointments/{}/mercado-pago/21/refund", transfer_verified.id), &f.csrf).await.status(), 303);
    assert_eq!(f.payment_row(&db, 21).await.try_get::<String>("", "refund_state").unwrap(), "none", "payment of another Appointment is not refundable");
    assert_eq!(f.post(&refund_path, &f.csrf).await.status(), 303);
    assert_eq!(f.post(&refund_path, &f.csrf).await.status(), 303);
    assert_eq!(count(&db, "SELECT count(*) AS n FROM appointment_jobs WHERE appointment_id=$1 AND kind='mp_refund'", expired.id).await, 1);
    assert_eq!(f.payment_row(&db, 21).await.try_get::<String>("", "refund_state").unwrap(), "requested");

    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    let refunds = f.requests_to("POST", "/v1/payments/21/refunds").await;
    assert_eq!(refunds.len(), 1);
    assert_eq!(refunds[0].body, json!({"amount": 1255.0}));
    assert!(refunds[0].idempotency_key.is_some());
    let refunded = f.payment_row(&db, 21).await;
    assert_eq!(refunded.try_get::<String>("", "refund_state").unwrap(), "refunded");
    assert_eq!(refunded.try_get::<Option<i64>>("", "refund_id").unwrap(), Some(900_021));
    assert_eq!(unresolved(&db, expired.id, "online_payment_late").await, 0);
    assert_eq!(count(&db, "SELECT count(*) AS n FROM appointment_history WHERE appointment_id=$1 AND kind='mercado_pago_refunded' AND actor='System'", expired.id).await, 1);
    assert!(outbox_for(&db, expired.id).await.iter().any(|(purpose, _, text)| purpose == "status" && text.contains("reembolso")));

    // A rejected refund keeps the Attention Item for a manual refund.
    f.provider.refund_rejected.store(true, Ordering::SeqCst);
    assert_eq!(f.post(&format!("/appointments/{}/mercado-pago/22/refund", transfer_verified.id), &f.csrf).await.status(), 303);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    assert_eq!(f.payment_row(&db, 22).await.try_get::<String>("", "refund_state").unwrap(), "failed");
    assert_eq!(unresolved(&db, transfer_verified.id, "online_payment_duplicate").await, 1);
    assert_eq!(count(&db, "SELECT count(*) AS n FROM appointment_history WHERE appointment_id=$1 AND kind='mercado_pago_refund_failed'", transfer_verified.id).await, 1);
    assert_eq!(appointment_state(&db, transfer_verified.id).await.1, "verified");
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn cancelled_paid_appointment_refund_resolves_refund_due_without_reversing_payment() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    f.set_ready(&db).await;
    let now = f.clock.now_millis();
    let cancelled = f.video_appointment(&db, "cancelled", "verified", now + 86_400_000).await;
    f.seed_active_preference(&db, &cancelled).await;
    db.conn.execute(sql("UPDATE mercado_pago_preferences SET outcome='closed',closed_at=$2,next_check_at=NULL WHERE appointment_id=$1", vec![cancelled.id.into(), now.into()])).await.unwrap();
    db.conn.execute(sql("INSERT INTO mercado_pago_payments(payment_id,appointment_id,revision,amount_centavos,outcome,provider_status,first_seen_at,updated_at) VALUES (31,$1,1,125500,'confirmed','approved',$2,$2)", vec![cancelled.id.into(), now.into()])).await.unwrap();
    f.add_payment(&cancelled.external_reference(), payment(31, "approved", json!(1255.0), "MXN", &cancelled.external_reference(), USER_ID)).await;
    let tx = db.conn.begin().await.unwrap();
    appointments::store::attention(&tx, Some(cancelled.id), Some(cancelled.contact), "refund_due", now).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(f.post(&format!("/appointments/{}/mercado-pago/31/refund", cancelled.id), &f.csrf).await.status(), 303);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    assert_eq!(f.payment_row(&db, 31).await.try_get::<String>("", "refund_state").unwrap(), "refunded");
    assert_eq!(unresolved(&db, cancelled.id, "refund_due").await, 0);
    assert_eq!(appointment_state(&db, cancelled.id).await, ("cancelled".into(), "verified".into(), "not_yet".into()));
    let detail = tests::text(tests::authenticated_get(&f.app, &format!("/appointments/{}", cancelled.id), &f.session).await).await;
    assert!(!detail.contains("Reembolsar con Mercado Pago"));
    assert!(detail.contains("Reembolso emitido por Mercado Pago"));
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn unpaid_deadline_closes_the_preference_and_existing_expiry_releases_the_hold() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    f.set_ready(&db).await;
    let now = f.clock.now_millis();
    let appointment = f
      .video_appointment(&db, "awaiting_payment", "awaiting_receipt", now - 1)
      .await;
    let preference_id = f.seed_active_preference(&db, &appointment).await;
    f.add_expiry_job(&db, appointment.id, now - 1).await;

    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref())
      .await
      .unwrap();
    let preference = f.preference(&db, appointment.id).await;
    assert_eq!(
      preference.try_get::<String>("", "outcome").unwrap(),
      "closed"
    );
    assert!(preference
      .try_get::<Option<i64>>("", "closed_at")
      .unwrap()
      .is_some());
    assert_eq!(
      f.requests_to("PUT", &format!("/checkout/preferences/{preference_id}"))
        .await
        .len(),
      1
    );

    appointments::timers::run_once(&db, f.clock.as_ref(), &f.cfg)
      .await
      .unwrap();
    assert_eq!(appointment_state(&db, appointment.id).await.0, "expired");
    let hold = db
      .conn
      .query_one(sql(
        "SELECT active FROM appointment_holds WHERE appointment_id=$1",
        vec![appointment.id.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert!(!hold.try_get::<bool>("", "active").unwrap());

    // Late checks continue for two hours after closing, then stop.
    f.clock.advance(15 * 60 + 1);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref())
      .await
      .unwrap();
    let searches = f.requests_to("GET", "/v1/payments/search").await;
    assert_eq!(searches.len(), 2);
    assert!(searches
      .iter()
      .all(|search| search.query["external_reference"] == appointment.external_reference()));
    f.clock.advance(2 * 3600);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref())
      .await
      .unwrap();
    assert!(f
      .preference(&db, appointment.id)
      .await
      .try_get::<Option<i64>>("", "next_check_at")
      .unwrap()
      .is_none());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn absent_configuration_keeps_transfer_instructions_and_makes_no_provider_calls() {
  with_database(|db| async move {
    let f = Fixture::new(&db, false).await;
    let now = f.clock.now_millis();
    let appointment = f
      .video_appointment(
        &db,
        "awaiting_payment",
        "awaiting_receipt",
        now + 86_400_000,
      )
      .await;
    f.queue_transfer_instructions(&db, &appointment).await;
    f.schedule_preference(&db, &appointment).await;
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, None)
      .await
      .unwrap();
    assert!(f.provider.requests.lock().await.is_empty());
    assert_eq!(
      count(
        &db,
        "SELECT count(*) AS n FROM mercado_pago_preferences WHERE appointment_id=$1",
        appointment.id
      )
      .await,
      0
    );
    assert!(outbox_for(&db, appointment.id)
      .await
      .iter()
      .any(|(purpose, state, text)| purpose == "status"
        && state == "queued"
        && text.contains("Banco de prueba")));
    let page =
      tests::text(tests::authenticated_get(&f.app, "/settings/mercado-pago", &f.session).await)
        .await;
    assert!(page.contains("Mercado Pago no está configurado"));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn rejected_token_blocks_new_preferences_until_it_is_accepted_again() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    let now = f.clock.now_millis();
    let appointment = f.video_appointment(&db, "awaiting_payment", "awaiting_receipt", now + 86_400_000).await;
    f.provider.unauthorized.store(true, Ordering::SeqCst);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    let account = db.conn.query_one(sql("SELECT user_id,notice FROM mercado_pago_account WHERE id=1", vec![])).await.unwrap().unwrap();
    assert_eq!(account.try_get::<Option<String>>("", "notice").unwrap().as_deref(), Some("unauthorized"));
    assert_eq!(count(&db, "SELECT count(*) AS n FROM attention_items WHERE kind='mercado_pago_unauthorized' AND resolved_at IS NULL AND $1::uuid IS NOT NULL", appointment.id).await, 1);
    f.schedule_preference(&db, &appointment).await;
    assert_eq!(count(&db, "SELECT count(*) AS n FROM mercado_pago_preferences WHERE appointment_id=$1", appointment.id).await, 0);
    let page = tests::text(tests::authenticated_get(&f.app, "/settings/mercado-pago", &f.session).await).await;
    assert!(page.contains("Mercado Pago rechazó el token"));
    assert!(!page.contains("synthetic-fixture"));

    f.provider.unauthorized.store(false, Ordering::SeqCst);
    f.clock.advance(5 * 60 + 1);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    let account = db.conn.query_one(sql("SELECT user_id,notice FROM mercado_pago_account WHERE id=1", vec![])).await.unwrap().unwrap();
    assert_eq!(account.try_get::<Option<i64>>("", "user_id").unwrap(), Some(USER_ID));
    assert!(account.try_get::<Option<String>>("", "notice").unwrap().is_none());
    assert_eq!(count(&db, "SELECT count(*) AS n FROM attention_items WHERE kind='mercado_pago_unauthorized' AND resolved_at IS NULL AND $1::uuid IS NOT NULL", appointment.id).await, 0);
    f.schedule_preference(&db, &appointment).await;
    assert_eq!(count(&db, "SELECT count(*) AS n FROM mercado_pago_preferences WHERE appointment_id=$1", appointment.id).await, 1);
    let page = tests::text(tests::authenticated_get(&f.app, "/settings/mercado-pago", &f.session).await).await;
    assert!(page.contains("Mercado Pago está listo"));
  }).await;
}

#[test]
fn malformed_access_token_fails_startup_configuration_without_echoing_it() {
  const CHILD: &str = "MEDICAL_MERCADO_PAGO_CONFIG_FAILURE_CHILD";
  const TEST_NAME: &str = "malformed_access_token_fails_startup_configuration_without_echoing_it";
  if std::env::var(CHILD).as_deref() == Ok("1") {
    let _ = crate::common::config::Configuration::new();
    panic!("startup unexpectedly accepted a malformed Mercado Pago token");
  }
  let name = format!("{}::{TEST_NAME}", module_path!())
    .trim_start_matches("server::")
    .to_owned();
  let output = Command::new(std::env::current_exe().unwrap())
    .env_clear()
    .args(["--exact", name.as_str(), "--test-threads=1"])
    .env(CHILD, "1")
    .env("APP_ENV", "production")
    .env("PORT", "3000")
    .env(
      "DATABASE_URL",
      "postgres://test:test@127.0.0.1/mercado_pago_test",
    )
    .env("PUBLIC_ORIGIN", "https://practice.test")
    .env("MERCADO_PAGO_ACCESS_TOKEN", "bad-token-sentinel")
    .output()
    .unwrap();
  assert!(!output.status.success());
  let diagnostic = format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(diagnostic.contains("Invalid Mercado Pago configuration"));
  assert!(!diagnostic.contains("bad-token-sentinel"));
}

type GoogleTestFixture = crate::modules::google_calendar::tests::fixture::Fixture;

async fn video_booking_context(
  db: &crate::database::Db,
) -> (Fixture, GoogleTestFixture, Uuid, i64) {
  let mut google = GoogleTestFixture::new(db).await;
  google.connect().await;
  assert_eq!(google.select().await.status(), 303);
  let weekly_hours = serde_json::Value::Array(
    (0..7)
      .map(|_| json!({"status":"open","ranges":[{"start":0,"end":1440}]}))
      .collect(),
  );
  db.conn.execute(sql(
    "UPDATE practice SET name='Consultorio de prueba',location='Consultorio de prueba',timezone='UTC',weekly_hours=$1,buffer_before_minutes=0,buffer_after_minutes=0,minimum_notice_minutes=60,grid_minutes=30,horizon_days=30,cancellation_policy='Confirme antes de cancelar.',bank='Banco de prueba',beneficiary='Beneficiario de prueba',clabe='012345678901234567',receipt_window_minutes=4320,review_margin_minutes=180,grace_minutes=10,reminder_2h_enabled=true WHERE id=1",
    vec![weekly_hours.into()],
  )).await.unwrap();
  let service_id = Uuid::now_v7();
  db.conn.execute(sql(
    "INSERT INTO services(id,name,duration_minutes,price_centavos) VALUES ($1,'Consulta general',30,125500)",
    vec![service_id.into()],
  )).await.unwrap();
  assert_eq!(
    google
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
    303,
  );
  for _ in 0..8 {
    crate::modules::google_calendar::worker::run_once(
      db,
      google.clock.as_ref(),
      &google.cfg,
      Some(&google.client),
    )
    .await
    .unwrap();
    google.clock.advance(11);
  }
  let old_app = std::mem::replace(&mut google.app, axum::Router::new());
  drop(old_app);
  let fixture = Fixture::with_session(
    db,
    &mut google.cfg,
    google.clock.clone(),
    google.session.clone(),
    true,
  )
  .await;
  fixture.set_ready(db).await;
  let date = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(fixture.clock.now_millis())
    .unwrap()
    .date_naive()
    + chrono::Duration::days(3);
  let starts_at = date
    .and_hms_opt(10, 0, 0)
    .unwrap()
    .and_utc()
    .timestamp_millis();
  (fixture, google, service_id, starts_at)
}

fn video_selection(
  contact: Uuid,
  service_id: Uuid,
  starts_at: i64,
  current: Option<(Uuid, i64)>,
) -> SelectionRequest {
  SelectionRequest {
    contact,
    service_id,
    format: Format::Video,
    starts_at,
    appointment_id: current.map(|(id, _)| id),
    expected_revision: current.map(|(_, revision)| revision),
    late_recovery: false,
  }
}

async fn commit_video_booking(
  db: &crate::database::Db,
  fixture: &Fixture,
  request: SelectionRequest,
  inbox: &str,
  patient: &str,
) -> Uuid {
  let expected_revision = request.expected_revision.map_or(1, |revision| revision + 1);
  let prepared = booking::prepare_selection(
    db,
    fixture.clock.as_ref(),
    &fixture.cfg,
    fixture.google_client.as_ref(),
    request,
    Some(patient),
  )
  .await
  .unwrap();
  let Prepared::Ready(preflight) = prepared else {
    panic!("configured video selection must produce a bookable slot");
  };
  let tx = db.conn.begin().await.unwrap();
  let result = booking::commit_selection(
    &tx,
    preflight,
    inbox,
    Some(patient),
    "https://practice.test",
    fixture.clock.now_millis(),
  )
  .await
  .unwrap();
  tx.commit().await.unwrap();
  let Commit::Applied {
    appointment_id,
    status,
    revision,
  } = result
  else {
    panic!("video booking must commit successfully");
  };
  assert_eq!(status, "awaiting_payment");
  assert_eq!(revision, expected_revision);
  appointment_id
}

async fn appointment_state(db: &crate::database::Db, id: Uuid) -> (String, String, String) {
  let row = db
    .conn
    .query_one(sql(
      "SELECT status,payment,calendar_sync FROM appointments WHERE id=$1",
      vec![id.into()],
    ))
    .await
    .unwrap()
    .unwrap();
  (
    row.try_get("", "status").unwrap(),
    row.try_get("", "payment").unwrap(),
    row.try_get("", "calendar_sync").unwrap(),
  )
}

async fn count(db: &crate::database::Db, query: &str, id: Uuid) -> i64 {
  db.conn
    .query_one(sql(query, vec![id.into()]))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap()
}

async fn unresolved(db: &crate::database::Db, id: Uuid, kind: &str) -> i64 {
  db.conn
    .query_one(sql("SELECT count(*) AS n FROM attention_items WHERE appointment_id=$1 AND kind=$2 AND resolved_at IS NULL", vec![id.into(), kind.into()]))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap()
}

async fn outbox_for(db: &crate::database::Db, id: Uuid) -> Vec<(String, String, String)> {
  db.conn
    .query_all(sql("SELECT purpose,state,rendered_text FROM outbox_messages WHERE appointment_id=$1 ORDER BY created_at,id", vec![id.into()]))
    .await
    .unwrap()
    .iter()
    .map(|row| (row.try_get("", "purpose").unwrap(), row.try_get("", "state").unwrap(), row.try_get("", "rendered_text").unwrap()))
    .collect()
}
