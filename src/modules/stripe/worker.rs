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

use super::client::{CheckoutSession, Client, PaymentLinkRequest};

const CLAIM_MS: i64 = 30_000;
const POLL_INTERVAL_MS: i64 = 15_000;
const RETRY_MS: i64 = 60_000;
const LINK_BATCH_SIZE: usize = 3;

struct LinkJob {
  id: Uuid,
  appointment_id: Uuid,
  key: String,
  token: Uuid,
}

struct PaymentLink {
  appointment_id: Uuid,
  revision: i64,
  account_id: String,
  link_id: String,
  amount_centavos: i64,
  deactivate_at: i64,
  paid_session_id: Option<String>,
  outcome: String,
}

struct LinkSnapshot {
  account_id: String,
  link_id: Option<String>,
  amount_centavos: i64,
  deactivate_at: i64,
  outcome: String,
  deactivated_at: Option<i64>,
}

pub async fn run_once(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
) -> Result<(), DbErr> {
  let (Some(_), Some(client), Some(origin)) =
    (cfg.stripe.as_ref(), client, cfg.public_origin.as_deref())
  else {
    return Ok(());
  };

  for _ in 0..LINK_BATCH_SIZE {
    let Some(link) = claim_payment_link(db, clock).await? else {
      break;
    };
    process_payment_link(db, clock, client, origin, link).await?;
  }
  if let Some(job) = claim_link_job(db, clock).await? {
    process_link_job(db, clock, client, origin, job).await?;
  }
  Ok(())
}

async fn claim_link_job(db: &Db, clock: &dyn Clock) -> Result<Option<LinkJob>, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let row = tx
    .query_one(sql(
      "SELECT id,appointment_id,dedupe_key FROM appointment_jobs WHERE kind='stripe_link' AND finished_at IS NULL AND due_at <= $1 AND (claim_until IS NULL OR claim_until <= $1) ORDER BY due_at,id LIMIT 1 FOR UPDATE SKIP LOCKED",
      vec![now.into()],
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
  let job = LinkJob {
    id,
    appointment_id: row.try_get("", "appointment_id")?,
    key: row.try_get("", "dedupe_key")?,
    token,
  };
  tx.commit().await?;
  Ok(Some(job))
}

async fn claim_payment_link(db: &Db, clock: &dyn Clock) -> Result<Option<PaymentLink>, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let row = tx
    .query_one(sql(
      "SELECT l.appointment_id,l.revision,l.account_id,l.link_id,l.amount_centavos,l.deactivate_at,l.paid_session_id,l.outcome,(l.deactivate_at <= $1 OR a.id IS NULL OR a.status<>'awaiting_payment' OR a.payment NOT IN ('awaiting_receipt','under_review','rejected') OR a.format<>'video' OR a.receipt_deadline IS DISTINCT FROM l.deactivate_at OR NOT EXISTS (SELECT 1 FROM stripe_connection c WHERE c.id=1 AND c.account_id=l.account_id)) AS stale FROM stripe_payment_links l LEFT JOIN appointments a ON a.id=l.appointment_id WHERE l.link_id IS NOT NULL AND l.deactivated_at IS NULL AND l.outcome IN ('active','deactivating','paid','late','duplicate','deactivated') AND ((l.next_check_at IS NULL OR l.next_check_at <= $1) OR (l.outcome='active' AND (l.deactivate_at <= $1 OR a.id IS NULL OR a.status<>'awaiting_payment' OR a.payment NOT IN ('awaiting_receipt','under_review','rejected') OR a.format<>'video' OR a.receipt_deadline IS DISTINCT FROM l.deactivate_at OR NOT EXISTS (SELECT 1 FROM stripe_connection c WHERE c.id=1 AND c.account_id=l.account_id)))) ORDER BY stale DESC,COALESCE(l.next_check_at,l.created_at),l.created_at LIMIT 1 FOR UPDATE OF l SKIP LOCKED",
      vec![now.into()],
    ))
    .await?;
  let Some(row) = row else {
    tx.commit().await?;
    return Ok(None);
  };
  let link = PaymentLink {
    appointment_id: row.try_get("", "appointment_id")?,
    revision: row.try_get("", "revision")?,
    account_id: row.try_get("", "account_id")?,
    link_id: row.try_get("", "link_id")?,
    amount_centavos: row.try_get("", "amount_centavos")?,
    deactivate_at: row.try_get("", "deactivate_at")?,
    paid_session_id: row.try_get("", "paid_session_id")?,
    outcome: row.try_get("", "outcome")?,
  };
  tx.execute(sql(
    "UPDATE stripe_payment_links SET outcome=CASE WHEN outcome='active' THEN 'deactivating' ELSE outcome END,next_check_at=$3 WHERE appointment_id=$1 AND revision=$2 AND deactivated_at IS NULL",
    vec![
      link.appointment_id.into(),
      link.revision.into(),
      now.saturating_add(CLAIM_MS).into(),
    ],
  ))
  .await?;
  tx.commit().await?;
  Ok(Some(link))
}

