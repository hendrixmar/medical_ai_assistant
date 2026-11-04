use crate::modules::{appointments::store as appointment_store, whatsapp::inbox::sql};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr};
use uuid::Uuid;

pub fn external_reference(appointment: Uuid, revision: i64) -> String {
  format!("ma1:{appointment}:{revision}")
}

/// Schedules a Checkout Pro preference after the caller's transaction commits. Skipped while the
/// access token has not been checked or was rejected; bank transfer stays available either way.
pub async fn schedule_preference(
  tx: &DatabaseTransaction,
  appointment: Uuid,
  revision: i64,
  amount_centavos: i64,
  deadline: i64,
  now: i64,
) -> Result<(), DbErr> {
  if amount_centavos <= 0 || deadline <= now {
    return Ok(());
  }
  if tx
    .query_one(sql(
      "SELECT 1 AS ready FROM mercado_pago_account WHERE id=1 AND user_id IS NOT NULL AND notice IS NULL FOR SHARE",
      vec![],
    ))
    .await?
    .is_none()
  {
    return Ok(());
  }
  tx.execute(sql(
    "INSERT INTO mercado_pago_preferences(appointment_id,revision,external_reference,amount_centavos,expires_at,created_at,outcome) VALUES ($1,$2,$3,$4,$5,$6,'pending') ON CONFLICT (appointment_id,revision) DO NOTHING",
    vec![
      appointment.into(),
      revision.into(),
      external_reference(appointment, revision).into(),
      amount_centavos.into(),
      deadline.into(),
      now.into(),
    ],
  ))
  .await?;
  appointment_store::job(
    tx,
    appointment,
    "mp_preference",
    now,
    &format!("{appointment}:{revision}:mp_preference"),
  )
  .await
}

/// Asks the worker to re-evaluate (and close) open preferences after an Appointment change.
pub async fn recheck_preferences(
  tx: &DatabaseTransaction,
  appointment: Uuid,
  now: i64,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE mercado_pago_preferences SET next_check_at=$2 WHERE appointment_id=$1 AND outcome IN ('active','closing')",
    vec![appointment.into(), now.into()],
  ))
  .await?;
  Ok(())
}

/// Mercado Pago payments the Doctor may refund from the dashboard: late, duplicate or
/// mismatched payments, and a confirmed payment whose Appointment was cancelled or expired.
const REFUNDABLE: &str = "SELECT p.payment_id FROM mercado_pago_payments p JOIN appointments a ON a.id=p.appointment_id WHERE p.appointment_id=$1 AND p.refund_state='none' AND p.amount_centavos>0 AND p.provider_status='approved' AND (p.outcome IN ('late','duplicate','mismatch') OR (p.outcome='confirmed' AND a.status IN ('cancelled','expired') AND a.payment='verified'))";

pub async fn refundable_payments<C: ConnectionTrait>(
  conn: &C,
  appointment: Uuid,
) -> Result<Vec<i64>, DbErr> {
  conn
    .query_all(sql(
      &format!("{REFUNDABLE} ORDER BY p.first_seen_at"),
      vec![appointment.into()],
    ))
    .await?
    .iter()
    .map(|row| row.try_get("", "payment_id"))
    .collect()
}

/// The Doctor's explicit refund request. The worker performs it with a fixed idempotency key.
pub async fn request_refund(
  tx: &DatabaseTransaction,
  appointment: Uuid,
  payment_id: i64,
  now: i64,
) -> Result<bool, DbErr> {
  let Some(owner) = tx
    .query_one(sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![appointment.into()],
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
  tx.query_one(sql(
    "SELECT id FROM appointments WHERE id=$1 FOR UPDATE",
    vec![appointment.into()],
  ))
  .await?;
  if tx
    .query_one(sql(
      &format!("{REFUNDABLE} AND p.payment_id=$2 FOR UPDATE OF p"),
      vec![appointment.into(), payment_id.into()],
    ))
    .await?
    .is_none()
  {
    return Ok(false);
  }
  tx.execute(sql(
    "UPDATE mercado_pago_payments SET refund_state='requested',updated_at=$2 WHERE payment_id=$1 AND refund_state='none'",
    vec![payment_id.into(), now.into()],
  ))
  .await?;
  appointment_store::history(
    tx,
    appointment,
    "Doctor",
    "mercado_pago_refund_requested",
    now,
  )
  .await?;
  appointment_store::job(
    tx,
    appointment,
    "mp_refund",
    now,
    &format!("{appointment}:{payment_id}:mp_refund"),
  )
  .await?;
  Ok(true)
}
