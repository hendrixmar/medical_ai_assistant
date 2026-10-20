use crate::{
  common::config::Config,
  database::Db,
  modules::{
    appointments::store,
    conversation::store::{enqueue_text, Message},
    doctor::Clock,
    practice::{next_working_time, PracticeDay},
    whatsapp::{bound_transaction, inbox::sql},
  },
};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};
use uuid::Uuid;

const CLAIM_MS: i64 = 30_000;
const RECEIPT_COPY_RECHECK_MS: i64 = 5_000;
const REVIEW_RETRY_MS: i64 = 15 * 60_000;
const LINK_RETRY_MS: i64 = 60_000;
const PENDING_RECEIPT_MAX_AGE_MS: i64 = 180_000;

#[derive(Clone, Copy)]
enum Work {
  Expiry,
  Reminder24h,
  Reminder2h,
  ReviewPeriodic,
  ReviewUrgent,
}

pub(crate) enum ExpiryCheck {
  NotDue,
  Deferred,
  Expired,
}

struct Claim {
  id: Uuid,
  appointment_id: Uuid,
  contact_id: Uuid,
  kind: String,
  due_at: i64,
  key: String,
  token: Uuid,
}

struct AppointmentState {
  contact_id: Uuid,
  patient_name: String,
  format: String,
  starts_at: i64,
  timezone: String,
  location: String,
  status: String,
  payment: String,
  calendar_sync: String,
  meet_url: Option<String>,
  revision: i64,
  receipt_deadline: Option<i64>,
}

struct ReviewSettings {
  working_hours: [PracticeDay; 7],
  timezone: chrono_tz::Tz,
  interval_minutes: i32,
}

fn work(kind: &str, key: &str, appointment_id: Uuid) -> Option<(i64, Work, Option<i64>)> {
  let mut parts = key.split(':');
  if parts.next()?.parse::<Uuid>().ok()? != appointment_id {
    return None;
  }
  let revision = parts.next()?.parse::<i64>().ok()?;
  let key_kind = parts.next()?;
  let variant = parts.next()?;
  let work = match (kind, key_kind, variant) {
    ("expiry", "expiry", "deadline") => Work::Expiry,
    ("reminders", "reminders", "24h") => Work::Reminder24h,
    ("reminders", "reminders", "2h") => Work::Reminder2h,
    ("review", "review", "periodic") => Work::ReviewPeriodic,
    ("review", "review", "urgent") => Work::ReviewUrgent,
    _ => return None,
  };
  let scheduled_starts_at = match work {
    Work::Reminder24h | Work::Reminder2h => {
      let starts_at = parts.next()?.parse::<i64>().ok()?;
      if parts.next().is_some() {
        return None;
      }
      Some(starts_at)
    }
    Work::ReviewPeriodic => {
      if parts.next().is_some_and(|due| due.parse::<i64>().is_err()) || parts.next().is_some() {
        return None;
      }
      None
    }
    _ if parts.next().is_some() => return None,
    _ => None,
  };
  Some((revision, work, scheduled_starts_at))
}

async fn claim(db: &Db, clock: &dyn Clock) -> Result<Option<Claim>, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let row = tx
    .query_one(sql(
      "SELECT j.id,j.appointment_id,a.contact_id,j.kind,j.due_at,j.dedupe_key FROM appointment_jobs j JOIN appointments a ON a.id=j.appointment_id WHERE j.kind IN ('expiry','reminders','review') AND j.finished_at IS NULL AND j.due_at <= $1 AND (j.claim_until IS NULL OR j.claim_until <= $1) ORDER BY j.due_at,j.id LIMIT 1 FOR UPDATE OF j SKIP LOCKED",
      vec![now.into()],
    ))
    .await?;
  let Some(row) = row else {
    tx.commit().await?;
    return Ok(None);
  };
  let token = Uuid::now_v7();
  let id: Uuid = row.try_get("", "id")?;
  let claim_until = now.saturating_add(CLAIM_MS);
  tx.execute(sql(
    "UPDATE appointment_jobs SET claim=$2,claim_until=$3,attempts=attempts+1 WHERE id=$1",
    vec![id.into(), token.into(), claim_until.into()],
  ))
  .await?;
  let claim = Claim {
    id,
    appointment_id: row.try_get("", "appointment_id")?,
    contact_id: row.try_get("", "contact_id")?,
    kind: row.try_get("", "kind")?,
    due_at: row.try_get("", "due_at")?,
    key: row.try_get("", "dedupe_key")?,
    token,
  };
  tx.commit().await?;
  Ok(Some(claim))
}

