use super::*;
use crate::{
  app::tests::with_database,
  modules::doctor::tests::{fixture, NOW},
};
use chrono::TimeZone;
use sea_orm::{ConnectionTrait, TransactionTrait};

async fn add_contact(db: &Db, digits: &str, now: i64) -> (Uuid, String, String) {
  let contact = Uuid::now_v7();
  let jid = format!("{digits}@c.us");
  let inbox = format!("timer-test-{digits}-{contact}");
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
        inbox.clone().into(),
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
      vec![jid.clone().into(), contact.into(), inbox.clone().into()],
    ))
    .await
    .unwrap();
  (contact, jid, inbox)
}

async fn verify_doctor(db: &Db, now: i64) -> Uuid {
  let (contact, _, inbox) = add_contact(db, "15550222222", now).await;
  db.conn
    .execute(sql(
      "UPDATE doctor_number_state SET verified_phone=(SELECT resolved_phone_identity FROM inbox_messages WHERE message_id=$1 AND contact_id=$2),original_target=(SELECT original_chat_identity FROM inbox_messages WHERE message_id=$1 AND contact_id=$2),contact_id=$2,inbox_message_id=$1,verified_at=$3,practice_phone='525555555555@c.us' WHERE id=1",
      vec![inbox.into(), contact.into(), now.into()],
    ))
    .await
    .unwrap();
  contact
}

async fn add_appointment(
  db: &Db,
  contact: Uuid,
  now: i64,
  starts_at: i64,
  state: (&str, &str, &str),
  deadline: Option<i64>,
  meet_url: Option<&str>,
) -> Uuid {
  let (status, payment, format) = state;
  let id = Uuid::now_v7();
  db.conn
  .execute(sql(
    "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,meet_url,revision,receipt_deadline,created_at,updated_at) VALUES ($1,$2,'Paciente de prueba',$3,'Consulta general',45,100000,$4,$5,$6,'America/Tijuana','Consultorio de prueba',0,0,$7,$8,'synced',$9,1,$10,$11,$11)",
    vec![
      id.into(),
      contact.into(),
      Uuid::now_v7().into(),
      format.to_owned().into(),
      starts_at.into(),
      (starts_at + 2_700_000).into(),
      status.to_owned().into(),
      payment.to_owned().into(),
      meet_url.map(str::to_owned).into(),
      deadline.into(),
      now.into(),
    ],
  ))
  .await
  .unwrap();
  id
}

async fn add_hold(db: &Db, appointment: Uuid, starts_at: i64, expires_at: Option<i64>) {
  db.conn
    .execute(sql(
      "INSERT INTO appointment_holds(id,appointment_id,starts_at,ends_at,active,expires_at) VALUES ($1,$2,$3,$4,true,$5)",
      vec![
        Uuid::now_v7().into(),
        appointment.into(),
        starts_at.into(),
        (starts_at + 2_700_000).into(),
        expires_at.into(),
      ],
    ))
    .await
    .unwrap();
}

async fn add_receipt(
  db: &Db,
  contact: Uuid,
  target: &str,
  appointment: Uuid,
  sent_at: i64,
  arrived_at: i64,
  reviewed_at: Option<i64>,
) {
  let inbox = format!("timer-receipt-{appointment}-{}", Uuid::now_v7());
  db.conn
    .execute(sql(
      "INSERT INTO inbox_messages(message_id,original_chat_identity,resolved_phone_identity,server_timestamp,arrived_at,raw_envelope,contact_id) VALUES ($1,$2,$2,$3,to_timestamp($4::double precision/1000.0),$5,$6)",
      vec![
        inbox.clone().into(),
        target.to_owned().into(),
        sent_at.div_euclid(1000).into(),
        arrived_at.into(),
        b"{}".to_vec().into(),
        contact.into(),
      ],
    ))
    .await
    .unwrap();
  db.conn
    .execute(sql(
      "INSERT INTO receipts(id,contact_id,inbox_message_id,appointment_id,mime,bytes,sent_at,arrived_at,timely,late,reviewed_at,review_outcome,created_at) VALUES ($1,$2,$3,$4,'application/pdf',decode('25504446','hex'),$5,$6,true,false,$7,CASE WHEN $7 IS NULL THEN NULL ELSE 'verified' END,$6)",
      vec![
        Uuid::now_v7().into(),
        contact.into(),
        inbox.into(),
        appointment.into(),
        sent_at.into(),
        arrived_at.into(),
        reviewed_at.into(),
      ],
    ))
    .await
    .unwrap();
}

