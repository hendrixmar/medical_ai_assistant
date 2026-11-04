use crate::{
  common::config::Config,
  database::Db,
  modules::{
    appointments::{model::Appointment, store as appointment_store},
    conversation::store::{enqueue_text, Message},
    doctor::Clock,
    payments::actions::{confirm_appointment_payment, PaymentConfirmation},
    whatsapp::{bound_transaction, inbox::sql},
  },
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use uuid::Uuid;

use super::{
  client::{Client, Payment, PreferenceRequest},
  Error,
};

const CLAIM_MS: i64 = 30_000;
const POLL_INTERVAL_MS: i64 = 30_000;
const RETRY_MS: i64 = 60_000;
const LATE_CHECK_MS: i64 = 15 * 60_000;
const LATE_WINDOW_MS: i64 = 2 * 3_600_000;
const RECHECK_MS: i64 = 86_400_000;
const RECHECK_AFTER_START_MS: i64 = 7 * 86_400_000;
const REFUND_CHECK_MS: i64 = 60_000;
const ACCOUNT_OK_MS: i64 = 6 * 3_600_000;
const ACCOUNT_RETRY_MS: i64 = 5 * 60_000;
const BATCH_SIZE: usize = 3;

struct Job {
  id: Uuid,
  appointment_id: Uuid,
  key: String,
  token: Uuid,
}

struct Preference {
  appointment_id: Uuid,
  revision: i64,
  external_reference: String,
  preference_id: String,
  amount_centavos: i64,
  expires_at: i64,
  closed_at: Option<i64>,
  outcome: String,
}

pub async fn run_once(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
) -> Result<(), DbErr> {
  let (Some(_), Some(client), Some(origin)) = (
    cfg.mercado_pago.as_ref(),
    client,
    cfg.public_origin.as_deref(),
  ) else {
    return Ok(());
  };
  let Some(collector) = check_account(db, clock, client).await? else {
    return Ok(());
  };
  for _ in 0..BATCH_SIZE {
    let Some(preference) = claim_preference(db, clock).await? else {
      break;
    };
    process_preference(db, clock, client, origin, collector, preference).await?;
  }
  if let Some(job) = claim_job(db, clock, "mp_preference").await? {
    process_preference_job(db, clock, client, origin, job).await?;
  }
  if let Some(job) = claim_job(db, clock, "mp_refund").await? {
    process_refund_job(db, clock, client, origin, job).await?;
  }
  for _ in 0..BATCH_SIZE {
    if !recheck_payment(db, clock, client, origin).await? {
      break;
    }
  }
  Ok(())
}

/// Returns the token owner's user id while the token is accepted.
async fn check_account(db: &Db, clock: &dyn Clock, client: &Client) -> Result<Option<i64>, DbErr> {
  let now = clock.now_millis();
  let row = db
    .conn
    .query_one(sql(
      "SELECT user_id,checked_at,notice FROM mercado_pago_account WHERE id=1",
      vec![],
    ))
    .await?;
  let (user_id, checked_at, notice) = match &row {
    Some(row) => (
      row.try_get::<Option<i64>>("", "user_id")?,
      row.try_get::<i64>("", "checked_at")?,
      row.try_get::<Option<String>>("", "notice")?,
    ),
    None => (None, i64::MIN, None),
  };
  let interval = if notice.is_none() && user_id.is_some() {
    ACCOUNT_OK_MS
  } else {
    ACCOUNT_RETRY_MS
  };
  if checked_at.saturating_add(interval) > now {
    return Ok(user_id.filter(|_| notice.is_none()));
  }
  let result = client.user_id().await;
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let (user_id, notice, checked_at) = match result {
    Ok(id) => (Some(id), None, now),
    Err(Error::Rejected) => (user_id, Some("unauthorized"), now),
    // Transient: keep the last verdict and retry in ACCOUNT_RETRY_MS.
    Err(_) if notice.is_none() && user_id.is_some() => (
      user_id,
      None,
      now
        .saturating_sub(ACCOUNT_OK_MS)
        .saturating_add(ACCOUNT_RETRY_MS),
    ),
    Err(_) => (user_id, Some("unavailable"), now),
  };
  tx.execute(sql(
    "INSERT INTO mercado_pago_account(id,user_id,checked_at,notice,created_at,updated_at) VALUES (1,$1,$2,$3,$4,$4) ON CONFLICT (id) DO UPDATE SET user_id=EXCLUDED.user_id,checked_at=EXCLUDED.checked_at,notice=EXCLUDED.notice,updated_at=EXCLUDED.updated_at",
    vec![user_id.into(), checked_at.into(), notice.map(str::to_owned).into(), now.into()],
  ))
  .await?;
  if notice == Some("unauthorized") {
    appointment_store::attention(&tx, None, None, "mercado_pago_unauthorized", now).await?;
  } else if notice.is_none() {
    tx.execute(sql(
      "UPDATE attention_items SET resolved_at=$1 WHERE kind='mercado_pago_unauthorized' AND resolved_at IS NULL",
      vec![now.into()],
    ))
    .await?;
  }
  tx.commit().await?;
  Ok(user_id.filter(|_| notice.is_none()))
}

async fn claim_job(db: &Db, clock: &dyn Clock, kind: &str) -> Result<Option<Job>, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let row = tx
    .query_one(sql(
      "SELECT id,appointment_id,dedupe_key FROM appointment_jobs WHERE kind=$2 AND finished_at IS NULL AND due_at <= $1 AND (claim_until IS NULL OR claim_until <= $1) ORDER BY due_at,id LIMIT 1 FOR UPDATE SKIP LOCKED",
      vec![now.into(), kind.into()],
    ))
    .await?;
  let Some(row) = row else {
    tx.commit().await?;
    return Ok(None);
  };
  let id: Uuid = row.try_get("", "id")?;
  let token = Uuid::now_v7();
  tx.execute(sql(
    "UPDATE appointment_jobs SET claim=$2,claim_until=$3,attempts=attempts+1 WHERE id=$1",
    vec![id.into(), token.into(), now.saturating_add(CLAIM_MS).into()],
  ))
  .await?;
  let job = Job {
    id,
    appointment_id: row.try_get("", "appointment_id")?,
    key: row.try_get("", "dedupe_key")?,
    token,
  };
  tx.commit().await?;
  Ok(Some(job))
}

