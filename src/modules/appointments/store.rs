use crate::modules::{
  conversation::store::{enqueue_text, Message},
  whatsapp::inbox::sql,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult};
use uuid::Uuid;

use super::model::Appointment;

pub async fn get<C: ConnectionTrait>(conn: &C, id: Uuid) -> Result<Option<Appointment>, DbErr> {
  conn
    .query_one(sql(
      "SELECT id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,booking_calendar,google_generation,google_selection_version,event_id,event_etag,meet_url,revision,attendance_confirmed_at,cancelled_at,expired_at,created_at,updated_at FROM appointments WHERE id=$1",
      vec![id.into()],
    ))
    .await?
    .map(|row| Appointment::from_query_result(&row, ""))
    .transpose()
}

pub async fn open_for_contact<C: ConnectionTrait>(
  conn: &C,
  contact: Uuid,
  now: i64,
) -> Result<Vec<Appointment>, DbErr> {
  conn
    .query_all(sql(
      "SELECT id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,booking_calendar,google_generation,google_selection_version,event_id,event_etag,meet_url,revision,attendance_confirmed_at,cancelled_at,expired_at,created_at,updated_at FROM appointments WHERE contact_id=$1 AND status IN ('requested','awaiting_payment','confirmed') AND starts_at>$2 ORDER BY starts_at,id",
      vec![contact.into(), now.into()],
    ))
    .await?
    .iter()
    .map(|row| Appointment::from_query_result(row, ""))
    .collect()
}

pub async fn history(
  tx: &DatabaseTransaction,
  id: Uuid,
  actor: &str,
  kind: &str,
  now: i64,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "INSERT INTO appointment_history(id,appointment_id,actor,kind,created_at) VALUES ($1,$2,$3,$4,$5)",
    vec![Uuid::now_v7().into(), id.into(), actor.into(), kind.into(), now.into()],
  ))
  .await?;
  Ok(())
}

pub async fn attention(
  tx: &DatabaseTransaction,
  appointment: Option<Uuid>,
  contact: Option<Uuid>,
  kind: &str,
  now: i64,
) -> Result<(), DbErr> {
  let key = format!(
    "{kind}:{}:{}:{}",
    appointment.map_or_else(|| "-".to_owned(), |id| id.to_string()),
    contact.map_or_else(|| "-".to_owned(), |id| id.to_string()),
    Uuid::now_v7()
  );
  tx.execute(sql(
    "INSERT INTO attention_items(id,appointment_id,contact_id,kind,created_at,dedupe_key) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",
    vec![Uuid::now_v7().into(), appointment.into(), contact.into(), kind.into(), now.into(), key.into()],
  )).await?;
  Ok(())
}

pub async fn job(
  tx: &DatabaseTransaction,
  id: Uuid,
  kind: &str,
  due: i64,
  key: &str,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "INSERT INTO appointment_jobs(id,appointment_id,kind,due_at,dedupe_key) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (dedupe_key) DO NOTHING",
    vec![Uuid::now_v7().into(), id.into(), kind.into(), due.into(), key.into()],
  ))
  .await?;
  Ok(())
}

