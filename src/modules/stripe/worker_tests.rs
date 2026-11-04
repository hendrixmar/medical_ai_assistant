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
    stripe::worker,
    whatsapp::inbox::sql,
  },
};
use fixture::{paid_session, Fixture, ACCOUNT, CHECKOUT_URL};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::json;
use std::process::Command;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn connect_reuses_the_created_account_and_requires_origin_and_csrf() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    assert_eq!(
      f.post(
        "/settings/stripe/connect",
        "https://attacker.invalid",
        &f.csrf
      )
      .await
      .status(),
      403
    );
    assert_eq!(
      f.post(
        "/settings/stripe/connect",
        "https://practice.test",
        "forged"
      )
      .await
      .status(),
      403
    );
    assert!(f.provider.requests.lock().await.is_empty());

    let first = f
      .post("/settings/stripe/connect", "https://practice.test", &f.csrf)
      .await;
    assert_eq!(first.status(), 303);
    assert_eq!(
      first.headers()["location"],
      "https://connect.stripe.com/setup/test_onboarding"
    );
    let second = f
      .post("/settings/stripe/connect", "https://practice.test", &f.csrf)
      .await;
    assert_eq!(second.status(), 303);
    assert_eq!(
      second.headers()["location"],
      "https://connect.stripe.com/setup/test_onboarding"
    );

    let requests = f.provider.requests.lock().await.clone();
    assert_eq!(
      requests
        .iter()
        .filter(|request| request.path == "/v1/accounts")
        .count(),
      1
    );
    let account_links: Vec<_> = requests
      .iter()
      .filter(|request| request.path == "/v1/account_links")
      .collect();
    assert_eq!(account_links.len(), 2);
    assert!(account_links
      .iter()
      .all(|request| request.form["account"] == ACCOUNT));
    assert!(account_links
      .iter()
      .all(|request| request.form["type"] == "account_onboarding"));
    assert!(account_links.iter().all(
      |request| request.form["refresh_url"] == "https://practice.test/settings/stripe/refresh"
    ));
    assert!(account_links
      .iter()
      .all(|request| request.form["return_url"] == "https://practice.test/settings/stripe/return"));

    f.provider.set_account_status(true, true).await;
    let returned = tests::authenticated_get(&f.app, "/settings/stripe/return", &f.session).await;
    assert_eq!(returned.headers()["location"], "/settings/stripe");
    let ready_page =
      tests::text(tests::authenticated_get(&f.app, "/settings/stripe", &f.session).await).await;
    assert!(ready_page.contains("Stripe está conectado"));
    assert!(ready_page.contains("Detalles enviados a Stripe: sí."));
    let row = db
      .conn
      .query_one(sql(
        "SELECT account_id,charges_enabled,details_submitted FROM stripe_connection WHERE id=1",
        vec![],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(row.try_get::<String>("", "account_id").unwrap(), ACCOUNT);
    assert!(row.try_get::<bool>("", "charges_enabled").unwrap());
    assert!(row.try_get::<bool>("", "details_submitted").unwrap());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn real_video_booking_commits_transfer_and_stripe_jobs_before_creating_a_card_link() {
  with_database(|db| async move {
    let (f, _google, service_id, start) = video_booking_context(&db).await;
    let patient = "Paciente Stripe privado";
    let (contact, inbox) = f.create_contact(&db).await;
    let appointment_id = commit_video_booking(
      &db,
      &f,
      video_selection(contact, service_id, start, None),
      &inbox,
      patient,
    )
    .await;
    let appointment = appointments::store::get(&db.conn, appointment_id).await.unwrap().unwrap();
    assert_eq!(appointment.status, "awaiting_payment");
    assert_eq!(appointment.payment, "awaiting_receipt");
    assert_eq!(appointment.format, "video");
    assert_eq!(appointment.price_centavos, 125_500);

    let scheduled = db.conn.query_one(sql(
      "SELECT count(*) AS n FROM appointment_jobs WHERE appointment_id=$1 AND kind='stripe_link' AND finished_at IS NULL",
      vec![appointment_id.into()],
    )).await.unwrap().unwrap();
    assert_eq!(scheduled.try_get::<i64>("", "n").unwrap(), 1);
    let pending_link = f.row(&db, appointment_id).await;
    assert_eq!(pending_link.try_get::<String>("", "outcome").unwrap(), "pending");
    assert!(pending_link.try_get::<Option<String>>("", "link_id").unwrap().is_none());
    let transfer_outbox = outbox_for(&db, appointment_id).await;
    assert!(transfer_outbox.iter().any(|(purpose, state, text)| purpose == "status" && state == "queued" && text.contains("Banco de prueba")));
    assert!(transfer_outbox.iter().any(|(purpose, state, text)| purpose == "payment_details" && state == "queued" && text == "012345678901234567"));

    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    let link = f.row(&db, appointment_id).await;
    assert_eq!(link.try_get::<Option<String>>("", "link_id").unwrap().as_deref(), Some("plink_fixture1"));
    assert_eq!(link.try_get::<Option<String>>("", "url").unwrap().as_deref(), Some(CHECKOUT_URL));
    assert_eq!(link.try_get::<i64>("", "amount_centavos").unwrap(), 125_500);

    let requests = f.provider.requests.lock().await.clone();
    let price = requests.iter().find(|request| request.path == "/v1/prices").unwrap();
    assert_eq!(price.method, "POST");
    assert_eq!(price.stripe_account.as_deref(), Some(ACCOUNT));
    assert_eq!(price.form["currency"], "mxn");
    assert_eq!(price.form["unit_amount"], "125500");
    assert!(price.form["product_data[name]"].starts_with("Consulta por video - "));
    let payment_link = requests.iter().find(|request| request.path == "/v1/payment_links").unwrap();
    assert_eq!(payment_link.method, "POST");
    assert_eq!(payment_link.stripe_account.as_deref(), Some(ACCOUNT));
    assert_eq!(payment_link.form["line_items[0][price]"], "price_fixture1");
    assert_eq!(payment_link.form["line_items[0][quantity]"], "1");
    assert_eq!(payment_link.form["payment_method_types[0]"], "card");
    assert_eq!(payment_link.form.keys().filter(|key| key.starts_with("payment_method_types[")).count(), 1);
    assert_eq!(payment_link.form["restrictions[completed_sessions][limit]"], "1");
    assert_eq!(payment_link.form["metadata[appointment_id]"], appointment_id.to_string());
    assert_eq!(payment_link.form["metadata[revision]"], "1");
    assert_eq!(payment_link.form["metadata[payment_reference]"], appointment.payment_reference.unwrap());
    let contact_id = contact.to_string();
    for request in [price, payment_link] {
      assert!(request.form.values().all(|value| !value.contains(patient)));
      assert!(request.form.values().all(|value| !value.contains(&contact_id)));
    }
    assert!(payment_link.form.keys().all(|key| !key.starts_with("application_fee")));
    let outbox = outbox_for(&db, appointment_id).await;
    assert!(outbox.iter().any(|(purpose, state, text)| purpose == "payment_details" && state == "queued" && text == &format!("Pague con tarjeta aquí: {CHECKOUT_URL}")));

    f.provider.unavailable.store(true, std::sync::atomic::Ordering::SeqCst);
    let request_count = f.provider.requests.lock().await.len();
    let (second_contact, second_inbox) = f.create_contact(&db).await;
    let second_id = commit_video_booking(
      &db,
      &f,
      video_selection(second_contact, service_id, start + 60 * 60_000, None),
      &second_inbox,
      patient,
    )
    .await;
    assert_eq!(f.provider.requests.lock().await.len(), request_count);
    let second = appointments::store::get(&db.conn, second_id).await.unwrap().unwrap();
    assert_eq!(second.status, "awaiting_payment");
    assert_eq!(second.payment, "awaiting_receipt");
    let second_outbox = outbox_for(&db, second_id).await;
    assert!(second_outbox.iter().any(|(purpose, state, text)| purpose == "status" && state == "queued" && text.contains("Banco de prueba")));
    assert!(second_outbox.iter().any(|(purpose, state, text)| purpose == "payment_details" && state == "queued" && text == "012345678901234567"));
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    let failed_requests = f.provider.requests.lock().await.clone();
    assert!(failed_requests.iter().skip(request_count).any(|request| request.path == "/v1/prices" && request.stripe_account.as_deref() == Some(ACCOUNT)));
    let pending = f.row(&db, second_id).await;
    assert_eq!(pending.try_get::<String>("", "outcome").unwrap(), "pending");
    assert!(pending.try_get::<Option<String>>("", "link_id").unwrap().is_none());
    assert_eq!(appointments::store::get(&db.conn, second_id).await.unwrap().unwrap().status, "awaiting_payment");
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn video_reschedule_keeps_the_existing_stripe_link_active() {
  with_database(|db| async move {
    let (f, _google, service_id, start) = video_booking_context(&db).await;
    let (contact, inbox) = f.create_contact(&db).await;
    let initial = commit_video_booking(
      &db,
      &f,
      video_selection(contact, service_id, start, None),
      &inbox,
      "Paciente Stripe privado",
    )
    .await;
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref())
      .await
      .unwrap();
    let before = f.row(&db, initial).await;
    let link_id: String = before.try_get("", "link_id").unwrap();
    let before_deadline: i64 = before.try_get("", "deactivate_at").unwrap();

    let rescheduled = commit_video_booking(
      &db,
      &f,
      video_selection(contact, service_id, start + 60 * 60_000, Some((initial, 1))),
      &inbox,
      "Paciente Stripe privado",
    )
    .await;
    assert_eq!(rescheduled, initial);
    let appointment = appointments::store::get(&db.conn, initial)
      .await
      .unwrap()
      .unwrap();
    assert_eq!(appointment.revision, 2);
    assert_eq!(appointment.receipt_deadline, Some(before_deadline));
    assert_eq!(appointment.starts_at, start + 60 * 60_000);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref())
      .await
      .unwrap();
    let after = db
      .conn
      .query_one(sql(
        "SELECT * FROM stripe_payment_links WHERE appointment_id=$1 AND revision=1",
        vec![initial.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(after.try_get::<String>("", "link_id").unwrap(), link_id);
    assert_eq!(after.try_get::<String>("", "outcome").unwrap(), "active");
    assert!(after
      .try_get::<Option<i64>>("", "deactivated_at")
      .unwrap()
      .is_none());
    assert_eq!(
      after.try_get::<i64>("", "deactivate_at").unwrap(),
      before_deadline
    );
    assert_eq!(
      f.provider
        .requests
        .lock()
        .await
        .iter()
        .filter(|request| request.path.starts_with("/v1/payment_links/")
          && request.path != "/v1/payment_links")
        .count(),
      0,
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn disconnect_forgets_account_but_keeps_failed_open_links_due_for_retry() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    f.set_ready(&db).await;
    let now = f.clock.now_millis();
    let appointment = f
      .video_appointment(
        &db,
        "awaiting_payment",
        "awaiting_receipt",
        now + 86_400_000,
      )
      .await;
    f.seed_active_link(&db, &appointment, "plink_disconnect", now + 86_400_000)
      .await;
    f.provider
      .unavailable
      .store(true, std::sync::atomic::Ordering::SeqCst);

    let response = f
      .post(
        "/settings/stripe/disconnect",
        "https://practice.test",
        &f.csrf,
      )
      .await;
    assert_eq!(response.status(), 303);
    let connection = db
      .conn
      .query_one(sql(
        "SELECT account_id,charges_enabled FROM stripe_connection WHERE id=1",
        vec![],
      ))
      .await
      .unwrap()
      .unwrap();
    assert!(connection
      .try_get::<Option<String>>("", "account_id")
      .unwrap()
      .is_none());
    assert!(!connection.try_get::<bool>("", "charges_enabled").unwrap());
    let disconnected_link = f.row(&db, appointment.id).await;
    assert_eq!(
      disconnected_link
        .try_get::<String>("", "account_id")
        .unwrap(),
      ACCOUNT
    );
    assert!(disconnected_link
      .try_get::<Option<i64>>("", "deactivated_at")
      .unwrap()
      .is_none());
    assert!(
      disconnected_link
        .try_get::<Option<i64>>("", "next_check_at")
        .unwrap()
        .unwrap()
        > now
    );

    f.clock.advance(61);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref())
      .await
      .unwrap();
    let retried = f.row(&db, appointment.id).await;
    assert_eq!(
      retried.try_get::<String>("", "account_id").unwrap(),
      ACCOUNT
    );
    assert!(retried
      .try_get::<Option<i64>>("", "deactivated_at")
      .unwrap()
      .is_none());
    assert!(
      retried
        .try_get::<Option<i64>>("", "next_check_at")
        .unwrap()
        .unwrap()
        > f.clock.now_millis()
    );
    let requests = f.provider.requests.lock().await.clone();
    let deactivations: Vec<_> = requests
      .iter()
      .filter(|request| request.path == "/v1/payment_links/plink_disconnect")
      .collect();
    assert_eq!(deactivations.len(), 2);
    assert!(deactivations
      .iter()
      .all(|request| request.stripe_account.as_deref() == Some(ACCOUNT)));
    assert!(requests.iter().any(|request| request.method == "GET"
      && request.path == "/v1/checkout/sessions"
      && request.stripe_account.as_deref() == Some(ACCOUNT)));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn polling_confirms_only_a_matching_paid_session_and_deactivates_its_link() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    f.set_ready(&db).await;
    let now = f.clock.now_millis();
    let deadline = now + 86_400_000;
    let matching = f.video_appointment(&db, "awaiting_payment", "awaiting_receipt", deadline).await;
    let wrong_amount = f.video_appointment(&db, "awaiting_payment", "awaiting_receipt", deadline).await;
    let wrong_currency = f.video_appointment(&db, "awaiting_payment", "awaiting_receipt", deadline).await;
    f.seed_active_link(&db, &matching, "plink_match", deadline).await;
    f.seed_active_link(&db, &wrong_amount, "plink_wrong_amount", deadline).await;
    f.seed_active_link(&db, &wrong_currency, "plink_wrong_currency", deadline).await;
    f.set_sessions("plink_match", vec![paid_session("cs_matching", matching.amount_centavos, "mxn")]).await;
    f.set_sessions("plink_wrong_amount", vec![paid_session("cs_wrong_amount", matching.amount_centavos + 1, "mxn")]).await;
    f.set_sessions("plink_wrong_currency", vec![paid_session("cs_wrong_currency", wrong_currency.amount_centavos, "usd")]).await;

    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    let paid = appointment_state(&db, matching.id).await;
    assert_eq!(paid.0, "confirmed");
    assert_eq!(paid.1, "verified");
    assert_eq!(paid.2, "pending");
    let history = db.conn.query_one(sql(
      "SELECT actor,kind FROM appointment_history WHERE appointment_id=$1 ORDER BY created_at DESC LIMIT 1",
      vec![matching.id.into()],
    )).await.unwrap().unwrap();
    assert_eq!(history.try_get::<String>("", "actor").unwrap(), "System");
    assert_eq!(history.try_get::<String>("", "kind").unwrap(), "payment_verified_stripe");
    let calendar_job = db.conn.query_one(sql(
      "SELECT count(*) AS n FROM appointment_jobs WHERE appointment_id=$1 AND kind='calendar_sync' AND finished_at IS NULL",
      vec![matching.id.into()],
    )).await.unwrap().unwrap();
    assert_eq!(calendar_job.try_get::<i64>("", "n").unwrap(), 1);
    let paid_link = f.row(&db, matching.id).await;
    assert_eq!(paid_link.try_get::<Option<String>>("", "paid_session_id").unwrap().as_deref(), Some("cs_matching"));
    assert!(paid_link.try_get::<Option<i64>>("", "deactivated_at").unwrap().is_some());
    assert_eq!(paid_link.try_get::<String>("", "outcome").unwrap(), "paid");

    for appointment in [&wrong_amount, &wrong_currency] {
      let state = appointment_state(&db, appointment.id).await;
      assert_eq!(state.0, "awaiting_payment");
      assert_eq!(state.1, "awaiting_receipt");
      let history = db.conn.query_one(sql(
        "SELECT count(*) AS n FROM appointment_history WHERE appointment_id=$1 AND kind='payment_verified_stripe'",
        vec![appointment.id.into()],
      )).await.unwrap().unwrap();
      assert_eq!(history.try_get::<i64>("", "n").unwrap(), 0);
    }
    let requests = f.provider.requests.lock().await.clone();
    let session_reads: Vec<_> = requests.iter().filter(|request| request.path == "/v1/checkout/sessions").collect();
    assert_eq!(session_reads.len(), 3);
    assert!(session_reads.iter().all(|request| request.stripe_account.as_deref() == Some(ACCOUNT)));
    assert!(session_reads.iter().all(|request| request.query.get("limit").map(String::as_str) == Some("3")));
    assert!(session_reads.iter().all(|request| request.query.contains_key("payment_link")));
    assert!(requests.iter().any(|request| request.path == "/v1/payment_links/plink_match" && request.form["active"] == "false"));
    let deactivate = requests.iter().find(|request| request.path == "/v1/payment_links/plink_match").unwrap();
    assert_eq!(deactivate.stripe_account.as_deref(), Some(ACCOUNT));
    assert_eq!(deactivate.form["active"], "false");
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn late_and_duplicate_paid_sessions_create_attention_without_reconfirming() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    f.set_ready(&db).await;
    let now = f.clock.now_millis();
    let expired = f.video_appointment(&db, "expired", "awaiting_receipt", now - 1).await;
    let transfer_verified = f.video_appointment(&db, "confirmed", "verified", now + 86_400_000).await;
    db.conn.execute(sql(
      "INSERT INTO appointment_history(id,appointment_id,actor,kind,created_at) VALUES ($1,$2,'Doctor','payment_verified',$3)",
      vec![Uuid::now_v7().into(), transfer_verified.id.into(), now.into()],
    )).await.unwrap();
    f.seed_active_link(&db, &expired, "plink_late", now - 1).await;
    f.seed_active_link(&db, &transfer_verified, "plink_duplicate", now + 86_400_000).await;
    f.set_sessions("plink_late", vec![paid_session("cs_late", expired.amount_centavos, "mxn")]).await;
    f.set_sessions("plink_duplicate", vec![paid_session("cs_duplicate", transfer_verified.amount_centavos, "mxn")]).await;

    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref()).await.unwrap();
    for appointment in [&expired, &transfer_verified] {
      let row = appointment_state(&db, appointment.id).await;
      assert_eq!(row.0, if appointment.id == expired.id { "expired" } else { "confirmed" });
      assert_eq!(row.1, if appointment.id == expired.id { "awaiting_receipt" } else { "verified" });
    }
    for (appointment, kind) in [(&expired, "stripe_payment_late"), (&transfer_verified, "stripe_payment_duplicate")] {
      let attention = db.conn.query_one(sql(
        "SELECT count(*) AS n FROM attention_items WHERE appointment_id=$1 AND kind=$2 AND resolved_at IS NULL",
        vec![appointment.id.into(), kind.to_owned().into()],
      )).await.unwrap().unwrap();
      assert_eq!(attention.try_get::<i64>("", "n").unwrap(), 1);
    }
    let late_history = db.conn.query_one(sql(
      "SELECT count(*) AS n FROM appointment_history WHERE appointment_id=$1 AND kind='stripe_payment_late'",
      vec![expired.id.into()],
    )).await.unwrap().unwrap();
    assert_eq!(late_history.try_get::<i64>("", "n").unwrap(), 1);
    let late_link = f.row(&db, expired.id).await;
    assert_eq!(late_link.try_get::<String>("", "outcome").unwrap(), "late");
    let duplicate_link = f.row(&db, transfer_verified.id).await;
    assert_eq!(duplicate_link.try_get::<String>("", "outcome").unwrap(), "duplicate");
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn unpaid_deadline_deactivates_payment_link_and_existing_expiry_releases_the_hold() {
  with_database(|db| async move {
    let f = Fixture::new(&db, true).await;
    f.set_ready(&db).await;
    let now = f.clock.now_millis();
    let appointment = f
      .video_appointment(&db, "awaiting_payment", "awaiting_receipt", now - 1)
      .await;
    f.seed_active_link(&db, &appointment, "plink_deadline", now - 1)
      .await;
    f.add_expiry_job(&db, appointment.id, now - 1).await;

    worker::run_once(&db, f.clock.as_ref(), &f.cfg, f.client.as_ref())
      .await
      .unwrap();
    let link = f.row(&db, appointment.id).await;
    assert!(link
      .try_get::<Option<i64>>("", "deactivated_at")
      .unwrap()
      .is_some());
    assert_eq!(
      link.try_get::<String>("", "outcome").unwrap(),
      "deactivated"
    );
    assert!(f
      .provider
      .requests
      .lock()
      .await
      .iter()
      .any(|request| request.path == "/v1/payment_links/plink_deadline"
        && request.form["active"] == "false"));
    let requests = f.provider.requests.lock().await;
    let deactivate = requests
      .iter()
      .find(|request| request.path == "/v1/payment_links/plink_deadline")
      .unwrap();
    assert_eq!(deactivate.stripe_account.as_deref(), Some(ACCOUNT));

    appointments::timers::run_once(&db, f.clock.as_ref(), &f.cfg)
      .await
      .unwrap();
    let state = appointment_state(&db, appointment.id).await;
    assert_eq!(state.0, "expired");
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
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn absent_stripe_configuration_keeps_transfer_instructions_and_makes_no_provider_calls() {
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
    f.schedule_link(&db, &appointment).await;
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, None)
      .await
      .unwrap();

    assert!(f.provider.requests.lock().await.is_empty());
    assert!(db
      .conn
      .query_one(sql(
        "SELECT 1 FROM stripe_payment_links WHERE appointment_id=$1",
        vec![appointment.id.into()],
      ))
      .await
      .unwrap()
      .is_none());
    let outbox = outbox_for(&db, appointment.id).await;
    assert!(outbox
      .iter()
      .any(|(purpose, state, text)| purpose == "status"
        && state == "queued"
        && text.contains("Banco de prueba")));
    assert_eq!(
      appointment_state(&db, appointment.id).await.0,
      "awaiting_payment"
    );
  })
  .await;
}

#[test]
fn malformed_stripe_key_fails_startup_configuration_without_echoing_the_key() {
  const CHILD: &str = "MEDICAL_STRIPE_CONFIG_FAILURE_CHILD";
  const TEST_NAME: &str =
    "malformed_stripe_key_fails_startup_configuration_without_echoing_the_key";
  if std::env::var(CHILD).as_deref() == Ok("1") {
    let _ = crate::common::config::Configuration::new();
    panic!("startup unexpectedly accepted a malformed Stripe key");
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
    .env("DATABASE_URL", "postgres://test:test@127.0.0.1/stripe_test")
    .env("PUBLIC_ORIGIN", "https://practice.test")
    .env("STRIPE_SECRET_KEY", "bad-key-sentinel")
    .output()
    .unwrap();
  assert!(!output.status.success());
  let diagnostic = format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(diagnostic.contains("Invalid Stripe configuration"));
  assert!(!diagnostic.contains("bad-key-sentinel"));
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
          ("expected_probe_id", ""),
        ],
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
  let stripe = Fixture::with_session(
    db,
    &mut google.cfg,
    google.clock.clone(),
    google.session.clone(),
    true,
  )
  .await;
  stripe.set_ready(db).await;
  let date = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(stripe.clock.now_millis())
    .unwrap()
    .date_naive()
    + chrono::Duration::days(3);
  let starts_at = date
    .and_hms_opt(10, 0, 0)
    .unwrap()
    .and_utc()
    .timestamp_millis();
  (stripe, google, service_id, starts_at)
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

async fn outbox_for(db: &crate::database::Db, id: Uuid) -> Vec<(String, String, String)> {
  db.conn.query_all(sql(
    "SELECT purpose,state,rendered_text FROM outbox_messages WHERE appointment_id=$1 ORDER BY created_at,id",
    vec![id.into()],
  )).await.unwrap().iter().map(|row| (
    row.try_get("", "purpose").unwrap(),
    row.try_get("", "state").unwrap(),
    row.try_get("", "rendered_text").unwrap(),
  )).collect()
}