/// Parses `<appointment>:<number>:<kind>` job keys.
fn job_number(job: &Job, kind: &str) -> Option<i64> {
  let mut parts = job.key.split(':');
  if parts.next()?.parse::<Uuid>().ok()? != job.appointment_id {
    return None;
  }
  let number = parts.next()?.parse::<i64>().ok()?;
  (parts.next()? == kind && parts.next().is_none()).then_some(number)
}

async fn finish_job(db: &Db, job: &Job, now: i64) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  finish_job_tx(&tx, job, now).await?;
  tx.commit().await
}

async fn finish_job_tx(tx: &DatabaseTransaction, job: &Job, now: i64) -> Result<(), DbErr> {
  let result = tx
    .execute(sql(
      "UPDATE appointment_jobs SET finished_at=$2,claim=NULL,claim_until=NULL,last_error=NULL WHERE id=$1 AND claim=$3 AND finished_at IS NULL",
      vec![job.id.into(), now.into(), job.token.into()],
    ))
    .await?;
  if result.rows_affected() != 1 {
    return Err(DbErr::Custom("Mercado Pago job claim was lost".into()));
  }
  Ok(())
}

async fn retry_job(db: &Db, clock: &dyn Clock, job: &Job) -> Result<(), DbErr> {
  let result = db
    .conn
    .execute(sql(
      "UPDATE appointment_jobs SET due_at=$2,claim=NULL,claim_until=NULL WHERE id=$1 AND claim=$3 AND finished_at IS NULL",
      vec![
        job.id.into(),
        clock.now_millis().saturating_add(RETRY_MS).into(),
        job.token.into(),
      ],
    ))
    .await?;
  if result.rows_affected() != 1 {
    return Err(DbErr::Custom("Mercado Pago job claim was lost".into()));
  }
  Ok(())
}

fn eligible_for_payment(appointment: &Appointment, deadline: i64, now: i64) -> bool {
  appointment.format == "video"
    && appointment.status == "awaiting_payment"
    && matches!(
      appointment.payment.as_str(),
      "awaiting_receipt" | "under_review" | "rejected"
    )
    && appointment.receipt_deadline == Some(deadline)
    && now < deadline
}