async fn add_job(db: &Db, appointment: Uuid, kind: &str, due: i64, key: String) {
  db.conn
    .execute(sql(
      "INSERT INTO appointment_jobs(id,appointment_id,kind,due_at,dedupe_key) VALUES ($1,$2,$3,$4,$5)",
      vec![
        Uuid::now_v7().into(),
        appointment.into(),
        kind.to_owned().into(),
        due.into(),
        key.into(),
      ],
    ))
    .await
    .unwrap();
}

#[test]
fn video_reminder_requires_verified_payment_and_a_validated_meet_url() {
  let mut appointment = AppointmentState {
    contact_id: Uuid::now_v7(),
    patient_name: "Paciente".into(),
    format: "video".into(),
    starts_at: NOW * 1000,
    timezone: "America/Tijuana".into(),
    location: String::new(),
    status: "confirmed".into(),
    payment: "verified".into(),
    calendar_sync: "synced".into(),
    meet_url: Some("https://meet.google.com/abc-def-ghi".into()),
    revision: 1,
    receipt_deadline: None,
  };
  let text = appointment_reminder(&appointment).unwrap();
  assert!(text.contains("https://meet.google.com/abc-def-ghi"));
  assert!(text.contains("responda CONFIRMO"));
  assert!(text.contains("America/Tijuana"));

  appointment.calendar_sync = "pending".into();
  assert!(appointment_reminder(&appointment).is_none());
  let pending = appointment_status(&appointment).unwrap();
  assert!(pending.contains("sincronización"));
  assert!(!pending.contains("CONFIRMO"));
  assert!(!pending.contains("https://"));

  appointment.calendar_sync = "synced".into();
  appointment.meet_url = Some("https://meet.google.com/invalid".into());
  let pending = appointment_status(&appointment).unwrap();
  assert!(pending.contains("pendiente de validación"));
  assert!(!pending.contains("https://meet.google.com/invalid"));

  appointment.meet_url = Some("https://meet.google.com/abc-def-ghi".into());
  appointment.payment = "under_review".into();
  assert!(appointment_reminder(&appointment).is_none());
  let pending = appointment_status(&appointment).unwrap();
  assert!(pending.contains("comprobante en revisión"));
  assert!(!pending.contains("https://"));
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn expiry_runs_at_the_hold_boundary_and_commits_release_history_and_message() {
  with_database(|db| async move {
    let (cfg, clock, _, _) = fixture(&db).await;
    let now = clock.now_millis();
    let (contact, _, _) = add_contact(&db, "15550111111", now).await;
    let appointment = add_appointment(&db, contact, now, now + 86_400_000, ("awaiting_payment", "awaiting_receipt", "video"), Some(now + 30_000), None)
    .await;
    let expiry = now + 1_000;
    add_hold(&db, appointment, now + 86_400_000, Some(expiry)).await;
    add_job(
      &db,
      appointment,
      "expiry",
      expiry,
      format!("{appointment}:1:expiry:deadline"),
    )
    .await;
    let protected = add_appointment(&db, contact, now, now + 172_800_000, ("awaiting_payment", "under_review", "video"), Some(expiry), None)
    .await;
    add_hold(&db, protected, now + 172_800_000, None).await;
    add_job(
      &db,
      protected,
      "expiry",
      expiry,
      format!("{protected}:1:expiry:deadline"),
    )
    .await;

    assert!(!run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    clock.advance(1);
    for _ in 0..2 {
      assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    }
    let state = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.revision,h.active,(SELECT count(*) FROM appointment_history WHERE appointment_id=a.id AND kind='expired') AS history,(SELECT count(*) FROM outbox_messages WHERE appointment_id=a.id AND purpose='status') AS messages FROM appointments a JOIN appointment_holds h ON h.appointment_id=a.id WHERE a.id=$1",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(state.try_get::<String>("", "status").unwrap(), "expired");
    assert_eq!(state.try_get::<i64>("", "revision").unwrap(), 2);
    assert!(!state.try_get::<bool>("", "active").unwrap());
    assert_eq!(state.try_get::<i64>("", "history").unwrap(), 1);
    assert_eq!(state.try_get::<i64>("", "messages").unwrap(), 1);
    let text: String = db
      .conn
      .query_one(sql(
        "SELECT rendered_text FROM outbox_messages WHERE appointment_id=$1 AND purpose='status'",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "rendered_text")
      .unwrap();
    assert!(text.contains("el horario se liberó"));
    let protected_state = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.payment,h.active,j.finished_at FROM appointments a JOIN appointment_holds h ON h.appointment_id=a.id JOIN appointment_jobs j ON j.appointment_id=a.id WHERE a.id=$1",
        vec![protected.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      protected_state.try_get::<String>("", "status").unwrap(),
      "awaiting_payment"
    );
    assert_eq!(
      protected_state.try_get::<String>("", "payment").unwrap(),
      "under_review"
    );
    assert!(protected_state.try_get::<bool>("", "active").unwrap());
    assert!(protected_state
      .try_get::<Option<i64>>("", "finished_at")
      .unwrap()
      .is_some());
    let helper_now = clock.now_millis();
    let helper_appointment = add_appointment(&db, contact, helper_now, helper_now + 259_200_000, ("awaiting_payment", "awaiting_receipt", "video"), Some(helper_now), None)
    .await;
    add_hold(
      &db,
      helper_appointment,
      helper_now + 259_200_000,
      Some(helper_now),
    )
    .await;
    add_job(
      &db,
      helper_appointment,
      "expiry",
      helper_now,
      format!("{helper_appointment}:1:expiry:deadline"),
    )
    .await;
    let tx = db.conn.begin().await.unwrap();
    let origin = cfg.public_origin.as_deref().unwrap();
    assert!(matches!(
      expire_if_due(&tx, helper_appointment, origin, helper_now)
        .await
        .unwrap(),
      ExpiryCheck::Expired
    ));
    tx.commit().await.unwrap();
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn timely_pending_media_defers_expiry_only_until_copy_resolves() {
  with_database(|db| async move {
    let (cfg, clock, _, _) = fixture(&db).await;
    let now = clock.now_millis();
    let (contact, _jid, inbox) = add_contact(&db, "15550111111", now - 100).await;
    let deadline = now - 1_000;
    let appointment = add_appointment(&db, contact, now, now + 86_400_000, ("awaiting_payment", "awaiting_receipt", "video"), Some(deadline), None)
    .await;
    add_hold(&db, appointment, now + 86_400_000, Some(now)).await;
    add_job(
      &db,
      appointment,
      "expiry",
      now,
      format!("{appointment}:1:expiry:deadline"),
    )
    .await;
    db.conn
      .execute(sql(
        "INSERT INTO receipt_intake_state(inbox_message_id,state,sent_at,arrived_at,updated_at) VALUES ($1,'pending',$2,$3,$3)",
        vec![
          inbox.clone().into(),
          deadline.into(),
          (now - PENDING_RECEIPT_MAX_AGE_MS).into(),
        ],
      ))
      .await
      .unwrap();

    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    let deferred = db
      .conn
      .query_one(sql(
        "SELECT a.status,h.active,j.due_at,j.finished_at FROM appointments a JOIN appointment_holds h ON h.appointment_id=a.id JOIN appointment_jobs j ON j.appointment_id=a.id WHERE a.id=$1",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      deferred.try_get::<String>("", "status").unwrap(),
      "awaiting_payment"
    );
    assert!(deferred.try_get::<bool>("", "active").unwrap());
    assert_eq!(deferred.try_get::<i64>("", "due_at").unwrap(), now + 5_000);
    assert_eq!(deferred.try_get::<Option<i64>>("", "finished_at").unwrap(), None);

    db.conn
      .execute(sql(
        "UPDATE receipt_intake_state SET state='complete',updated_at=$2 WHERE inbox_message_id=$1",
        vec![inbox.into(), (now + 5_000).into()],
      ))
      .await
      .unwrap();
    clock.advance(5);
    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    let status: String = db
      .conn
      .query_one(sql(
        "SELECT status FROM appointments WHERE id=$1",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "status")
      .unwrap();
    assert_eq!(status, "expired");
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn rejected_payment_expires_at_the_original_due_without_pending_media_grace() {
  with_database(|db| async move {
    let (cfg, clock, _, _) = fixture(&db).await;
    let now = clock.now_millis();
    let (contact, _, _) = add_contact(&db, "15550111111", now).await;
    let appointment = add_appointment(&db, contact, now, now + 3_600_000, ("awaiting_payment", "rejected", "video"), Some(now - 60_000), None)
    .await;
    add_hold(&db, appointment, now + 3_600_000, None).await;
    add_job(
      &db,
      appointment,
      "expiry",
      now,
      format!("{appointment}:1:expiry:deadline"),
    )
    .await;
    db.conn
      .execute(sql(
        "UPDATE appointments SET revision=2,updated_at=$2 WHERE id=$1",
        vec![appointment.into(), now.into()],
      ))
      .await
      .unwrap();

    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    let state = db
      .conn
      .query_one(sql(
        "SELECT a.status,a.revision,h.active,(SELECT count(*) FROM appointment_history WHERE appointment_id=a.id AND kind='expired') AS history,(SELECT rendered_text FROM outbox_messages WHERE appointment_id=a.id AND purpose='status') AS message FROM appointments a JOIN appointment_holds h ON h.appointment_id=a.id WHERE a.id=$1",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(state.try_get::<String>("", "status").unwrap(), "expired");
    assert_eq!(state.try_get::<i64>("", "revision").unwrap(), 3);
    assert!(!state.try_get::<bool>("", "active").unwrap());
    assert_eq!(state.try_get::<i64>("", "history").unwrap(), 1);
    let text: String = state.try_get("", "message").unwrap();
    assert!(text.contains("comprobante fue rechazado"));
    assert!(!text.contains("No recibimos un comprobante"));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn confirmed_reminder_invites_attendance_but_unconfirmed_citation_gets_status_only() {
  with_database(|db| async move {
    let (cfg, clock, _, _) = fixture(&db).await;
    let now = clock.now_millis();
    let (contact, _, _) = add_contact(&db, "15550111111", now).await;
    let confirmed = add_appointment(&db, contact, now, now + 86_400_000, ("confirmed", "not_required", "in_person"), None, None)
    .await;
    add_job(
      &db,
      confirmed,
      "reminders",
      now,
      format!("{confirmed}:1:reminders:24h:{}", now + 86_400_000),
    )
    .await;
    db.conn
      .execute(sql(
        "UPDATE appointments SET revision=2,updated_at=$2 WHERE id=$1",
        vec![confirmed.into(), now.into()],
      ))
      .await
      .unwrap();
    add_job(
      &db,
      confirmed,
      "reminders",
      now,
      format!("{confirmed}:2:reminders:24h:{}", now + 86_400_000),
    )
    .await;
    let moved_start = now + 86_400_000;
    let moved = add_appointment(&db, contact, now, moved_start, ("confirmed", "not_required", "in_person"), None, None)
    .await;
    add_job(
      &db,
      moved,
      "reminders",
      now,
      format!("{moved}:1:reminders:24h:{moved_start}"),
    )
    .await;
    let new_start = now + 90_000_000;
    db.conn
      .execute(sql(
        "UPDATE appointments SET starts_at=$2,ends_at=$3,revision=2,updated_at=$4 WHERE id=$1",
        vec![
          moved.into(),
          new_start.into(),
          (new_start + 2_700_000).into(),
          now.into(),
        ],
      ))
      .await
      .unwrap();
    let under_review = add_appointment(&db, contact, now, now + 86_400_000, ("awaiting_payment", "under_review", "video"), Some(now + 3_600_000), None)
    .await;
    add_job(
      &db,
      under_review,
      "reminders",
      now,
      format!("{under_review}:1:reminders:24h:{}", now + 86_400_000),
    )
    .await;
    let disabled_2h = add_appointment(&db, contact, now, now + 7_200_000, ("confirmed", "not_required", "in_person"), None, None)
    .await;
    add_job(
      &db,
      disabled_2h,
      "reminders",
      now,
      format!("{disabled_2h}:1:reminders:2h:{}", now + 7_200_000),
    )
    .await;
    let elapsed = add_appointment(&db, contact, now, now, ("confirmed", "not_required", "in_person"), None, None)
    .await;
    add_job(
      &db,
      elapsed,
      "reminders",
      now,
      format!("{elapsed}:1:reminders:24h:{now}"),
    )
    .await;

    for _ in 0..6 {
      assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    }
    db.conn
      .execute_unprepared("UPDATE practice SET reminder_2h_enabled=true WHERE id=1")
      .await
      .unwrap();
    let enabled_2h = add_appointment(&db, contact, now, now + 7_200_000, ("confirmed", "not_required", "in_person"), None, None)
    .await;
    add_job(
      &db,
      enabled_2h,
      "reminders",
      now,
      format!("{enabled_2h}:1:reminders:2h:{}", now + 7_200_000),
    )
    .await;
    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());

    let rows = db
      .conn
      .query_all(sql(
        "SELECT appointment_id,purpose,rendered_text,expected_revision,dedupe_key FROM outbox_messages WHERE appointment_id IN ($1,$2,$3) ORDER BY appointment_id",
        vec![confirmed.into(), under_review.into(), enabled_2h.into()],
      ))
      .await
      .unwrap();
    assert_eq!(rows.len(), 3);
    for row in rows {
      let id: Uuid = row.try_get("", "appointment_id").unwrap();
      let purpose: String = row.try_get("", "purpose").unwrap();
      let text: String = row.try_get("", "rendered_text").unwrap();
      let dedupe_key: String = row.try_get("", "dedupe_key").unwrap();
      assert_eq!(
        row.try_get::<Option<i64>>("", "expected_revision").unwrap(),
        Some(if id == confirmed { 2 } else { 1 })
      );
      if id == confirmed {
        assert_eq!(
          dedupe_key,
          format!("appointment:{confirmed}:reminders:{}:24h", now + 86_400_000)
        );
        assert_eq!(purpose, "reminder");
        assert!(text.contains("Consultorio de prueba"));
        assert!(text.contains("responda CONFIRMO"));
      } else if id == enabled_2h {
        assert_eq!(
          dedupe_key,
          format!("appointment:{enabled_2h}:reminders:{}:2h", now + 7_200_000)
        );
        assert_eq!(purpose, "reminder");
        assert!(text.contains("responda CONFIRMO"));
      } else {
        assert_eq!(id, under_review);
        assert_eq!(
          dedupe_key,
          format!("appointment:{under_review}:reminders:{}:24h", now + 86_400_000)
        );
        assert_eq!(purpose, "status");
        assert!(text.contains("comprobante en revisión"));
        assert!(!text.contains("CONFIRMO"));
        assert!(!text.contains("meet.google.com"));
      }
    }
    let omitted = db
      .conn
      .query_one(sql(
        "SELECT count(*) AS n FROM outbox_messages WHERE appointment_id IN ($1,$2,$3)",
        vec![disabled_2h.into(), elapsed.into(), moved.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get::<i64>("", "n")
      .unwrap();
    assert_eq!(omitted, 0);
    let attendance = db
      .conn
      .query_one(sql(
        "SELECT count(*) AS confirmed FROM appointments WHERE id IN ($1,$2,$3,$4,$5,$6) AND attendance_confirmed_at IS NOT NULL",
        vec![
          confirmed.into(),
          under_review.into(),
          disabled_2h.into(),
          enabled_2h.into(),
          elapsed.into(),
          moved.into(),
        ],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get::<i64>("", "confirmed")
      .unwrap();
    assert_eq!(attendance, 0);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn periodic_review_waits_for_working_hours_and_urgent_start_boundary_keeps_review_state() {
  with_database(|db| async move {
    let (cfg, clock, _, _) = fixture(&db).await;
    let now = clock.now_millis();
    let (owner, owner_target, _) = add_contact(&db, "15550111111", now).await;
    let doctor = verify_doctor(&db, now).await;
    let friday_hours = serde_json::json!([
      {"status":"closed"},
      {"status":"closed"},
      {"status":"closed"},
      {"status":"closed"},
      {"status":"open","ranges":[{"start":540,"end":1020}]},
      {"status":"closed"},
      {"status":"closed"}
    ]);
    db.conn
      .execute(sql(
        "UPDATE practice SET weekly_hours=$1,timezone='America/Tijuana',review_interval_minutes=240,urgent_notice_minutes=120 WHERE id=1",
        vec![friday_hours.into()],
      ))
      .await
      .unwrap();
    let review = add_appointment(&db, owner, now, now + 172_800_000, ("awaiting_payment", "under_review", "video"), Some(now), None)
    .await;
    add_hold(&db, review, now + 172_800_000, None).await;
    add_receipt(&db, owner, &owner_target, review, now - 10_000, now - 5_000, None).await;
    add_job(
      &db,
      review,
      "review",
      now,
      format!("{review}:1:review:periodic"),
    )
    .await;
    let tz = "America/Tijuana".parse::<chrono_tz::Tz>().unwrap();
    let local_now = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now)
      .unwrap()
      .with_timezone(&tz);
    let opening = tz
      .from_local_datetime(&local_now.date_naive().and_hms_opt(9, 0, 0).unwrap())
      .single()
      .unwrap()
      .timestamp_millis();

    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    assert_eq!(
      db.conn
        .query_one(sql(
          "SELECT due_at FROM appointment_jobs WHERE appointment_id=$1 AND kind='review'",
          vec![review.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "due_at")
        .unwrap(),
      opening
    );
    assert_eq!(
      db.conn
        .query_one(sql(
          "SELECT count(*) AS n FROM outbox_messages WHERE appointment_id=$1",
          vec![review.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap(),
      0
    );
    clock.advance((opening - now).div_euclid(1000));
    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    let notification = db
      .conn
      .query_one(sql(
        "SELECT contact_id,original_target,purpose,rendered_text FROM outbox_messages WHERE appointment_id=$1",
        vec![review.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(notification.try_get::<Uuid>("", "contact_id").unwrap(), doctor);
    assert_eq!(
      notification.try_get::<String>("", "original_target").unwrap(),
      "15550222222@c.us"
    );
    assert_eq!(
      notification.try_get::<String>("", "purpose").unwrap(),
      "doctor_notification"
    );
    assert!(notification
      .try_get::<String>("", "rendered_text")
      .unwrap()
      .contains(&format!("/appointments/{review}")));
    assert_eq!(
      db.conn
        .query_one(sql(
          "SELECT count(*) AS n FROM appointment_jobs WHERE appointment_id=$1 AND kind='review' AND finished_at IS NULL",
          vec![review.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap(),
      1
    );

    let starts_now = clock.now_millis();
    let at_start = add_appointment(&db, owner, starts_now, starts_now, ("awaiting_payment", "under_review", "video"), Some(starts_now - 1), None)
    .await;
    add_hold(&db, at_start, starts_now, None).await;
    add_receipt(&db, owner, &owner_target, at_start, starts_now - 10_000, starts_now - 5_000, None).await;
    add_job(
      &db,
      at_start,
      "review",
      starts_now,
      format!("{at_start}:1:review:urgent"),
    )
    .await;
    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    let state = db
      .conn
      .query_one(sql(
        "SELECT status,payment,meet_url FROM appointments WHERE id=$1",
        vec![at_start.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(state.try_get::<String>("", "status").unwrap(), "awaiting_payment");
    assert_eq!(state.try_get::<String>("", "payment").unwrap(), "under_review");
    assert_eq!(state.try_get::<Option<String>>("", "meet_url").unwrap(), None);
    assert_eq!(
      db.conn
        .query_one(sql(
          "SELECT count(*) AS n FROM outbox_messages WHERE appointment_id=$1",
          vec![at_start.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap(),
      0
    );
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn urgent_review_is_sent_two_hours_before_when_closed_but_not_at_start_or_after_resolution() {
  with_database(|db| async move {
    let (cfg, clock, _, _) = fixture(&db).await;
    let now = clock.now_millis();
    let (owner, owner_target, _) = add_contact(&db, "15550111111", now).await;
    let doctor = verify_doctor(&db, now).await;
    db.conn
      .execute(sql(
        "UPDATE practice SET weekly_hours='[{\"status\":\"closed\"},{\"status\":\"closed\"},{\"status\":\"closed\"},{\"status\":\"closed\"},{\"status\":\"closed\"},{\"status\":\"closed\"},{\"status\":\"closed\"}]'::jsonb WHERE id=1",
        vec![],
      ))
      .await
      .unwrap();
    let urgent = add_appointment(&db, owner, now, now + 7_200_000, ("awaiting_payment", "under_review", "video"), Some(now - 1), None)
    .await;
    add_hold(&db, urgent, now + 7_200_000, None).await;
    add_receipt(&db, owner, &owner_target, urgent, now - 10_000, now - 5_000, None).await;
    add_job(
      &db,
      urgent,
      "review",
      now,
      format!("{urgent}:1:review:urgent"),
    )
    .await;
    let at_start = add_appointment(&db, owner, now, now, ("awaiting_payment", "under_review", "video"), Some(now - 1), None)
    .await;
    add_hold(&db, at_start, now, None).await;
    add_receipt(&db, owner, &owner_target, at_start, now - 10_000, now - 5_000, None).await;
    add_job(
      &db,
      at_start,
      "review",
      now,
      format!("{at_start}:1:review:urgent"),
    )
    .await;
    let resolved = add_appointment(&db, owner, now, now + 14_400_000, ("awaiting_payment", "under_review", "video"), Some(now + 3_600_000), None)
    .await;
    add_hold(&db, resolved, now + 14_400_000, None).await;
    add_receipt(&db, owner, &owner_target, resolved, now - 10_000, now - 5_000, None).await;
    add_job(
      &db,
      resolved,
      "review",
      now + 7_200_000,
      format!("{resolved}:1:review:urgent"),
    )
    .await;
    db.conn
      .execute(sql(
        "UPDATE receipts SET reviewed_at=$2,review_outcome='verified' WHERE appointment_id=$1",
        vec![resolved.into(), now.into()],
      ))
      .await
      .unwrap();
    db.conn
      .execute(sql(
        "UPDATE appointments SET status='confirmed',payment='verified',meet_url='https://meet.google.com/abc-def-ghi',revision=2,updated_at=$2 WHERE id=$1",
        vec![resolved.into(), now.into()],
      ))
      .await
      .unwrap();

    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    let notice = db
      .conn
      .query_one(sql(
        "SELECT contact_id,purpose,rendered_text FROM outbox_messages WHERE appointment_id=$1",
        vec![urgent.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(notice.try_get::<Uuid>("", "contact_id").unwrap(), doctor);
    assert_eq!(
      notice.try_get::<String>("", "purpose").unwrap(),
      "doctor_notification"
    );
    assert!(notice
      .try_get::<String>("", "rendered_text")
      .unwrap()
      .contains("Aviso urgente"));
    clock.advance(7_200);
    assert!(run_once(&db, clock.as_ref(), &cfg).await.unwrap());
    assert_eq!(
      db.conn
        .query_one(sql(
          "SELECT count(*) AS n FROM appointments a JOIN receipts r ON r.appointment_id=a.id WHERE a.id=$1 AND a.status='confirmed' AND a.payment='verified' AND r.bytes IS NOT NULL AND r.reviewed_at IS NOT NULL AND r.review_outcome='verified'",
          vec![resolved.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap(),
      1
    );
    assert_eq!(
      db.conn
        .query_one(sql(
          "SELECT count(*) AS n FROM outbox_messages WHERE appointment_id IN ($1,$2)",
          vec![at_start.into(), resolved.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap(),
      0
    );
    assert_eq!(
      db.conn
        .query_one(sql(
          "SELECT status,payment,meet_url FROM appointments WHERE id=$1",
          vec![at_start.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<String>("", "status")
        .unwrap(),
      "awaiting_payment"
    );
    assert_eq!(
      db.conn
        .query_one(sql(
          "SELECT payment FROM appointments WHERE id=$1",
          vec![at_start.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<String>("", "payment")
        .unwrap(),
      "under_review"
    );
    assert_eq!(
      db.conn
        .query_one(sql(
          "SELECT count(*) AS n FROM outbox_messages WHERE appointment_id=$1",
          vec![at_start.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap(),
      0
    );
  })
  .await;
}
