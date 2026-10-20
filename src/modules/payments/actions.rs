use crate::modules::{
  appointments::{
    self,
    model::Appointment,
    store as appointment_store,
    timers::{self, ExpiryCheck},
  },
  conversation::store::{enqueue_text, Message},
  whatsapp::inbox::sql,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionOutcome {
  Applied,
  Already,
  Stale,
  Late,
  Pending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectionChoice {
  NewReceipt,
  Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LateChoice {
  VerifiedRecovery,
  VerifiedRefund,
  Clarification,
}

#[derive(FromQueryResult)]
struct ReceiptRow {
  id: Uuid,
  contact_id: Uuid,
  inbox_message_id: String,
  appointment_id: Option<Uuid>,
  bytes_present: bool,
  deleted_at: Option<i64>,
  late: Option<bool>,
  reviewed_at: Option<i64>,
  clarification_requested_at: Option<i64>,
}

struct ReviewContext {
  receipt: ReceiptRow,
  appointment: Appointment,
  hold: super::intake::HoldState,
  expiry: ExpiryCheck,
}

pub async fn verify(
  tx: &DatabaseTransaction,
  receipt_id: Uuid,
  origin: &str,
  now: i64,
) -> Result<ActionOutcome, DbErr> {
  let Some(mut context) = lock_context(tx, receipt_id, origin, now).await? else {
    return Ok(ActionOutcome::Stale);
  };
  if context.receipt.reviewed_at.is_some() {
    return Ok(ActionOutcome::Already);
  }
  if !has_evidence(&context.receipt) {
    return Ok(ActionOutcome::Stale);
  }
  if let Some(outcome) = normal_review_gate(tx, &mut context, now).await? {
    return Ok(outcome);
  }

  if !matches!(
    context.appointment.payment.as_str(),
    "awaiting_receipt" | "under_review"
  ) {
    return Ok(ActionOutcome::Stale);
  }
  let revision = next_revision(context.appointment.revision)?;
  let event_id = context
    .appointment
    .event_id
    .clone()
    .unwrap_or_else(|| context.appointment.id.simple().to_string());
  let updated = tx
    .execute(sql(
      "UPDATE appointments SET status='confirmed',payment='verified',calendar_sync='pending',event_id=COALESCE(event_id,$2),revision=$3,updated_at=$4 WHERE id=$1 AND revision=$5 AND status='awaiting_payment' AND payment IN ('awaiting_receipt','under_review')",
      vec![context.appointment.id.into(), event_id.into(), revision.into(), now.into(), context.appointment.revision.into()],
    ))
    .await?;
  if updated.rows_affected() != 1 {
    return Ok(ActionOutcome::Stale);
  }
  let released = tx
    .execute(sql(
      "UPDATE appointment_holds SET expires_at=NULL WHERE appointment_id=$1 AND active",
      vec![context.appointment.id.into()],
    ))
    .await?;
  if released.rows_affected() == 0 {
    return Err(DbErr::Custom("Appointment Hold unavailable".into()));
  }
  if !review_receipt(tx, &context.receipt, "verified", now).await? {
    return Err(DbErr::Custom(
      "Receipt changed during payment verification".into(),
    ));
  }
  appointment_store::history(
    tx,
    context.appointment.id,
    "Doctor",
    "payment_verified",
    now,
  )
  .await?;
  resolve_review_attention(tx, context.receipt.contact_id, context.appointment.id, now).await?;
  appointment_store::job(
    tx,
    context.appointment.id,
    "calendar_sync",
    now,
    &format!("{}:{revision}:calendar_sync", context.appointment.id),
  )
  .await?;
  appointment_store::job(
    tx,
    context.appointment.id,
    "conflict",
    now,
    &format!("{}:{revision}:conflict", context.appointment.id),
  )
  .await?;
  enqueue_text(tx, Message { contact: context.receipt.contact_id, appointment: Some(context.appointment.id), key: &format!("receipt:{}:verified", context.receipt.id), text: "El doctor confirmó su pago. La cita está en proceso de confirmación del calendario; le avisaré cuando se complete.", purpose: "status", revision: Some(revision) }, origin, now)
  .await?;
  Ok(ActionOutcome::Applied)
}

pub async fn reject(
  tx: &DatabaseTransaction,
  receipt_id: Uuid,
  choice: RejectionChoice,
  origin: &str,
  now: i64,
) -> Result<ActionOutcome, DbErr> {
  if choice == RejectionChoice::Cancel {
    return reject_and_cancel(tx, receipt_id, origin, now).await;
  }
  let Some(mut context) = lock_context(tx, receipt_id, origin, now).await? else {
    return Ok(ActionOutcome::Stale);
  };
  if context.receipt.reviewed_at.is_some() {
    return Ok(ActionOutcome::Already);
  }
  if !has_evidence(&context.receipt) {
    return Ok(ActionOutcome::Stale);
  }
  if let Some(outcome) = normal_review_gate(tx, &mut context, now).await? {
    return Ok(outcome);
  }
  if !matches!(
    context.appointment.payment.as_str(),
    "awaiting_receipt" | "under_review"
  ) {
    return Ok(ActionOutcome::Stale);
  }

  let settings = tx
    .query_one(sql(
      "SELECT receipt_window_minutes,review_margin_minutes,grace_minutes FROM practice WHERE id=1",
      vec![],
    ))
    .await?;
  let mut new_window = None;
  if let Some(settings) = settings {
    let window = settings.try_get::<Option<i32>>("", "receipt_window_minutes")?;
    let margin = settings.try_get::<Option<i32>>("", "review_margin_minutes")?;
    let grace = settings.try_get::<Option<i32>>("", "grace_minutes")?;
    if let (
      Some(window),
      Some(margin),
      Some(grace),
      Some(reference),
      Some(bank),
      Some(beneficiary),
      Some(clabe),
    ) = (
      window,
      margin,
      grace,
      context.appointment.payment_reference.as_deref(),
      context.appointment.bank.as_deref(),
      context.appointment.beneficiary.as_deref(),
      context.appointment.clabe.as_deref(),
    ) {
      let window_ms = i64::from(window).saturating_mul(60_000);
      let margin_ms = i64::from(margin).saturating_mul(60_000);
      let grace_ms = i64::from(grace).saturating_mul(60_000);
      let deadline = now
        .saturating_add(window_ms)
        .min(context.appointment.starts_at.saturating_sub(margin_ms));
      if deadline.saturating_sub(now) >= 30 * 60_000 {
        new_window = Some((
          deadline,
          deadline.saturating_add(grace_ms),
          reference,
          bank,
          beneficiary,
          clabe,
        ));
      }
    }
  }

  let revision = next_revision(context.appointment.revision)?;
  if let Some((deadline, hold_expiry, reference, bank, beneficiary, clabe)) = new_window {
    let updated = tx
      .execute(sql(
        "UPDATE appointments SET payment='awaiting_receipt',receipt_deadline=$2,revision=$3,updated_at=$4 WHERE id=$1 AND revision=$5 AND status='awaiting_payment' AND payment IN ('awaiting_receipt','under_review')",
        vec![context.appointment.id.into(), deadline.into(), revision.into(), now.into(), context.appointment.revision.into()],
      ))
      .await?;
    if updated.rows_affected() != 1 {
      return Ok(ActionOutcome::Stale);
    }
    let held = tx
      .execute(sql(
        "UPDATE appointment_holds SET expires_at=$2 WHERE appointment_id=$1 AND active",
        vec![context.appointment.id.into(), hold_expiry.into()],
      ))
      .await?;
    if held.rows_affected() == 0 {
      return Err(DbErr::Custom("Appointment Hold unavailable".into()));
    }
    if !review_receipt(tx, &context.receipt, "new_receipt", now).await? {
      return Err(DbErr::Custom("Receipt changed during rejection".into()));
    }
    appointment_store::history(
      tx,
      context.appointment.id,
      "Doctor",
      "receipt_new_receipt_requested",
      now,
    )
    .await?;
    resolve_review_attention(tx, context.receipt.contact_id, context.appointment.id, now).await?;
    appointment_store::job(
      tx,
      context.appointment.id,
      "expiry",
      hold_expiry,
      &format!("{}:{revision}:expiry:deadline", context.appointment.id),
    )
    .await?;
    appointments::booking::enqueue_payment_instructions(
      tx,
      appointments::booking::PaymentInstructions {
        appointment: context.appointment.id,
        revision,
        contact: context.receipt.contact_id,
        deadline,
        price_centavos: context.appointment.price_centavos,
        bank,
        beneficiary,
        clabe,
        reference,
        timezone: &context.appointment.timezone,
      },
      origin,
      now,
    )
    .await?;
  } else {
    let original_expiry = if let Some(expiry) = context.hold.latest_expiry {
      Some(expiry)
    } else {
      tx.query_one(sql(
        "SELECT max(due_at) AS original_expiry FROM appointment_jobs WHERE appointment_id=$1 AND kind='expiry'",
        vec![context.appointment.id.into()],
      ))
      .await?
      .map(|row| row.try_get::<Option<i64>>("", "original_expiry"))
      .transpose()?
      .flatten()
    }
    .ok_or_else(|| DbErr::Custom("Original Appointment expiry unavailable".into()))?;
    let updated = tx
      .execute(sql(
        "UPDATE appointments SET payment='rejected',revision=$2,updated_at=$3 WHERE id=$1 AND revision=$4 AND status='awaiting_payment' AND payment IN ('awaiting_receipt','under_review')",
        vec![context.appointment.id.into(), revision.into(), now.into(), context.appointment.revision.into()],
      ))
      .await?;
    if updated.rows_affected() != 1 {
      return Ok(ActionOutcome::Stale);
    }
    if !review_receipt(tx, &context.receipt, "new_receipt", now).await? {
      return Err(DbErr::Custom("Receipt changed during rejection".into()));
    }
    appointment_store::history(
      tx,
      context.appointment.id,
      "Doctor",
      "payment_rejected",
      now,
    )
    .await?;
    appointment_store::history(
      tx,
      context.appointment.id,
      "Doctor",
      "receipt_new_receipt_requested",
      now,
    )
    .await?;
    resolve_review_attention(tx, context.receipt.contact_id, context.appointment.id, now).await?;
    create_followup(tx, &context.receipt, context.appointment.id, now).await?;
    appointment_store::job(
      tx,
      context.appointment.id,
      "expiry",
      original_expiry,
      &format!("{}:{revision}:expiry:deadline", context.appointment.id),
    )
    .await?;
    let text = if original_expiry > now {
      format!(
        "El doctor indicó que no recibió el pago y no pudo ofrecer un nuevo plazo seguro. El horario conserva su vencimiento original: {} ({}). Escriba 5 para recibir ayuda; la cita aún no está confirmada.",
        appointment_store::local_time(original_expiry, &context.appointment.timezone),
        context.appointment.timezone,
      )
    } else {
      "El doctor indicó que no recibió el pago y no pudo ofrecer un nuevo plazo seguro. El vencimiento original ya pasó y el horario se liberará; escriba 5 para recibir ayuda. La cita aún no está confirmada.".to_owned()
    };
    enqueue_text(
      tx,
      Message {
        contact: context.receipt.contact_id,
        appointment: Some(context.appointment.id),
        key: &format!("receipt:{}:no-new-window", context.receipt.id),
        text: &text,
        purpose: "status",
        revision: Some(revision),
      },
      origin,
      now,
    )
    .await?;
  }
  Ok(ActionOutcome::Applied)
}

async fn reject_and_cancel(
  tx: &DatabaseTransaction,
  receipt_id: Uuid,
  origin: &str,
  now: i64,
) -> Result<ActionOutcome, DbErr> {
  let Some(mut context) = lock_context(tx, receipt_id, origin, now).await? else {
    return Ok(ActionOutcome::Stale);
  };
  if context.receipt.reviewed_at.is_some() {
    return Ok(ActionOutcome::Already);
  }
  if !has_evidence(&context.receipt) {
    return Ok(ActionOutcome::Stale);
  }
  if let Some(outcome) = normal_review_gate(tx, &mut context, now).await? {
    return Ok(outcome);
  }
  if !matches!(
    context.appointment.payment.as_str(),
    "awaiting_receipt" | "under_review"
  ) || context.appointment.starts_at <= now
  {
    return Ok(ActionOutcome::Stale);
  }
  let revision = next_revision(context.appointment.revision)?;
  let updated = tx
    .execute(sql(
      "UPDATE appointments SET payment='rejected',revision=$2,updated_at=$3 WHERE id=$1 AND revision=$4 AND status='awaiting_payment' AND payment IN ('awaiting_receipt','under_review')",
      vec![context.appointment.id.into(), revision.into(), now.into(), context.appointment.revision.into()],
    ))
    .await?;
  if updated.rows_affected() != 1 {
    return Ok(ActionOutcome::Stale);
  }
  if !review_receipt(tx, &context.receipt, "cancelled", now).await? {
    return Err(DbErr::Custom(
      "Receipt changed during rejection cancellation".into(),
    ));
  }
  appointment_store::history(
    tx,
    context.appointment.id,
    "Doctor",
    "payment_rejected",
    now,
  )
  .await?;
  if !appointment_store::cancel(tx, context.appointment.id, "Doctor", origin, now).await? {
    return Err(DbErr::Custom(
      "Rejected Appointment cancellation unavailable".into(),
    ));
  }
  resolve_review_attention(tx, context.receipt.contact_id, context.appointment.id, now).await?;
  Ok(ActionOutcome::Applied)
}

pub async fn cancel(
  tx: &DatabaseTransaction,
  receipt_id: Uuid,
  origin: &str,
  now: i64,
) -> Result<ActionOutcome, DbErr> {
  let Some(context) = lock_context(tx, receipt_id, origin, now).await? else {
    return Ok(ActionOutcome::Stale);
  };
  if context.appointment.status == "cancelled" {
    return Ok(ActionOutcome::Already);
  }
  if context.appointment.status == "expired" || matches!(&context.expiry, ExpiryCheck::Expired) {
    if context.receipt.reviewed_at.is_none() && has_evidence(&context.receipt) {
      mark_late(tx, &context.receipt, context.appointment.id, now).await?;
    }
    return Ok(ActionOutcome::Late);
  }
  if matches!(&context.expiry, ExpiryCheck::Deferred) {
    return Ok(ActionOutcome::Pending);
  }
  if context.hold.exists && context.hold.due_at(now) {
    if context.receipt.reviewed_at.is_none() && has_evidence(&context.receipt) {
      mark_late(tx, &context.receipt, context.appointment.id, now).await?;
    }
    return Ok(ActionOutcome::Late);
  }
  if !matches!(
    context.appointment.status.as_str(),
    "awaiting_payment" | "confirmed"
  ) || context.appointment.starts_at <= now
  {
    return Ok(ActionOutcome::Stale);
  }
  if context.receipt.reviewed_at.is_none()
    && has_evidence(&context.receipt)
    && !review_receipt(tx, &context.receipt, "cancelled", now).await?
  {
    return Err(DbErr::Custom("Receipt changed during cancellation".into()));
  }
  let cancelled =
    appointment_store::cancel(tx, context.appointment.id, "Doctor", origin, now).await?;
  if !cancelled {
    return Err(DbErr::Custom("Appointment cancellation unavailable".into()));
  }
  resolve_review_attention(tx, context.receipt.contact_id, context.appointment.id, now).await?;
  Ok(ActionOutcome::Applied)
}

pub async fn late_outcome(
  tx: &DatabaseTransaction,
  receipt_id: Uuid,
  choice: LateChoice,
  origin: &str,
  now: i64,
) -> Result<ActionOutcome, DbErr> {
  let Some(mut context) = lock_context(tx, receipt_id, origin, now).await? else {
    return Ok(ActionOutcome::Stale);
  };
  if context.receipt.reviewed_at.is_some() {
    return Ok(ActionOutcome::Already);
  }
  if !has_evidence(&context.receipt) {
    return Ok(ActionOutcome::Stale);
  }
  if context.appointment.status != "expired" {
    if matches!(&context.expiry, ExpiryCheck::Deferred) {
      return Ok(ActionOutcome::Pending);
    }
    if matches!(&context.expiry, ExpiryCheck::Expired)
      || (context.hold.exists && context.hold.due_at(now))
    {
      mark_late(tx, &context.receipt, context.appointment.id, now).await?;
      return Ok(ActionOutcome::Pending);
    }
    return Ok(ActionOutcome::Stale);
  }
  if context.receipt.late != Some(true) {
    mark_late(tx, &context.receipt, context.appointment.id, now).await?;
    context.receipt.late = Some(true);
  }
  match choice {
    LateChoice::Clarification => {
      if context.receipt.clarification_requested_at.is_some() {
        return Ok(ActionOutcome::Already);
      }
      let updated = tx
        .execute(sql(
          "UPDATE receipts SET clarification_requested_at=$2 WHERE id=$1 AND reviewed_at IS NULL AND clarification_requested_at IS NULL",
          vec![context.receipt.id.into(), now.into()],
        ))
        .await?;
      if updated.rows_affected() != 1 {
        return Ok(ActionOutcome::Already);
      }
      appointment_store::history(
        tx,
        context.appointment.id,
        "Doctor",
        "late_receipt_clarification_requested",
        now,
      )
      .await?;
      resolve_attention_kind(
        tx,
        context.receipt.contact_id,
        context.appointment.id,
        "late_receipt",
        now,
      )
      .await?;
      appointment_store::attention(
        tx,
        Some(context.appointment.id),
        Some(context.receipt.contact_id),
        "receipt_clarification",
        now,
      )
      .await?;
      enqueue_text(tx, Message { contact: context.receipt.contact_id, appointment: Some(context.appointment.id), key: &format!("receipt:{}:clarification", context.receipt.id), text: "El doctor recibió el comprobante después de vencer la cita y necesita aclarar un detalle. La cita sigue vencida y aún no está confirmada.", purpose: "status", revision: Some(context.appointment.revision) }, origin, now)
      .await?;
      Ok(ActionOutcome::Applied)
    }
    LateChoice::VerifiedRecovery | LateChoice::VerifiedRefund => {
      if !matches!(
        context.appointment.payment.as_str(),
        "awaiting_receipt" | "under_review" | "rejected"
      ) {
        return Ok(ActionOutcome::Stale);
      }
      let revision = next_revision(context.appointment.revision)?;
      let updated = tx
        .execute(sql(
          "UPDATE appointments SET payment='verified',revision=$2,updated_at=$3 WHERE id=$1 AND revision=$4 AND status='expired' AND payment IN ('awaiting_receipt','under_review','rejected')",
          vec![context.appointment.id.into(), revision.into(), now.into(), context.appointment.revision.into()],
        ))
        .await?;
      if updated.rows_affected() != 1 {
        return Ok(ActionOutcome::Stale);
      }
      let review_outcome = match choice {
        LateChoice::VerifiedRecovery => "late_verified_recovery",
        LateChoice::VerifiedRefund => "late_verified_refund",
        LateChoice::Clarification => unreachable!(),
      };
      if !review_receipt(tx, &context.receipt, review_outcome, now).await? {
        return Err(DbErr::Custom(
          "Late Receipt changed during verification".into(),
        ));
      }
      appointment_store::history(
        tx,
        context.appointment.id,
        "Doctor",
        "payment_verified",
        now,
      )
      .await?;
      let decision_kind = match choice {
        LateChoice::VerifiedRecovery => "late_receipt_verified_recovery",
        LateChoice::VerifiedRefund => "late_receipt_verified_refund",
        LateChoice::Clarification => unreachable!(),
      };
      appointment_store::history(tx, context.appointment.id, "Doctor", decision_kind, now).await?;
      resolve_review_attention(tx, context.receipt.contact_id, context.appointment.id, now).await?;
      match choice {
        LateChoice::VerifiedRecovery => {
          if !appointments::booking::begin_late_recovery(tx, context.appointment.id, origin, now)
            .await?
          {
            return Err(DbErr::Custom("Late Receipt recovery unavailable".into()));
          }
        }
        LateChoice::VerifiedRefund => {
          appointment_store::attention(
            tx,
            Some(context.appointment.id),
            Some(context.receipt.contact_id),
            "refund_due",
            now,
          )
          .await?;
          enqueue_text(tx, Message { contact: context.receipt.contact_id, appointment: Some(context.appointment.id), key: &format!("receipt:{}:late-refund", context.receipt.id), text: "El doctor verificó el pago recibido después de vencer la cita. La cita permanece vencida y el reembolso requiere gestión manual; no se realizó ningún reembolso automático.", purpose: "status", revision: Some(revision) }, origin, now)
          .await?;
        }
        LateChoice::Clarification => unreachable!(),
      }
      Ok(ActionOutcome::Applied)
    }
  }
}

async fn lock_context(
  tx: &DatabaseTransaction,
  receipt_id: Uuid,
  origin: &str,
  now: i64,
) -> Result<Option<ReviewContext>, DbErr> {
  let Some(target) = tx
    .query_one(sql(
      "SELECT contact_id,appointment_id FROM receipts WHERE id=$1",
      vec![receipt_id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  let contact: Uuid = target.try_get("", "contact_id")?;
  let Some(appointment_id) = target.try_get::<Option<Uuid>>("", "appointment_id")? else {
    return Ok(None);
  };
  tx.query_one(sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![contact.into()],
  ))
  .await?
  .ok_or_else(|| DbErr::Custom("Receipt Contact unavailable".into()))?;
  let expiry = timers::expire_if_due(tx, appointment_id, origin, now).await?;
  let Some(app_row) = tx
    .query_one(sql(
      "SELECT id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,booking_calendar,google_generation,google_selection_version,event_id,event_etag,meet_url,revision,attendance_confirmed_at,cancelled_at,expired_at,created_at,updated_at FROM appointments WHERE id=$1 FOR UPDATE",
      vec![appointment_id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  let appointment = Appointment::from_query_result(&app_row, "")?;
  if appointment.contact_id != contact {
    return Ok(None);
  }
  let hold = super::intake::active_hold(tx, appointment.id).await?;
  let Some(receipt_row) = tx
    .query_one(sql(
      "SELECT id,contact_id,inbox_message_id,appointment_id,bytes IS NOT NULL AS bytes_present,deleted_at,late,reviewed_at,clarification_requested_at FROM receipts WHERE id=$1 FOR UPDATE",
      vec![receipt_id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  let receipt = ReceiptRow::from_query_result(&receipt_row, "")?;
  if receipt.contact_id != contact || receipt.appointment_id != Some(appointment.id) {
    return Ok(None);
  }
  Ok(Some(ReviewContext {
    receipt,
    appointment,
    hold,
    expiry,
  }))
}

async fn normal_review_gate(
  tx: &DatabaseTransaction,
  context: &mut ReviewContext,
  now: i64,
) -> Result<Option<ActionOutcome>, DbErr> {
  if context.appointment.status == "expired" || matches!(&context.expiry, ExpiryCheck::Expired) {
    mark_late(tx, &context.receipt, context.appointment.id, now).await?;
    return Ok(Some(ActionOutcome::Late));
  }
  if matches!(&context.expiry, ExpiryCheck::Deferred) {
    return Ok(Some(ActionOutcome::Pending));
  }
  if context.receipt.late == Some(true) || (context.hold.exists && context.hold.due_at(now)) {
    mark_late(tx, &context.receipt, context.appointment.id, now).await?;
    return Ok(Some(ActionOutcome::Late));
  }
  if !context.hold.exists
    || context.appointment.status != "awaiting_payment"
    || !matches!(
      context.appointment.payment.as_str(),
      "awaiting_receipt" | "under_review"
    )
  {
    return Ok(Some(ActionOutcome::Stale));
  }
  Ok(None)
}

fn has_evidence(receipt: &ReceiptRow) -> bool {
  receipt.bytes_present && receipt.deleted_at.is_none()
}

async fn mark_late(
  tx: &DatabaseTransaction,
  receipt: &ReceiptRow,
  appointment: Uuid,
  now: i64,
) -> Result<(), DbErr> {
  let changed = tx
    .execute(sql(
      "UPDATE receipts SET late=true WHERE id=$1 AND late IS DISTINCT FROM TRUE",
      vec![receipt.id.into()],
    ))
    .await?;
  if changed.rows_affected() != 0 {
    appointment_store::history(tx, appointment, "System", "receipt_late_received", now).await?;
  }
  resolve_attention_kind(tx, receipt.contact_id, appointment, "receipt_review", now).await?;
  appointment_store::attention(
    tx,
    Some(appointment),
    Some(receipt.contact_id),
    "late_receipt",
    now,
  )
  .await?;
  Ok(())
}

async fn review_receipt(
  tx: &DatabaseTransaction,
  receipt: &ReceiptRow,
  outcome: &str,
  now: i64,
) -> Result<bool, DbErr> {
  let changed = tx
    .execute(sql(
      "UPDATE receipts SET reviewed_at=$2,review_outcome=$3 WHERE id=$1 AND reviewed_at IS NULL AND bytes IS NOT NULL AND deleted_at IS NULL",
      vec![receipt.id.into(), now.into(), outcome.into()],
    ))
    .await?;
  Ok(changed.rows_affected() == 1)
}

async fn create_followup(
  tx: &DatabaseTransaction,
  receipt: &ReceiptRow,
  appointment: Uuid,
  now: i64,
) -> Result<(), DbErr> {
  let Some(inbox) = tx
    .query_one(sql(
      "SELECT original_chat_identity FROM inbox_messages WHERE message_id=$1",
      vec![receipt.inbox_message_id.clone().into()],
    ))
    .await?
  else {
    return Err(DbErr::Custom("Receipt source unavailable".into()));
  };
  let target: String = inbox.try_get("", "original_chat_identity")?;
  tx.execute(sql(
    "INSERT INTO followup_requests(id,contact_id,inbox_message_id,original_target,created_at) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
    vec![Uuid::now_v7().into(), receipt.contact_id.into(), receipt.inbox_message_id.clone().into(), target.into(), now.into()],
  ))
  .await?;
  appointment_store::attention(
    tx,
    Some(appointment),
    Some(receipt.contact_id),
    "receipt_followup",
    now,
  )
  .await?;
  Ok(())
}

async fn resolve_review_attention(
  tx: &DatabaseTransaction,
  contact: Uuid,
  appointment: Uuid,
  now: i64,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE attention_items SET resolved_at=$3 WHERE contact_id=$1 AND appointment_id=$2 AND resolved_at IS NULL AND kind IN ('receipt_review','late_receipt','receipt_clarification','receipt_followup')",
    vec![contact.into(), appointment.into(), now.into()],
  ))
  .await?;
  Ok(())
}

async fn resolve_attention_kind(
  tx: &DatabaseTransaction,
  contact: Uuid,
  appointment: Uuid,
  kind: &str,
  now: i64,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE attention_items SET resolved_at=$4 WHERE contact_id=$1 AND appointment_id=$2 AND kind=$3 AND resolved_at IS NULL",
    vec![contact.into(), appointment.into(), kind.into(), now.into()],
  ))
  .await?;
  Ok(())
}

fn next_revision(current: i64) -> Result<i64, DbErr> {
  current
    .checked_add(1)
    .ok_or_else(|| DbErr::Custom("Appointment revision exhausted".into()))
}