async fn lock_appointment(
  tx: &DatabaseTransaction,
  appointment_id: Uuid,
) -> Result<Option<Appointment>, DbErr> {
  let Some(owner) = tx
    .query_one(sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![appointment_id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  let contact: Uuid = owner.try_get("", "contact_id")?;
  if tx
    .query_one(sql(
      "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
      vec![contact.into()],
    ))
    .await?
    .is_none()
  {
    return Err(DbErr::Custom("Appointment Contact is unavailable".into()));
  }
  let Some(row) = tx
    .query_one(sql(
      "SELECT * FROM appointments WHERE id=$1 FOR UPDATE",
      vec![appointment_id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  let appointment = Appointment::from_query_result(&row, "")?;
  if appointment.contact_id != contact {
    return Err(DbErr::Custom(
      "Appointment Contact changed while locking".into(),
    ));
  }
  Ok(Some(appointment))
}

// ---- Preference creation -------------------------------------------------------------------

async fn process_preference_job(
  db: &Db,
  clock: &dyn Clock,
  client: &Client,
  origin: &str,
  job: Job,
) -> Result<(), DbErr> {
  let now = clock.now_millis();
  let Some(revision) = job_number(&job, "mp_preference") else {
    return finish_job(db, &job, now).await;
  };
  let Some(row) = db
    .conn
    .query_one(sql(
      "SELECT external_reference,preference_id,amount_centavos,expires_at,outcome,closed_at FROM mercado_pago_preferences WHERE appointment_id=$1 AND revision=$2",
      vec![job.appointment_id.into(), revision.into()],
    ))
    .await?
  else {
    return finish_job(db, &job, now).await;
  };
  if row
    .try_get::<Option<String>>("", "preference_id")?
    .is_some()
  {
    return finish_job(db, &job, now).await;
  }
  let external_reference: String = row.try_get("", "external_reference")?;
  let amount_centavos: i64 = row.try_get("", "amount_centavos")?;
  let expires_at: i64 = row.try_get("", "expires_at")?;
  let open = row.try_get::<String>("", "outcome")? == "pending"
    && row.try_get::<Option<i64>>("", "closed_at")?.is_none();
  let appointment = appointment_store::get(&db.conn, job.appointment_id).await?;
  let Some(appointment) =
    appointment.filter(|appointment| open && eligible_for_payment(appointment, expires_at, now))
  else {
    return abandon_preference(db, &job, revision, now).await;
  };

  let local_start_time =
    appointment_store::local_time(appointment.starts_at, &appointment.timezone);
  let created = match client
    .create_preference(PreferenceRequest {
      appointment_id: appointment.id,
      revision,
      external_reference: &external_reference,
      amount_centavos,
      local_start_time: &local_start_time,
      created_at: now,
      expires_at,
    })
    .await
  {
    Ok(created) => created,
    Err(Error::Rejected | Error::Invalid) => {
      return abandon_preference(db, &job, revision, now).await
    }
    Err(Error::Unavailable) => return retry_job(db, clock, &job).await,
  };

  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let appointment = lock_appointment(&tx, job.appointment_id).await?;
  let Some(row) = tx
    .query_one(sql(
      "SELECT preference_id,outcome,closed_at FROM mercado_pago_preferences WHERE appointment_id=$1 AND revision=$2 FOR UPDATE",
      vec![job.appointment_id.into(), revision.into()],
    ))
    .await?
  else {
    finish_job_tx(&tx, &job, now).await?;
    return tx.commit().await;
  };
  if row
    .try_get::<Option<String>>("", "preference_id")?
    .is_some()
  {
    finish_job_tx(&tx, &job, now).await?;
    return tx.commit().await;
  }
  let live = row.try_get::<String>("", "outcome")? == "pending"
    && row.try_get::<Option<i64>>("", "closed_at")?.is_none()
    && appointment
      .as_ref()
      .is_some_and(|appointment| eligible_for_payment(appointment, expires_at, now));
  // A preference that lost its Appointment meanwhile is closed by the poll loop right away.
  tx.execute(sql(
    "UPDATE mercado_pago_preferences SET preference_id=$3,url=$4,outcome=$5,next_check_at=$6 WHERE appointment_id=$1 AND revision=$2 AND preference_id IS NULL",
    vec![
      job.appointment_id.into(),
      revision.into(),
      created.id.clone().into(),
      created.url.clone().into(),
      if live { "active" } else { "closing" }.into(),
      now.into(),
    ],
  ))
  .await?;
  if let Some(appointment) = appointment.filter(|_| live) {
    let key = format!(
      "appointment:{}:{}:mercado_pago_link",
      appointment.id, appointment.revision
    );
    let text = format!(
      "Pague con tarjeta o saldo de Mercado Pago aquí: {}",
      created.url
    );
    enqueue_text(
      &tx,
      Message {
        contact: appointment.contact_id,
        appointment: Some(appointment.id),
        key: &key,
        text: &text,
        purpose: "payment_details",
        revision: Some(appointment.revision),
      },
      origin,
      now,
    )
    .await?;
  }
  finish_job_tx(&tx, &job, now).await?;
  tx.commit().await
}

async fn abandon_preference(db: &Db, job: &Job, revision: i64, now: i64) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  tx.execute(sql(
    "UPDATE mercado_pago_preferences SET outcome='closed',closed_at=$3,next_check_at=NULL WHERE appointment_id=$1 AND revision=$2 AND preference_id IS NULL AND closed_at IS NULL",
    vec![job.appointment_id.into(), revision.into(), now.into()],
  ))
  .await?;
  finish_job_tx(&tx, job, now).await?;
  tx.commit().await
}

// ---- Polling and confirmation --------------------------------------------------------------

async fn claim_preference(db: &Db, clock: &dyn Clock) -> Result<Option<Preference>, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let Some(row) = tx
    .query_one(sql(
      "SELECT appointment_id,revision,external_reference,preference_id,amount_centavos,expires_at,closed_at,outcome FROM mercado_pago_preferences WHERE preference_id IS NOT NULL AND next_check_at <= $1 ORDER BY next_check_at LIMIT 1 FOR UPDATE SKIP LOCKED",
      vec![now.into()],
    ))
    .await?
  else {
    tx.commit().await?;
    return Ok(None);
  };
  let preference = Preference {
    appointment_id: row.try_get("", "appointment_id")?,
    revision: row.try_get("", "revision")?,
    external_reference: row.try_get("", "external_reference")?,
    preference_id: row.try_get("", "preference_id")?,
    amount_centavos: row.try_get("", "amount_centavos")?,
    expires_at: row.try_get("", "expires_at")?,
    closed_at: row.try_get("", "closed_at")?,
    outcome: row.try_get("", "outcome")?,
  };
  set_next_check(&tx, &preference, Some(now.saturating_add(CLAIM_MS))).await?;
  tx.commit().await?;
  Ok(Some(preference))
}

async fn set_next_check<C: ConnectionTrait>(
  conn: &C,
  preference: &Preference,
  due: Option<i64>,
) -> Result<(), DbErr> {
  conn
    .execute(sql(
      "UPDATE mercado_pago_preferences SET next_check_at=$3 WHERE appointment_id=$1 AND revision=$2",
      vec![
        preference.appointment_id.into(),
        preference.revision.into(),
        due.into(),
      ],
    ))
    .await?;
  Ok(())
}

async fn process_preference(
  db: &Db,
  clock: &dyn Clock,
  client: &Client,
  origin: &str,
  collector: i64,
  preference: Preference,
) -> Result<(), DbErr> {
  if let Ok(found) = client.search_payments(&preference.external_reference).await {
    for summary in found
      .into_iter()
      .filter(|payment| payment.status == "approved")
    {
      if payment_seen(db, summary.id).await? {
        continue;
      }
      // The search result only nominates; the payment itself is re-read before any decision.
      if let Ok(payment) = client.payment(summary.id).await {
        if payment.status == "approved" {
          record_payment(db, clock, &preference, &payment, collector, origin).await?;
        }
      }
    }
  }

  let now = clock.now_millis();
  if preference.outcome == "closed" {
    let closed_at = preference.closed_at.unwrap_or(now);
    let due = (now < closed_at.saturating_add(LATE_WINDOW_MS)).then(|| now + LATE_CHECK_MS);
    return set_next_check(&db.conn, &preference, due).await;
  }
  if preference.outcome == "closing" || needs_closing(db, &preference, now).await? {
    return close_preference(db, clock, client, &preference).await;
  }
  let due = now
    .saturating_add(POLL_INTERVAL_MS)
    .min(preference.expires_at);
  set_next_check(&db.conn, &preference, Some(due)).await
}

async fn payment_seen(db: &Db, payment_id: i64) -> Result<bool, DbErr> {
  Ok(
    db.conn
      .query_one(sql(
        "SELECT 1 AS seen FROM mercado_pago_payments WHERE payment_id=$1",
        vec![payment_id.into()],
      ))
      .await?
      .is_some(),
  )
}

async fn needs_closing(db: &Db, preference: &Preference, now: i64) -> Result<bool, DbErr> {
  let row = db
    .conn
    .query_one(sql(
      "SELECT (p.expires_at <= $3 OR a.id IS NULL OR a.status<>'awaiting_payment' OR a.payment NOT IN ('awaiting_receipt','under_review','rejected') OR a.format<>'video' OR a.receipt_deadline IS DISTINCT FROM p.expires_at) AS needed FROM mercado_pago_preferences p LEFT JOIN appointments a ON a.id=p.appointment_id WHERE p.appointment_id=$1 AND p.revision=$2",
      vec![
        preference.appointment_id.into(),
        preference.revision.into(),
        now.into(),
      ],
    ))
    .await?;
  row
    .ok_or_else(|| DbErr::Custom("Mercado Pago preference is unavailable".into()))?
    .try_get("", "needed")
}

async fn close_preference(
  db: &Db,
  clock: &dyn Clock,
  client: &Client,
  preference: &Preference,
) -> Result<(), DbErr> {
  let now = clock.now_millis();
  db.conn
    .execute(sql(
      "UPDATE mercado_pago_preferences SET outcome='closing' WHERE appointment_id=$1 AND revision=$2 AND outcome='active'",
      vec![preference.appointment_id.into(), preference.revision.into()],
    ))
    .await?;
  match client
    .expire_preference(&preference.preference_id, now)
    .await
  {
    // A rejected update leaves the native expiry at the Receipt Deadline in force.
    Ok(()) | Err(Error::Rejected | Error::Invalid) => {
      db.conn
        .execute(sql(
          "UPDATE mercado_pago_preferences SET outcome='closed',closed_at=$3,next_check_at=$4 WHERE appointment_id=$1 AND revision=$2 AND outcome='closing'",
          vec![
            preference.appointment_id.into(),
            preference.revision.into(),
            now.into(),
            now.saturating_add(LATE_CHECK_MS).into(),
          ],
        ))
        .await?;
      Ok(())
    }
    Err(Error::Unavailable) => {
      set_next_check(&db.conn, preference, Some(now.saturating_add(RETRY_MS))).await
    }
  }
}

fn payment_matches(payment: &Payment, preference: &Preference, collector: i64) -> bool {
  payment.status == "approved"
    && payment.currency_id.as_deref() == Some("MXN")
    && payment.amount_centavos == Some(preference.amount_centavos)
    && payment.external_reference.as_deref() == Some(preference.external_reference.as_str())
    && payment.collector_id == Some(collector)
}

async fn record_payment(
  db: &Db,
  clock: &dyn Clock,
  preference: &Preference,
  payment: &Payment,
  collector: i64,
  origin: &str,
) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let Some(appointment) = lock_appointment(&tx, preference.appointment_id).await? else {
    return tx.commit().await;
  };
  let (outcome, attention, history) = if !payment_matches(payment, preference, collector) {
    (
      "mismatch",
      Some("online_payment_mismatch"),
      "mercado_pago_payment_mismatch",
    )
  } else if appointment.payment == "verified" {
    (
      "duplicate",
      Some("online_payment_duplicate"),
      "mercado_pago_payment_duplicate",
    )
  } else if eligible_for_payment(&appointment, preference.expires_at, now)
    && confirm_appointment_payment(
      &tx,
      &appointment,
      PaymentConfirmation {
        contact: appointment.contact_id,
        actor: "System",
        history_kind: "payment_verified_mercado_pago",
        message_key: &format!(
          "appointment:{}:{}:mercado_pago_payment_verified",
          appointment.id, preference.revision
        ),
        message: "Mercado Pago confirmó su pago. La cita está en proceso de confirmación del calendario; le avisaré cuando se complete.",
        origin,
        now,
      },
    )
    .await?
    .is_some()
  {
    ("confirmed", None, "")
  } else {
    (
      "late",
      Some("online_payment_late"),
      "mercado_pago_payment_late",
    )
  };
  let inserted = tx
    .execute(sql(
      "INSERT INTO mercado_pago_payments(payment_id,appointment_id,revision,amount_centavos,outcome,provider_status,next_check_at,first_seen_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$8) ON CONFLICT (payment_id) DO NOTHING",
      vec![
        payment.id.into(),
        preference.appointment_id.into(),
        preference.revision.into(),
        payment.amount_centavos.unwrap_or(0).into(),
        outcome.into(),
        payment.status.clone().into(),
        now.saturating_add(RECHECK_MS).into(),
        now.into(),
      ],
    ))
    .await?;
  if inserted.rows_affected() != 1 {
    // Another worker recorded it first; dropping the transaction undoes this attempt.
    return tx.rollback().await;
  }
  if let Some(kind) = attention {
    appointment_store::history(&tx, appointment.id, "System", history, now).await?;
    appointment_store::attention(
      &tx,
      Some(appointment.id),
      Some(appointment.contact_id),
      kind,
      now,
    )
    .await?;
  }
  tx.commit().await
}

// ---- Refunds and later provider changes ----------------------------------------------------

async fn process_refund_job(
  db: &Db,
  clock: &dyn Clock,
  client: &Client,
  origin: &str,
  job: Job,
) -> Result<(), DbErr> {
  let now = clock.now_millis();
  let Some(payment_id) = job_number(&job, "mp_refund") else {
    return finish_job(db, &job, now).await;
  };
  let Some(row) = db
    .conn
    .query_one(sql(
      "SELECT amount_centavos,refund_state FROM mercado_pago_payments WHERE payment_id=$1 AND appointment_id=$2",
      vec![payment_id.into(), job.appointment_id.into()],
    ))
    .await?
  else {
    return finish_job(db, &job, now).await;
  };
  if row.try_get::<String>("", "refund_state")? != "requested" {
    return finish_job(db, &job, now).await;
  }
  let amount: i64 = row.try_get("", "amount_centavos")?;
  let result = client.refund(payment_id, amount).await;
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  match result {
    Ok(refund) if refund.status == "approved" => {
      finish_refund(&tx, payment_id, Some(refund.id), origin, now).await?;
    }
    Ok(refund) => {
      tx.execute(sql(
        "UPDATE mercado_pago_payments SET refund_id=$2,next_check_at=$3,updated_at=$4 WHERE payment_id=$1",
        vec![
          payment_id.into(),
          refund.id.into(),
          now.saturating_add(REFUND_CHECK_MS).into(),
          now.into(),
        ],
      ))
      .await?;
    }
    Err(Error::Rejected | Error::Invalid) => {
      let failed = tx
        .execute(sql(
          "UPDATE mercado_pago_payments SET refund_state='failed',updated_at=$2 WHERE payment_id=$1 AND refund_state='requested'",
          vec![payment_id.into(), now.into()],
        ))
        .await?;
      if failed.rows_affected() == 1 {
        appointment_store::history(
          &tx,
          job.appointment_id,
          "System",
          "mercado_pago_refund_failed",
          now,
        )
        .await?;
      }
    }
    Err(Error::Unavailable) => {
      drop(tx);
      return retry_job(db, clock, &job).await;
    }
  }
  finish_job_tx(&tx, &job, now).await?;
  tx.commit().await
}

/// Records a provider-confirmed refund once: history, the matching Attention Item and a notice.
/// Payment Verification is never reversed.
async fn finish_refund(
  tx: &DatabaseTransaction,
  payment_id: i64,
  refund_id: Option<i64>,
  origin: &str,
  now: i64,
) -> Result<(), DbErr> {
  let Some(row) = tx
    .query_one(sql(
      "SELECT appointment_id,outcome FROM mercado_pago_payments WHERE payment_id=$1",
      vec![payment_id.into()],
    ))
    .await?
  else {
    return Ok(());
  };
  let appointment_id: Uuid = row.try_get("", "appointment_id")?;
  let outcome: String = row.try_get("", "outcome")?;
  let Some(appointment) = lock_appointment(tx, appointment_id).await? else {
    return Ok(());
  };
  let updated = tx
    .execute(sql(
      "UPDATE mercado_pago_payments SET refund_state='refunded',refund_id=COALESCE($2,refund_id),updated_at=$3 WHERE payment_id=$1 AND refund_state='requested'",
      vec![payment_id.into(), refund_id.into(), now.into()],
    ))
    .await?;
  if updated.rows_affected() != 1 {
    return Ok(());
  }
  appointment_store::history(tx, appointment.id, "System", "mercado_pago_refunded", now).await?;
  let kind = match outcome.as_str() {
    "confirmed" => "refund_due",
    "late" => "online_payment_late",
    "duplicate" => "online_payment_duplicate",
    _ => "online_payment_mismatch",
  };
  tx.execute(sql(
    "UPDATE attention_items SET resolved_at=$3 WHERE appointment_id=$1 AND kind=$2 AND resolved_at IS NULL",
    vec![appointment.id.into(), kind.into(), now.into()],
  ))
  .await?;
  enqueue_text(
    tx,
    Message {
      contact: appointment.contact_id,
      appointment: Some(appointment.id),
      key: &format!(
        "appointment:{}:mercado_pago_refund:{payment_id}",
        appointment.id
      ),
      text: "Se emitió el reembolso de su pago con Mercado Pago. Puede tardar algunos días en reflejarse, según su banco.",
      purpose: "status",
      revision: Some(appointment.revision),
    },
    origin,
    now,
  )
  .await
}

/// Re-reads one due payment: completes requested refunds and flags refunds or chargebacks made
/// outside the app. Returns whether a payment was due.
async fn recheck_payment(
  db: &Db,
  clock: &dyn Clock,
  client: &Client,
  origin: &str,
) -> Result<bool, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let Some(row) = tx
    .query_one(sql(
      "SELECT payment_id FROM mercado_pago_payments WHERE next_check_at <= $1 ORDER BY next_check_at LIMIT 1 FOR UPDATE SKIP LOCKED",
      vec![now.into()],
    ))
    .await?
  else {
    tx.commit().await?;
    return Ok(false);
  };
  let payment_id: i64 = row.try_get("", "payment_id")?;
  tx.execute(sql(
    "UPDATE mercado_pago_payments SET next_check_at=$2 WHERE payment_id=$1",
    vec![payment_id.into(), now.saturating_add(CLAIM_MS).into()],
  ))
  .await?;
  tx.commit().await?;

  let result = client.payment(payment_id).await;
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let Ok(payment) = result else {
    tx.execute(sql(
      "UPDATE mercado_pago_payments SET next_check_at=$2 WHERE payment_id=$1",
      vec![payment_id.into(), now.saturating_add(RETRY_MS).into()],
    ))
    .await?;
    tx.commit().await?;
    return Ok(true);
  };
  let Some(row) = tx
    .query_one(sql(
      "SELECT p.appointment_id,p.provider_status,p.refund_state,p.outcome,a.contact_id,a.starts_at FROM mercado_pago_payments p JOIN appointments a ON a.id=p.appointment_id WHERE p.payment_id=$1",
      vec![payment_id.into()],
    ))
    .await?
  else {
    tx.commit().await?;
    return Ok(true);
  };
  let appointment_id: Uuid = row.try_get("", "appointment_id")?;
  let previous: String = row.try_get("", "provider_status")?;
  let refund_state: String = row.try_get("", "refund_state")?;
  let starts_at: i64 = row.try_get("", "starts_at")?;
  let reversed = matches!(payment.status.as_str(), "refunded" | "charged_back");
  if reversed && refund_state == "requested" {
    finish_refund(&tx, payment_id, None, origin, now).await?;
  } else if reversed && refund_state != "refunded" && previous != payment.status {
    appointment_store::history(
      &tx,
      appointment_id,
      "System",
      "mercado_pago_payment_reversed",
      now,
    )
    .await?;
    appointment_store::attention(
      &tx,
      Some(appointment_id),
      Some(row.try_get("", "contact_id")?),
      "online_payment_reversed",
      now,
    )
    .await?;
  }
  let still_requested = refund_state == "requested" && !reversed;
  let due = if still_requested {
    Some(now.saturating_add(REFUND_CHECK_MS))
  } else if now < starts_at.saturating_add(RECHECK_AFTER_START_MS) {
    Some(now.saturating_add(RECHECK_MS))
  } else {
    None
  };
  tx.execute(sql(
    "UPDATE mercado_pago_payments SET provider_status=$2,next_check_at=$3,updated_at=$4 WHERE payment_id=$1",
    vec![
      payment_id.into(),
      payment.status.into(),
      due.into(),
      now.into(),
    ],
  ))
  .await?;
  tx.commit().await?;
  Ok(true)
}