fn job_revision(job: &LinkJob) -> Option<i64> {
  let mut parts = job.key.split(':');
  if parts.next()?.parse::<Uuid>().ok()? != job.appointment_id {
    return None;
  }
  let revision = parts.next()?.parse::<i64>().ok()?;
  (parts.next()? == "stripe_link" && parts.next().is_none()).then_some(revision)
}

async fn process_link_job(
  db: &Db,
  clock: &dyn Clock,
  client: &Client,
  origin: &str,
  job: LinkJob,
) -> Result<(), DbErr> {
  let now = clock.now_millis();
  let Some(revision) = job_revision(&job) else {
    return finish_link_job(db, &job, now).await;
  };
  let Some(snapshot) = load_link_snapshot(db, job.appointment_id, revision).await? else {
    return finish_link_job(db, &job, now).await;
  };
  if snapshot.link_id.is_some() {
    return finish_link_job(db, &job, now).await;
  }
  if snapshot.outcome != "pending" || snapshot.deactivated_at.is_some() {
    return abandon_uncreated_link(db, &job, revision, now).await;
  }

  let Some(appointment) = appointment_store::get(&db.conn, job.appointment_id).await? else {
    return abandon_uncreated_link(db, &job, revision, now).await;
  };
  let connected_account = connection_account(&db.conn).await?;
  if !eligible_for_link(&appointment, snapshot.deactivate_at, now)
    || connected_account.as_deref() != Some(snapshot.account_id.as_str())
    || appointment
      .payment_reference
      .as_deref()
      .is_none_or(|reference| reference.is_empty() || reference.chars().any(char::is_control))
  {
    return abandon_uncreated_link(db, &job, revision, now).await;
  }

  let local_start_time =
    appointment_store::local_time(appointment.starts_at, &appointment.timezone);
  let result = client
    .create_payment_link(PaymentLinkRequest {
      account_id: &snapshot.account_id,
      appointment_id: appointment.id,
      revision,
      amount_centavos: snapshot.amount_centavos,
      local_start_time: &local_start_time,
      payment_reference: appointment.payment_reference.as_deref().unwrap_or_default(),
    })
    .await;
  let created = match result {
    Ok(created) => created,
    Err(_) => return retry_link_job(db, clock, &job).await,
  };

  let deactivate =
    persist_created_link(db, clock, &job, revision, &snapshot, &created, origin).await?;
  if deactivate {
    deactivate_link(
      db,
      clock,
      client,
      &PaymentLink {
        appointment_id: job.appointment_id,
        revision,
        account_id: snapshot.account_id,
        link_id: created.link_id,
        amount_centavos: snapshot.amount_centavos,
        deactivate_at: snapshot.deactivate_at,
        paid_session_id: None,
        outcome: "active".into(),
      },
    )
    .await?;
  }
  Ok(())
}