async fn lock_contact(tx: &sea_orm::DatabaseTransaction, id: Uuid) -> Result<(), DbErr> {
  tx.query_one(sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![id.into()],
  ))
  .await?
  .ok_or_else(|| DbErr::Custom("Appointment Contact is unavailable".into()))?;
  Ok(())
}

async fn doctor_contact(
  tx: &sea_orm::DatabaseTransaction,
  owner: Uuid,
) -> Result<Option<Uuid>, DbErr> {
  let row = tx
    .query_one(sql(
      "SELECT contact_id FROM doctor_number_state WHERE id=1 AND verified_phone IS NOT NULL",
      vec![],
    ))
    .await?;
  let Some(row) = row else {
    return Ok(None);
  };
  let contact: Option<Uuid> = row.try_get("", "contact_id")?;
  let Some(contact) = contact else {
    return Ok(None);
  };
  if contact != owner {
    lock_contact(tx, contact).await?;
  }
  Ok(Some(contact))
}

async fn appointment(
  tx: &sea_orm::DatabaseTransaction,
  id: Uuid,
) -> Result<Option<AppointmentState>, DbErr> {
  let Some(row) = tx
    .query_one(sql(
      "SELECT contact_id,patient_name,format,starts_at,timezone,location,status,payment,calendar_sync,meet_url,revision,receipt_deadline FROM appointments WHERE id=$1 FOR UPDATE",
      vec![id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  Ok(Some(AppointmentState {
    contact_id: row.try_get("", "contact_id")?,
    patient_name: row.try_get("", "patient_name")?,
    format: row.try_get("", "format")?,
    starts_at: row.try_get("", "starts_at")?,
    timezone: row.try_get("", "timezone")?,
    location: row.try_get("", "location")?,
    status: row.try_get("", "status")?,
    payment: row.try_get("", "payment")?,
    calendar_sync: row.try_get("", "calendar_sync")?,
    meet_url: row.try_get("", "meet_url")?,
    revision: row.try_get("", "revision")?,
    receipt_deadline: row.try_get("", "receipt_deadline")?,
  }))
}

async fn job_current(tx: &sea_orm::DatabaseTransaction, claim: &Claim) -> Result<bool, DbErr> {
  let row = tx
    .query_one(sql(
      "SELECT claim,finished_at FROM appointment_jobs WHERE id=$1 FOR UPDATE",
      vec![claim.id.into()],
    ))
    .await?;
  let Some(row) = row else {
    return Ok(false);
  };
  let token: Option<Uuid> = row.try_get("", "claim")?;
  let finished_at: Option<i64> = row.try_get("", "finished_at")?;
  Ok(token == Some(claim.token) && finished_at.is_none())
}

async fn finish(tx: &sea_orm::DatabaseTransaction, claim: &Claim, now: i64) -> Result<(), DbErr> {
  let result = tx
    .execute(sql(
      "UPDATE appointment_jobs SET finished_at=$2,claim=NULL,claim_until=NULL,last_error=NULL WHERE id=$1 AND claim=$3 AND finished_at IS NULL",
      vec![claim.id.into(), now.into(), claim.token.into()],
    ))
    .await?;
  if result.rows_affected() != 1 {
    return Err(DbErr::Custom("Appointment job claim was lost".into()));
  }
  Ok(())
}

async fn defer(tx: &sea_orm::DatabaseTransaction, claim: &Claim, due_at: i64) -> Result<(), DbErr> {
  let result = tx
    .execute(sql(
      "UPDATE appointment_jobs SET due_at=$2,claim=NULL,claim_until=NULL,last_error=NULL WHERE id=$1 AND claim=$3 AND finished_at IS NULL",
      vec![claim.id.into(), due_at.into(), claim.token.into()],
    ))
    .await?;
  if result.rows_affected() != 1 {
    return Err(DbErr::Custom("Appointment job claim was lost".into()));
  }
  Ok(())
}

async fn active_hold_expiries(
  tx: &sea_orm::DatabaseTransaction,
  id: Uuid,
) -> Result<Vec<Option<i64>>, DbErr> {
  tx.query_all(sql(
    "SELECT expires_at FROM appointment_holds WHERE appointment_id=$1 AND active FOR UPDATE",
    vec![id.into()],
  ))
  .await?
  .iter()
  .map(|row| row.try_get("", "expires_at"))
  .collect()
}

async fn timely_pending_media(
  tx: &sea_orm::DatabaseTransaction,
  appointment: &AppointmentState,
  appointment_id: Uuid,
  hold_expiry: i64,
  now: i64,
) -> Result<bool, DbErr> {
  let Some(deadline) = appointment.receipt_deadline else {
    return Ok(false);
  };
  let row = tx
    .query_one(sql(
      "SELECT EXISTS (SELECT 1 FROM receipt_intake_state intake JOIN inbox_messages i ON i.message_id=intake.inbox_message_id JOIN contacts c ON c.id=$1 WHERE c.identity_conflict=false AND intake.state='pending' AND intake.sent_at <= $2 AND intake.arrived_at < $3 AND intake.arrived_at >= $4 AND intake.arrived_at <= $5 AND (i.contact_id=$1 OR EXISTS (SELECT 1 FROM contact_aliases alias WHERE alias.contact_id=$1 AND alias.jid IN (i.original_chat_identity,i.resolved_phone_identity))) AND (SELECT count(DISTINCT candidate.id) FROM appointments candidate JOIN appointment_holds h ON h.appointment_id=candidate.id AND h.active AND h.expires_at IS NOT NULL AND intake.arrived_at < h.expires_at WHERE candidate.contact_id=$1 AND candidate.format='video' AND candidate.status='awaiting_payment' AND candidate.payment='awaiting_receipt' AND candidate.receipt_deadline IS NOT NULL AND candidate.receipt_deadline >= intake.sent_at)=1 AND EXISTS (SELECT 1 FROM appointments candidate JOIN appointment_holds h ON h.appointment_id=candidate.id AND h.active AND h.expires_at IS NOT NULL AND intake.arrived_at < h.expires_at WHERE candidate.id=$6 AND candidate.contact_id=$1 AND candidate.format='video' AND candidate.status='awaiting_payment' AND candidate.payment='awaiting_receipt' AND candidate.receipt_deadline IS NOT NULL AND candidate.receipt_deadline >= intake.sent_at)) AS pending",
      vec![
        appointment.contact_id.into(),
        deadline.into(),
        hold_expiry.into(),
        now.saturating_sub(PENDING_RECEIPT_MAX_AGE_MS).into(),
        now.into(),
        appointment_id.into(),
      ],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Pending Receipt state is unavailable".into()))?;
  row.try_get("", "pending")
}

fn local_time(starts_at: i64, timezone: &str) -> Option<String> {
  let timezone = timezone.parse::<chrono_tz::Tz>().ok()?;
  let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(starts_at)?;
  Some(format!(
    "{} ({})",
    utc.with_timezone(&timezone).format("%d/%m/%Y a las %H:%M"),
    timezone.name()
  ))
}

fn payment_label(payment: &str) -> &'static str {
  match payment {
    "awaiting_receipt" => "pendiente de comprobante",
    "under_review" => "comprobante en revisión",
    "verified" => "pago verificado",
    "rejected" => "comprobante rechazado",
    "not_required" => "sin pago requerido",
    _ => "estado de pago registrado",
  }
}

fn appointment_reminder(appointment: &AppointmentState) -> Option<String> {
  if appointment.status != "confirmed" || appointment.calendar_sync != "synced" {
    return None;
  }
  let local = local_time(appointment.starts_at, &appointment.timezone)?;
  match appointment.format.as_str() {
    "in_person" if !appointment.location.trim().is_empty() => Some(format!(
      "Le recordamos su cita del {local} en {}. Para confirmar su asistencia, responda CONFIRMO.",
      appointment.location
    )),
    "video" if appointment.payment == "verified"
      && appointment
        .meet_url
        .as_deref()
        .is_some_and(crate::modules::google_calendar::worker::valid_meet) =>
    {
      Some(format!(
        "Le recordamos su cita de video del {local}. Enlace de Google Meet: {}. Para confirmar su asistencia, responda CONFIRMO.",
        appointment.meet_url.as_deref()?
      ))
    }
    _ => None,
  }
}

fn appointment_status(appointment: &AppointmentState) -> Option<String> {
  let local = local_time(appointment.starts_at, &appointment.timezone)?;
  let status = match appointment.status.as_str() {
    "requested" => "solicitada, todavía sin confirmación",
    "awaiting_payment" => "pendiente de confirmación",
    "confirmed" if appointment.format == "video" && appointment.payment != "verified" => {
      return Some(format!(
        "Su cita de video del {local} sigue confirmada, pero el pago figura como {} y todavía no podemos compartir acceso a la sesión.",
        payment_label(&appointment.payment)
      ));
    }
    "confirmed" if appointment.calendar_sync != "synced" => {
      return Some(format!(
        "Su cita del {local} sigue confirmada, pero la sincronización con el calendario aún no está completa. Le avisaremos cuando termine."
      ));
    }
    "confirmed" if appointment.format == "video" => {
      return Some(format!(
        "Su cita de video del {local} sigue confirmada, pero el enlace está pendiente de validación. Le avisaremos cuando esté disponible."
      ));
    }
    "confirmed" if appointment.format == "in_person" && appointment.location.trim().is_empty() => {
      return Some(format!(
        "Su cita del {local} sigue confirmada, pero la ubicación aún no está disponible. Le avisaremos cuando esté lista."
      ));
    }
    _ => return None,
  };
  Some(format!(
    "Su cita del {local} continúa {status}; el estado de pago es {}. Todavía no está confirmada. Le avisaremos si cambia.",
    payment_label(&appointment.payment)
  ))
}

async fn practice_reminder_enabled(tx: &sea_orm::DatabaseTransaction) -> Result<bool, DbErr> {
  let row = tx
    .query_one(sql(
      "SELECT reminder_2h_enabled FROM practice WHERE id=1",
      vec![],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Practice is unavailable".into()))?;
  row.try_get("", "reminder_2h_enabled")
}

async fn review_settings(
  tx: &sea_orm::DatabaseTransaction,
) -> Result<Option<ReviewSettings>, DbErr> {
  let Some(row) = tx
    .query_one(sql(
      "SELECT weekly_hours,timezone,review_interval_minutes FROM practice WHERE id=1",
      vec![],
    ))
    .await?
  else {
    return Err(DbErr::Custom("Practice is unavailable".into()));
  };
  let hours: Option<serde_json::Value> = row.try_get("", "weekly_hours")?;
  let timezone: Option<String> = row.try_get("", "timezone")?;
  let interval_minutes: i32 = row.try_get("", "review_interval_minutes")?;
  let Some(working_hours) = hours.and_then(|hours| serde_json::from_value(hours).ok()) else {
    return Ok(None);
  };
  let Some(timezone) = timezone.and_then(|timezone| timezone.parse().ok()) else {
    return Ok(None);
  };
  Ok(Some(ReviewSettings {
    working_hours,
    timezone,
    interval_minutes,
  }))
}

async fn process_expiry(
  tx: &sea_orm::DatabaseTransaction,
  claim: &Claim,
  appointment: &AppointmentState,
  now: i64,
  origin: &str,
) -> Result<(), DbErr> {
  let rejected = appointment.payment == "rejected";
  if appointment.status != "awaiting_payment"
    || !(appointment.payment == "awaiting_receipt" || rejected)
    || appointment.format != "video"
  {
    return finish(tx, claim, now).await;
  }
  if rejected
    && appointment
      .receipt_deadline
      .is_none_or(|deadline| claim.due_at < deadline)
  {
    return finish(tx, claim, now).await;
  }
  let expiries = active_hold_expiries(tx, claim.appointment_id).await?;
  if rejected {
    let expiry = expiries
      .iter()
      .flatten()
      .copied()
      .max()
      .unwrap_or(claim.due_at)
      .max(claim.due_at);
    if expiry > now {
      return defer(tx, claim, expiry).await;
    }
  } else {
    if expiries.iter().any(Option::is_none) {
      return finish(tx, claim, now).await;
    }
    if let Some(latest_expiry) = expiries.iter().flatten().copied().max() {
      if latest_expiry > now {
        return defer(tx, claim, latest_expiry).await;
      }
      if timely_pending_media(tx, appointment, claim.appointment_id, latest_expiry, now).await? {
        return defer(tx, claim, now.saturating_add(RECEIPT_COPY_RECHECK_MS)).await;
      }
    }
  }
  let revision = appointment
    .revision
    .checked_add(1)
    .ok_or_else(|| DbErr::Custom("Appointment revision overflow".into()))?;
  let updated = tx
    .execute(sql(
      "UPDATE appointments SET status='expired',expired_at=$2,revision=$3,updated_at=$2 WHERE id=$1 AND status='awaiting_payment' AND payment=$5 AND revision=$4",
      vec![
        claim.appointment_id.into(),
        now.into(),
        revision.into(),
        appointment.revision.into(),
        appointment.payment.clone().into(),
      ],
    ))
    .await?;
  if updated.rows_affected() != 1 {
    return Err(DbErr::Custom("Appointment expiry state changed".into()));
  }
  tx.execute(sql(
    "UPDATE appointment_holds SET active=false WHERE appointment_id=$1 AND active",
    vec![claim.appointment_id.into()],
  ))
  .await?;
  store::history(tx, claim.appointment_id, "System", "expired", now).await?;
  let local = local_time(appointment.starts_at, &appointment.timezone)
    .ok_or_else(|| DbErr::Custom("Appointment timezone is invalid".into()))?;
  let text = if rejected {
    format!("Su comprobante fue rechazado y la cita del {local} venció. El horario se liberó.")
  } else {
    format!(
      "No recibimos un comprobante a tiempo. Su solicitud de cita del {local} venció y el horario se liberó."
    )
  };
  let key = format!("appointment:{}:{revision}:expired", claim.appointment_id);
  enqueue_text(
    tx,
    Message {
      contact: appointment.contact_id,
      appointment: Some(claim.appointment_id),
      key: &key,
      text: &text,
      purpose: "status",
      revision: Some(revision),
    },
    origin,
    now,
  )
  .await?;
  finish(tx, claim, now).await
}

/// Applies an already-due expiry job inside a caller's transaction before a Doctor decision.
/// The caller must invoke this before taking the Appointment lock; Contact is locked first.
pub(crate) async fn expire_if_due(
  tx: &sea_orm::DatabaseTransaction,
  appointment_id: Uuid,
  origin: &str,
  now: i64,
) -> Result<ExpiryCheck, DbErr> {
  let Some(row) = tx
    .query_one(sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![appointment_id.into()],
    ))
    .await?
  else {
    return Ok(ExpiryCheck::NotDue);
  };
  let contact_id: Uuid = row.try_get("", "contact_id")?;
  lock_contact(tx, contact_id).await?;
  let Some(appointment) = appointment(tx, appointment_id).await? else {
    return Ok(ExpiryCheck::NotDue);
  };
  let key = format!("{appointment_id}:{}:expiry:deadline", appointment.revision);
  let Some(row) = tx
    .query_one(sql(
      "SELECT id,kind,due_at,dedupe_key FROM appointment_jobs WHERE appointment_id=$1 AND kind='expiry' AND dedupe_key=$2 AND finished_at IS NULL FOR UPDATE",
      vec![appointment_id.into(), key.clone().into()],
    ))
    .await?
  else {
    return Ok(ExpiryCheck::NotDue);
  };
  let due_at: i64 = row.try_get("", "due_at")?;
  if due_at > now {
    return Ok(ExpiryCheck::NotDue);
  }
  let token = Uuid::now_v7();
  let id: Uuid = row.try_get("", "id")?;
  let claimed = tx
    .execute(sql(
      "UPDATE appointment_jobs SET claim=$2,claim_until=$3 WHERE id=$1 AND finished_at IS NULL",
      vec![id.into(), token.into(), now.saturating_add(CLAIM_MS).into()],
    ))
    .await?;
  if claimed.rows_affected() != 1 {
    return Err(DbErr::Custom(
      "Appointment expiry job claim was lost".into(),
    ));
  }
  let claim = Claim {
    id,
    appointment_id,
    contact_id,
    kind: row.try_get("", "kind")?,
    due_at,
    key: row.try_get("", "dedupe_key")?,
    token,
  };
  process_expiry(tx, &claim, &appointment, now, origin).await?;
  let row = tx
    .query_one(sql(
      "SELECT a.status,j.due_at,j.finished_at FROM appointments a JOIN appointment_jobs j ON j.id=$1 WHERE a.id=$2",
      vec![id.into(), appointment_id.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Appointment expiry job disappeared".into()))?;
  let status: String = row.try_get("", "status")?;
  if status == "expired" {
    return Ok(ExpiryCheck::Expired);
  }
  let due_at: i64 = row.try_get("", "due_at")?;
  let finished_at: Option<i64> = row.try_get("", "finished_at")?;
  Ok(if finished_at.is_none() && due_at > now {
    ExpiryCheck::Deferred
  } else {
    ExpiryCheck::NotDue
  })
}

async fn process_reminder(
  tx: &sea_orm::DatabaseTransaction,
  claim: &Claim,
  appointment: &AppointmentState,
  reminder: Work,
  now: i64,
  origin: &str,
) -> Result<(), DbErr> {
  if appointment.starts_at <= now {
    return finish(tx, claim, now).await;
  }
  if matches!(reminder, Work::Reminder2h) && !practice_reminder_enabled(tx).await? {
    return finish(tx, claim, now).await;
  }
  let offset = if matches!(reminder, Work::Reminder24h) {
    86_400_000
  } else {
    7_200_000
  };
  let due_at = appointment.starts_at.saturating_sub(offset);
  if due_at > now {
    return defer(tx, claim, due_at).await;
  }
  let (text, purpose) = if appointment.status == "confirmed" {
    if let Some(text) = appointment_reminder(appointment) {
      (text, "reminder")
    } else if let Some(text) = appointment_status(appointment) {
      (text, "status")
    } else {
      return defer(tx, claim, now.saturating_add(LINK_RETRY_MS)).await;
    }
  } else {
    let Some(text) = appointment_status(appointment) else {
      return finish(tx, claim, now).await;
    };
    (text, "status")
  };
  let variant = if matches!(reminder, Work::Reminder24h) {
    "24h"
  } else {
    "2h"
  };
  let key = format!(
    "appointment:{}:reminders:{}:{variant}",
    claim.appointment_id, appointment.starts_at
  );
  enqueue_text(
    tx,
    Message {
      contact: appointment.contact_id,
      appointment: Some(claim.appointment_id),
      key: &key,
      text: &text,
      purpose,
      revision: Some(appointment.revision),
    },
    origin,
    now,
  )
  .await?;
  finish(tx, claim, now).await
}

fn review_message(
  appointment: &AppointmentState,
  appointment_id: Uuid,
  variant: Work,
  origin: &str,
) -> Option<String> {
  let local = local_time(appointment.starts_at, &appointment.timezone)?;
  let link = format!("{origin}/appointments/{appointment_id}");
  Some(if matches!(variant, Work::ReviewUrgent) {
    format!(
      "Aviso urgente: el comprobante de {} para la cita del {local} sigue en revisión. Revíselo en el panel: {link}",
      appointment.patient_name
    )
  } else {
    format!(
      "El comprobante de {} para la cita del {local} sigue pendiente de revisión. Revíselo en el panel: {link}",
      appointment.patient_name
    )
  })
}

async fn process_review(
  tx: &sea_orm::DatabaseTransaction,
  claim: &Claim,
  appointment: &AppointmentState,
  doctor: Option<Uuid>,
  variant: Work,
  now: i64,
  origin: &str,
) -> Result<(), DbErr> {
  if appointment.status != "awaiting_payment" {
    return finish(tx, claim, now).await;
  }
  let expiries = active_hold_expiries(tx, claim.appointment_id).await?;
  let hold_allows_review = match appointment.payment.as_str() {
    "under_review" => expiries.iter().any(Option::is_none),
    "awaiting_receipt" => {
      !expiries.is_empty()
        && expiries.iter().all(Option::is_some)
        && expiries
          .iter()
          .flatten()
          .copied()
          .max()
          .is_some_and(|expiry| expiry > now)
    }
    _ => false,
  };
  if !hold_allows_review {
    return finish(tx, claim, now).await;
  }
  let receipt_query = match appointment.payment.as_str() {
    "under_review" => {
      "SELECT EXISTS (SELECT 1 FROM receipts WHERE appointment_id=$1 AND bytes IS NOT NULL AND reviewed_at IS NULL AND deleted_at IS NULL) AS pending"
    }
    "awaiting_receipt" => {
      "SELECT EXISTS (SELECT 1 FROM receipts WHERE appointment_id=$1 AND bytes IS NOT NULL AND reviewed_at IS NULL AND deleted_at IS NULL AND timely=false AND late=false) AS pending"
    }
    _ => return finish(tx, claim, now).await,
  };
  let Some(evidence) = tx
    .query_one(sql(receipt_query, vec![claim.appointment_id.into()]))
    .await?
  else {
    return Err(DbErr::Custom(
      "Appointment Receipt evidence unavailable".into(),
    ));
  };
  if !evidence.try_get::<bool>("", "pending")? {
    return finish(tx, claim, now).await;
  }
  if matches!(variant, Work::ReviewUrgent) && appointment.starts_at <= now {
    return finish(tx, claim, now).await;
  }
  let Some(doctor) = doctor else {
    return defer(tx, claim, now.saturating_add(REVIEW_RETRY_MS)).await;
  };
  if matches!(variant, Work::ReviewPeriodic) {
    let Some(settings) = review_settings(tx).await? else {
      return defer(tx, claim, now.saturating_add(REVIEW_RETRY_MS)).await;
    };
    let Some(next_open) = next_working_time(&settings.working_hours, settings.timezone, now) else {
      return defer(
        tx,
        claim,
        now.saturating_add(i64::from(settings.interval_minutes) * 60_000),
      )
      .await;
    };
    if next_open > now {
      return defer(tx, claim, next_open).await;
    }
  }
  let Some(text) = review_message(appointment, claim.appointment_id, variant, origin) else {
    return defer(tx, claim, now.saturating_add(REVIEW_RETRY_MS)).await;
  };
  let variant_key = if matches!(variant, Work::ReviewUrgent) {
    "urgent"
  } else {
    "periodic"
  };
  let key = format!(
    "appointment:{}:{}:review:{variant_key}:{}",
    claim.appointment_id, appointment.revision, claim.due_at
  );
  enqueue_text(
    tx,
    Message {
      contact: doctor,
      appointment: Some(claim.appointment_id),
      key: &key,
      text: &text,
      purpose: "doctor_notification",
      revision: Some(appointment.revision),
    },
    origin,
    now,
  )
  .await?;
  if matches!(variant, Work::ReviewPeriodic) {
    let settings = review_settings(tx)
      .await?
      .ok_or_else(|| DbErr::Custom("Practice review settings are unavailable".into()))?;
    let due = now
      .checked_add(i64::from(settings.interval_minutes) * 60_000)
      .ok_or_else(|| DbErr::Custom("Review due time overflow".into()))?;
    store::job(
      tx,
      claim.appointment_id,
      "review",
      due,
      &format!(
        "{}:{}:review:periodic:{due}",
        claim.appointment_id, appointment.revision
      ),
    )
    .await?;
  }
  finish(tx, claim, now).await
}

async fn process(db: &Db, clock: &dyn Clock, claim: Claim, origin: &str) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  lock_contact(&tx, claim.contact_id).await?;
  let parsed = work(&claim.kind, &claim.key, claim.appointment_id);
  let is_review = matches!(
    parsed,
    Some((_, Work::ReviewPeriodic | Work::ReviewUrgent, _))
  );
  let doctor = if is_review {
    doctor_contact(&tx, claim.contact_id).await?
  } else {
    None
  };
  let Some(appointment) = appointment(&tx, claim.appointment_id).await? else {
    tx.commit().await?;
    return Ok(());
  };
  let now = clock.now_millis();
  if !job_current(&tx, &claim).await? {
    tx.commit().await?;
    return Ok(());
  }
  if appointment.contact_id != claim.contact_id {
    finish(&tx, &claim, now).await?;
    tx.commit().await?;
    return Ok(());
  }
  let Some((revision, task, scheduled_starts_at)) = parsed else {
    finish(&tx, &claim, now).await?;
    tx.commit().await?;
    return Ok(());
  };
  let reminder = matches!(task, Work::Reminder24h | Work::Reminder2h);
  let rejected_expiry = matches!(task, Work::Expiry)
    && appointment.status == "awaiting_payment"
    && appointment.payment == "rejected"
    && appointment.format == "video"
    && appointment
      .receipt_deadline
      .is_some_and(|deadline| claim.due_at >= deadline);
  if (appointment.revision != revision && !reminder && !rejected_expiry)
    || (reminder && scheduled_starts_at != Some(appointment.starts_at))
  {
    finish(&tx, &claim, now).await?;
    tx.commit().await?;
    return Ok(());
  }
  match task {
    Work::Expiry => process_expiry(&tx, &claim, &appointment, now, origin).await?,
    Work::Reminder24h | Work::Reminder2h => {
      process_reminder(&tx, &claim, &appointment, task, now, origin).await?
    }
    Work::ReviewPeriodic | Work::ReviewUrgent => {
      process_review(&tx, &claim, &appointment, doctor, task, now, origin).await?
    }
  }
  tx.commit().await?;
  Ok(())
}

/// Claims and completes one durable expiry, reminder, or review job. All state transitions,
/// History, and logical messages commit together; provider delivery remains the paced outbox's job.
pub async fn run_once(db: &Db, clock: &dyn Clock, cfg: &Config) -> Result<bool, DbErr> {
  let Some(origin) = cfg.public_origin.as_deref() else {
    return Ok(false);
  };
  let Some(claim) = claim(db, clock).await? else {
    return Ok(false);
  };
  process(db, clock, claim, origin).await?;
  Ok(true)
}
#[cfg(test)]
#[path = "timers_tests.rs"]
mod tests;