pub async fn cancel(
  tx: &DatabaseTransaction,
  id: Uuid,
  actor: &str,
  origin: &str,
  now: i64,
) -> Result<bool, DbErr> {
  let Some(owner) = tx
    .query_one(sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![id.into()],
    ))
    .await?
  else {
    return Ok(false);
  };
  let contact: Uuid = owner.try_get("", "contact_id")?;
  tx.query_one(sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![contact.into()],
  ))
  .await?
  .ok_or_else(|| DbErr::Custom("Appointment Contact unavailable".into()))?;
  let Some(row) = tx
    .query_one(sql(
      "SELECT id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,booking_calendar,google_generation,google_selection_version,event_id,event_etag,meet_url,revision,attendance_confirmed_at,cancelled_at,expired_at,created_at,updated_at FROM appointments WHERE id=$1 FOR UPDATE",
      vec![id.into()],
    ))
    .await?
  else {
    return Ok(false);
  };
  let appointment = Appointment::from_query_result(&row, "")?;
  if appointment.contact_id != contact
    || !matches!(
      appointment.status.as_str(),
      "requested" | "awaiting_payment" | "confirmed"
    )
    || appointment.starts_at <= now
  {
    return Ok(false);
  }

  let revision = appointment
    .revision
    .checked_add(1)
    .ok_or_else(|| DbErr::Custom("Appointment revision exhausted".into()))?;
  let calendar_pending = appointment.event_id.is_some();
  tx.execute(sql(
    "UPDATE appointments SET status='cancelled',cancelled_at=$2,calendar_sync=CASE WHEN $3 THEN 'pending' ELSE calendar_sync END,revision=$4,updated_at=$2 WHERE id=$1",
    vec![id.into(), now.into(), calendar_pending.into(), revision.into()],
  ))
  .await?;
  tx.execute(sql(
    "UPDATE appointment_holds SET active=false WHERE appointment_id=$1 AND active",
    vec![id.into()],
  ))
  .await?;
  history(tx, id, actor, "cancelled", now).await?;
  if appointment.event_id.is_some() {
    job(
      tx,
      id,
      "calendar_cancel",
      now,
      &format!("{id}:{revision}:calendar_cancel"),
    )
    .await?;
  }
  if appointment.payment == "verified" {
    attention(tx, Some(id), Some(contact), "refund_due", now).await?;
  }

  let local = local_time(appointment.starts_at, &appointment.timezone);
  let subject = format!(
    "la cita de {} para {} del {local}",
    appointment.service_name, appointment.patient_name
  );
  let text = if calendar_pending {
    format!("Quedó cancelada {subject} en los registros. La actualización de Google Calendar está pendiente.")
  } else {
    format!("Quedó cancelada {subject} en los registros.")
  };
  enqueue_text(
    tx,
    Message {
      contact,
      appointment: Some(id),
      key: &format!("appointment:{id}:{revision}:cancelled"),
      text: &text,
      purpose: "status",
      revision: Some(revision),
    },
    origin,
    now,
  )
  .await?;
  Ok(true)
}

pub(crate) fn local_time(starts_at: i64, timezone: &str) -> String {
  let Some(timezone) = timezone.parse::<chrono_tz::Tz>().ok() else {
    return "en el horario registrado".into();
  };
  let Some(utc) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(starts_at) else {
    return "en el horario registrado".into();
  };
  utc
    .with_timezone(&timezone)
    .format("%d/%m/%Y a las %H:%M %Z")
    .to_string()
}

pub(crate) async fn resolve_refund(
  tx: &DatabaseTransaction,
  id: Uuid,
  now: i64,
) -> Result<bool, DbErr> {
  let Some(owner) = tx
    .query_one(sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![id.into()],
    ))
    .await?
  else {
    return Ok(false);
  };
  let contact: Uuid = owner.try_get("", "contact_id")?;
  tx.query_one(sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![contact.into()],
  ))
  .await?
  .ok_or_else(|| DbErr::Custom("Appointment Contact unavailable".into()))?;
  let Some(current) = tx.query_one(sql(
    "SELECT contact_id,(status IN ('cancelled','expired') AND payment='verified') AS eligible FROM appointments WHERE id=$1 FOR UPDATE",
    vec![id.into()],
  )).await? else {
    return Ok(false);
  };
  if current.try_get::<Uuid>("", "contact_id")? != contact
    || !current.try_get::<bool>("", "eligible")?
  {
    return Ok(false);
  }
  let changed = tx.execute(sql(
    "UPDATE attention_items SET resolved_at=$2 WHERE appointment_id=$1 AND kind='refund_due' AND resolved_at IS NULL",
    vec![id.into(), now.into()],
  )).await?.rows_affected() > 0;
  if changed {
    history(tx, id, "Doctor", "refund_resolved", now).await?;
  }
  Ok(changed)
}