async fn load_link_snapshot(
  db: &Db,
  appointment_id: Uuid,
  revision: i64,
) -> Result<Option<LinkSnapshot>, DbErr> {
  let Some(row) = db
    .conn
    .query_one(sql(
      "SELECT account_id,link_id,amount_centavos,deactivate_at,outcome,deactivated_at FROM stripe_payment_links WHERE appointment_id=$1 AND revision=$2",
      vec![appointment_id.into(), revision.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  Ok(Some(LinkSnapshot {
    account_id: row.try_get("", "account_id")?,
    link_id: row.try_get("", "link_id")?,
    amount_centavos: row.try_get("", "amount_centavos")?,
    deactivate_at: row.try_get("", "deactivate_at")?,
    outcome: row.try_get("", "outcome")?,
    deactivated_at: row.try_get("", "deactivated_at")?,
  }))
}

async fn connection_account<C: ConnectionTrait>(conn: &C) -> Result<Option<String>, DbErr> {
  let Some(row) = conn
    .query_one(sql(
      "SELECT account_id FROM stripe_connection WHERE id=1",
      vec![],
    ))
    .await?
  else {
    return Ok(None);
  };
  row.try_get("", "account_id")
}

fn eligible_for_link(appointment: &Appointment, deadline: i64, now: i64) -> bool {
  appointment.format == "video"
    && appointment.status == "awaiting_payment"
    && matches!(
      appointment.payment.as_str(),
      "awaiting_receipt" | "under_review" | "rejected"
    )
    && appointment.receipt_deadline == Some(deadline)
    && now < deadline
}

async fn persist_created_link(
  db: &Db,
  clock: &dyn Clock,
  job: &LinkJob,
  revision: i64,
  snapshot: &LinkSnapshot,
  created: &super::client::CreatedPaymentLink,
  origin: &str,
) -> Result<bool, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let appointment = lock_appointment(&tx, job.appointment_id).await?;
  let Some(row) = tx
    .query_one(sql(
      "SELECT link_id,outcome,deactivated_at FROM stripe_payment_links WHERE appointment_id=$1 AND revision=$2 FOR UPDATE",
      vec![job.appointment_id.into(), revision.into()],
    ))
    .await?
  else {
    finish_link_job_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(true);
  };
  let existing_link: Option<String> = row.try_get("", "link_id")?;
  if existing_link.is_some() {
    finish_link_job_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(false);
  }
  let outcome: String = row.try_get("", "outcome")?;
  let deactivated_at: Option<i64> = row.try_get("", "deactivated_at")?;

  let connected = connection_account(&tx).await?;
  let live = outcome == "pending"
    && deactivated_at.is_none()
    && appointment.as_ref().is_some_and(|appointment| {
      eligible_for_link(appointment, snapshot.deactivate_at, now)
        && connected.as_deref() == Some(snapshot.account_id.as_str())
    });
  let link_outcome = if live { "active" } else { "deactivating" };
  let next_check_at = if live {
    now
  } else {
    now.saturating_add(CLAIM_MS)
  };
  tx.execute(sql(
    "UPDATE stripe_payment_links SET price_id=$3,link_id=$4,url=$5,deactivated_at=NULL,outcome=$6,next_check_at=$7 WHERE appointment_id=$1 AND revision=$2 AND link_id IS NULL",
    vec![
      job.appointment_id.into(),
      revision.into(),
      created.price_id.clone().into(),
      created.link_id.clone().into(),
      created.url.clone().into(),
      link_outcome.into(),
      next_check_at.into(),
    ],
  ))
  .await?;
  if live {
    let appointment = appointment.as_ref().expect("checked above");
    let key = format!(
      "appointment:{}:{}:stripe_payment_link",
      appointment.id, appointment.revision
    );
    let text = format!("Pague con tarjeta aquí: {}", created.url);
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
  finish_link_job_tx(&tx, job, now).await?;
  tx.commit().await?;
  Ok(!live)
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

async fn abandon_uncreated_link(
  db: &Db,
  job: &LinkJob,
  revision: i64,
  now: i64,
) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  tx.execute(sql(
    "UPDATE stripe_payment_links SET outcome='deactivated',deactivated_at=$3,next_check_at=NULL WHERE appointment_id=$1 AND revision=$2 AND link_id IS NULL AND deactivated_at IS NULL",
    vec![job.appointment_id.into(), revision.into(), now.into()],
  ))
  .await?;
  finish_link_job_tx(&tx, job, now).await?;
  tx.commit().await?;
  Ok(())
}

async fn finish_link_job(db: &Db, job: &LinkJob, now: i64) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  finish_link_job_tx(&tx, job, now).await?;
  tx.commit().await?;
  Ok(())
}

async fn finish_link_job_tx(
  tx: &DatabaseTransaction,
  job: &LinkJob,
  now: i64,
) -> Result<(), DbErr> {
  let result = tx
    .execute(sql(
      "UPDATE appointment_jobs SET finished_at=$2,claim=NULL,claim_until=NULL,last_error=NULL WHERE id=$1 AND claim=$3 AND finished_at IS NULL",
      vec![job.id.into(), now.into(), job.token.into()],
    ))
    .await?;
  if result.rows_affected() != 1 {
    return Err(DbErr::Custom("Stripe link job claim was lost".into()));
  }
  Ok(())
}

async fn retry_link_job(db: &Db, clock: &dyn Clock, job: &LinkJob) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let result = tx
    .execute(sql(
      "UPDATE appointment_jobs SET due_at=$2,claim=NULL,claim_until=NULL WHERE id=$1 AND claim=$3 AND finished_at IS NULL",
      vec![job.id.into(), now.saturating_add(RETRY_MS).into(), job.token.into()],
    ))
    .await?;
  if result.rows_affected() != 1 {
    return Err(DbErr::Custom("Stripe link job claim was lost".into()));
  }
  tx.commit().await?;
  Ok(())
}

async fn process_payment_link(
  db: &Db,
  clock: &dyn Clock,
  client: &Client,
  origin: &str,
  link: PaymentLink,
) -> Result<(), DbErr> {
  let poll = if link.paid_session_id.is_none()
    && matches!(link.outcome.as_str(), "active" | "deactivating")
  {
    Some(
      client
        .list_checkout_sessions(&link.account_id, &link.link_id)
        .await,
    )
  } else {
    None
  };

  if let Some(Ok(sessions)) = poll.as_ref() {
    for session in sessions {
      if paid_session_matches(session, link.amount_centavos) {
        if record_paid_session(db, clock, &link, session, origin).await? {
          deactivate_link(db, clock, client, &link).await?;
          return Ok(());
        }
        break;
      }
    }
  }

  if link_needs_deactivation(db, &link, clock.now_millis(), link.outcome != "active").await? {
    deactivate_link(db, clock, client, &link).await?;
  } else {
    let now = clock.now_millis();
    let due = now.saturating_add(POLL_INTERVAL_MS).min(link.deactivate_at);
    schedule_link_poll(db, &link, due, now).await?;
  }
  Ok(())
}

async fn schedule_link_poll(db: &Db, link: &PaymentLink, due: i64, now: i64) -> Result<(), DbErr> {
  db.conn
    .execute(sql(
      "UPDATE stripe_payment_links SET outcome='active',next_check_at=$3 WHERE appointment_id=$1 AND revision=$2 AND deactivated_at IS NULL AND paid_session_id IS NULL AND outcome IN ('active','deactivating') AND NOT (outcome='deactivating' AND next_check_at <= $4)",
      vec![
        link.appointment_id.into(),
        link.revision.into(),
        due.into(),
        now.into(),
      ],
    ))
    .await?;
  Ok(())
}

fn paid_session_matches(session: &CheckoutSession, amount_centavos: i64) -> bool {
  session.status == "complete"
    && session.payment_status == "paid"
    && session.currency.as_deref() == Some("mxn")
    && session.amount_total == Some(amount_centavos)
}

async fn record_paid_session(
  db: &Db,
  clock: &dyn Clock,
  link: &PaymentLink,
  session: &CheckoutSession,
  origin: &str,
) -> Result<bool, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let Some(appointment) = lock_appointment(&tx, link.appointment_id).await? else {
    tx.commit().await?;
    return Ok(false);
  };
  let Some(row) = tx
    .query_one(sql(
      "SELECT paid_session_id,deactivate_at FROM stripe_payment_links WHERE appointment_id=$1 AND revision=$2 FOR UPDATE",
      vec![link.appointment_id.into(), link.revision.into()],
    ))
    .await?
  else {
    tx.commit().await?;
    return Ok(false);
  };
  let already_paid: Option<String> = row.try_get("", "paid_session_id")?;
  let deadline: i64 = row.try_get("", "deactivate_at")?;
  if already_paid.is_some() {
    tx.commit().await?;
    return Ok(false);
  }

  let outcome = if appointment.payment == "verified" {
    appointment_store::attention(
      &tx,
      Some(appointment.id),
      Some(appointment.contact_id),
      "stripe_payment_duplicate",
      now,
    )
    .await?;
    "duplicate"
  } else if eligible_for_link(&appointment, deadline, now) {
    let message_key = format!(
      "appointment:{}:{}:stripe_payment_verified",
      appointment.id, link.revision
    );
    let message = "Stripe confirmó su pago. La cita está en proceso de confirmación del calendario; le avisaré cuando se complete.";
    if confirm_appointment_payment(
      &tx,
      &appointment,
      PaymentConfirmation {
        contact: appointment.contact_id,
        actor: "System",
        history_kind: "payment_verified_stripe",
        message_key: &message_key,
        message,
        origin,
        now,
      },
    )
    .await?
    .is_some()
    {
      "paid"
    } else {
      record_late_payment(&tx, &appointment, now).await?;
      "late"
    }
  } else {
    record_late_payment(&tx, &appointment, now).await?;
    "late"
  };

  tx.execute(sql(
    "UPDATE stripe_payment_links SET paid_session_id=$3,paid_at=$4,outcome=$5,next_check_at=$4 WHERE appointment_id=$1 AND revision=$2 AND paid_session_id IS NULL",
    vec![
      link.appointment_id.into(),
      link.revision.into(),
      session.id.clone().into(),
      now.into(),
      outcome.into(),
    ],
  ))
  .await?;
  tx.commit().await?;
  Ok(true)
}

async fn record_late_payment(
  tx: &DatabaseTransaction,
  appointment: &Appointment,
  now: i64,
) -> Result<(), DbErr> {
  appointment_store::history(tx, appointment.id, "System", "stripe_payment_late", now).await?;
  appointment_store::attention(
    tx,
    Some(appointment.id),
    Some(appointment.contact_id),
    "stripe_payment_late",
    now,
  )
  .await
}

async fn link_needs_deactivation(
  db: &Db,
  link: &PaymentLink,
  now: i64,
  force: bool,
) -> Result<bool, DbErr> {
  if force {
    return Ok(true);
  }
  let row = db
    .conn
    .query_one(sql(
      "SELECT (l.paid_session_id IS NOT NULL OR (l.outcome='deactivating' AND l.next_check_at <= $3) OR l.deactivate_at <= $3 OR a.id IS NULL OR a.status<>'awaiting_payment' OR a.payment NOT IN ('awaiting_receipt','under_review','rejected') OR a.format<>'video' OR a.receipt_deadline IS DISTINCT FROM l.deactivate_at OR NOT EXISTS (SELECT 1 FROM stripe_connection c WHERE c.id=1 AND c.account_id=l.account_id)) AS needed FROM stripe_payment_links l LEFT JOIN appointments a ON a.id=l.appointment_id WHERE l.appointment_id=$1 AND l.revision=$2",
      vec![link.appointment_id.into(), link.revision.into(), now.into()],
    ))
    .await?;
  row
    .ok_or_else(|| DbErr::Custom("Stripe payment link is unavailable".into()))?
    .try_get("", "needed")
}

async fn deactivate_link(
  db: &Db,
  clock: &dyn Clock,
  client: &Client,
  link: &PaymentLink,
) -> Result<(), DbErr> {
  let now = clock.now_millis();
  db.conn
    .execute(sql(
      "UPDATE stripe_payment_links SET outcome=CASE WHEN outcome='active' THEN 'deactivating' ELSE outcome END,next_check_at=$3 WHERE appointment_id=$1 AND revision=$2 AND deactivated_at IS NULL",
      vec![
        link.appointment_id.into(),
        link.revision.into(),
        now.saturating_add(CLAIM_MS).into(),
      ],
    ))
    .await?;
  if client
    .deactivate_payment_link(&link.account_id, &link.link_id)
    .await
    .is_ok()
  {
    db.conn
      .execute(sql(
        "UPDATE stripe_payment_links SET deactivated_at=$3,next_check_at=NULL,outcome=CASE WHEN outcome='deactivating' THEN 'deactivated' ELSE outcome END WHERE appointment_id=$1 AND revision=$2 AND deactivated_at IS NULL",
        vec![link.appointment_id.into(), link.revision.into(), now.into()],
      ))
      .await?;
  } else {
    schedule_link_retry(db, clock, link, RETRY_MS).await?;
  }
  Ok(())
}

async fn schedule_link_retry(
  db: &Db,
  clock: &dyn Clock,
  link: &PaymentLink,
  delay: i64,
) -> Result<(), DbErr> {
  db.conn
    .execute(sql(
      "UPDATE stripe_payment_links SET outcome=CASE WHEN outcome='active' THEN 'deactivating' ELSE outcome END,next_check_at=$3 WHERE appointment_id=$1 AND revision=$2 AND deactivated_at IS NULL",
      vec![
        link.appointment_id.into(),
        link.revision.into(),
        clock.now_millis().saturating_add(delay).into(),
      ],
    ))
    .await?;
  Ok(())
}
