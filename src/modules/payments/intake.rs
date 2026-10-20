use crate::{
  database::Db,
  modules::{
    appointments::{
      model::Appointment,
      store as appointment_store,
      timers::{self, ExpiryCheck},
    },
    conversation::store::{enqueue_text, Message},
    whatsapp::{
      self,
      client::{Client, MediaDownloadError},
      inbox::sql,
      media::{self, InvalidMedia},
    },
  },
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use uuid::Uuid;

const MEDIA_COPY_LEASE_MS: i64 = 15_000;
const RETRY_DELAYS_MS: [i64; 4] = [5_000, 10_000, 20_000, 40_000];

#[derive(Debug)]
pub struct Unavailable;

impl From<DbErr> for Unavailable {
  fn from(_: DbErr) -> Self {
    Self
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiveOutcome {
  Duplicate,
  Discarded,
  Unmatched,
  Unsupported,
  Attached {
    receipt_id: Uuid,
    appointment_id: Uuid,
    timely: bool,
    late: bool,
  },
  AwaitingSelection {
    receipt_id: Uuid,
    candidate_count: usize,
  },
  MediaUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionActor {
  Contact,
  Doctor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectOutcome {
  Attached { timely: bool, late: bool },
  Stale,
  AlreadySelected,
}

struct Claim {
  inbox_id: String,
  contact: Uuid,
  candidates: Vec<Appointment>,
  attempt: i32,
}

enum Prepared {
  Complete(ReceiveOutcome),
  Retry,
  Fetch(Claim),
}

#[derive(Clone, Copy)]
pub(crate) struct HoldState {
  pub(crate) exists: bool,
  pub(crate) unbounded: bool,
  pub(crate) latest_expiry: Option<i64>,
}

impl HoldState {
  pub(crate) fn expired_at(self, at: i64) -> bool {
    !self.exists || (!self.unbounded && self.latest_expiry.is_some_and(|expiry| at >= expiry))
  }

  pub(crate) fn due_at(self, at: i64) -> bool {
    self.expired_at(at)
  }
}

pub struct PendingReceipt {
  pub contact: Option<Uuid>,
  pub url: Option<String>,
  pub mimetype: Option<String>,
  pub sent_at: i64,
  pub arrived_at: i64,
}
pub async fn receive(
  db: &Db,
  client: Option<&Client>,
  inbox_message_id: &str,
  receipt: &PendingReceipt,
  now_ms: i64,
  origin: Option<&str>,
) -> Result<ReceiveOutcome, Unavailable> {
  let media_url = receipt.url.as_deref();
  let declared_mime = receipt.mimetype.as_deref();
  let sent_at_ms = receipt.sent_at;
  let arrived_at_ms = receipt.arrived_at;
  let origin = origin.ok_or(Unavailable)?;
  if inbox_message_id.is_empty()
    || inbox_message_id.len() > 512
    || inbox_message_id.chars().any(char::is_control)
    || !(0..=253_402_300_799_000).contains(&sent_at_ms)
    || arrived_at_ms < 0
    || arrived_at_ms > now_ms
    || now_ms < 0
  {
    return Err(Unavailable);
  }

  let prepared = {
    let tx = db.conn.begin().await?;
    whatsapp::bound_transaction(&tx).await?;
    let prepared = prepare(
      &tx,
      inbox_message_id,
      receipt,
      now_ms,
      origin,
      client.is_some(),
    )
    .await?;
    tx.commit().await?;
    prepared
  };

  let claim = match prepared {
    Prepared::Complete(outcome) => return Ok(outcome),
    Prepared::Retry => return Err(Unavailable),
    Prepared::Fetch(claim) => claim,
  };
  let Some(client) = client else {
    return Err(Unavailable);
  };
  let raw_url = media_url.ok_or(Unavailable)?;
  let bytes = match client.download_media(raw_url).await {
    Ok(bytes) => bytes,
    Err(MediaDownloadError::Unavailable) => {
      return retry_or_finish(db, &claim, origin, now_ms).await;
    }
    Err(MediaDownloadError::TooLarge | MediaDownloadError::UnsafeUrl) => {
      return finish_invalid(db, &claim, origin, now_ms).await;
    }
  };
  let mime = match media::canonical_mime(declared_mime.unwrap_or_default(), &bytes) {
    Ok(mime) => mime,
    Err(InvalidMedia::Unsupported | InvalidMedia::TooLarge) => {
      return finish_invalid(db, &claim, origin, now_ms).await;
    }
  };
  finalize(db, &claim, mime, bytes, origin, now_ms).await
}

async fn prepare(
  tx: &DatabaseTransaction,
  inbox_id: &str,
  receipt: &PendingReceipt,
  now: i64,
  origin: &str,
  client_available: bool,
) -> Result<Prepared, DbErr> {
  let contact = receipt.contact;
  let media_url = receipt.url.as_deref();
  let declared_mime = receipt.mimetype.as_deref();
  let sent_at = receipt.sent_at;
  let arrived_at = receipt.arrived_at;
  let inbox_contact = tx
    .query_one(sql(
      "SELECT contact_id FROM inbox_messages WHERE message_id=$1",
      vec![inbox_id.into()],
    ))
    .await?
    .map(|row| row.try_get::<Option<Uuid>>("", "contact_id"))
    .transpose()?
    .flatten();
  let Some(contact_id) = contact.filter(|id| Some(*id) == inbox_contact) else {
    complete_marker(tx, inbox_id, now, None).await?;
    return Ok(Prepared::Complete(ReceiveOutcome::Discarded));
  };

  let doctor = doctor_contact(tx).await?;
  if lock_contact(tx, contact_id).await?.is_none() {
    complete_marker(tx, inbox_id, now, None).await?;
    return Ok(Prepared::Complete(ReceiveOutcome::Discarded));
  }
  if let Some(doctor) = doctor.filter(|doctor| *doctor != contact_id) {
    let _ = lock_contact(tx, doctor).await?;
  }
  let conflict = tx
    .query_one(sql(
      "SELECT identity_conflict FROM contacts WHERE id=$1",
      vec![contact_id.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Receipt Contact unavailable".into()))?
    .try_get::<bool>("", "identity_conflict")?;
  let candidates = candidate_appointments(tx, contact_id).await?;
  let marker = lock_marker(tx, inbox_id).await?;
  let state: String = marker.try_get("", "state")?;
  if state == "complete" {
    return Ok(Prepared::Complete(ReceiveOutcome::Duplicate));
  }
  if marker.try_get::<i64>("", "sent_at")? != sent_at
    || marker.try_get::<i64>("", "arrived_at")? != arrived_at
  {
    return Err(DbErr::Custom("Receipt intake timestamps changed".into()));
  }
  if marker.try_get::<i64>("", "available_at")? > now {
    return Ok(Prepared::Retry);
  }
  if conflict {
    complete_locked_marker(tx, inbox_id, now, None).await?;
    return Ok(Prepared::Complete(ReceiveOutcome::Discarded));
  }
  if candidates.is_empty() {
    complete_locked_marker(tx, inbox_id, now, None).await?;
    enqueue_text(tx, Message { contact: contact_id, appointment: None, key: &format!("receipt-intake:{inbox_id}:unmatched"), text: "No encontré una cita con pago pendiente para este comprobante. Si necesita ayuda, escriba 5 para pedir seguimiento.", purpose: "status", revision: None }, origin, now)
  .await?;
    return Ok(Prepared::Complete(ReceiveOutcome::Unmatched));
  }
  let supported = declared_mime.is_some_and(|mime| media::supported_mime(mime).is_some());
  let bounded_url = media_url
    .is_some_and(|url| !url.is_empty() && url.len() <= 8192 && !url.chars().any(char::is_control));
  if !supported || !bounded_url {
    complete_locked_marker(tx, inbox_id, now, Some("invalid_media")).await?;
    enqueue_unsupported(tx, contact_id, inbox_id, origin, now).await?;
    return Ok(Prepared::Complete(ReceiveOutcome::Unsupported));
  }
  if !client_available {
    return Ok(Prepared::Retry);
  }

  let attempts: i32 = marker.try_get("", "attempts")?;
  if attempts >= 5 {
    finish_copy_failure(tx, contact_id, inbox_id, &candidates, origin, now).await?;
    return Ok(Prepared::Complete(ReceiveOutcome::MediaUnavailable));
  }
  let attempt = attempts + 1;
  tx.execute(sql(
  "UPDATE receipt_intake_state SET attempts=$2,available_at=$3,last_error_code=NULL,updated_at=$4 WHERE inbox_message_id=$1 AND state='pending'",
  vec![inbox_id.into(), attempt.into(), now.saturating_add(MEDIA_COPY_LEASE_MS).into(), now.into()],
))
.await?;
  Ok(Prepared::Fetch(Claim {
    inbox_id: inbox_id.to_owned(),
    contact: contact_id,
    candidates,
    attempt,
  }))
}

async fn finalize(
  db: &Db,
  claim: &Claim,
  mime: &str,
  bytes: Vec<u8>,
  origin: &str,
  now: i64,
) -> Result<ReceiveOutcome, Unavailable> {
  let tx = db.conn.begin().await?;
  whatsapp::bound_transaction(&tx).await?;
  if lock_contact(&tx, claim.contact).await?.is_none() {
    complete_marker(&tx, &claim.inbox_id, now, Some("invalid_media")).await?;
    tx.commit().await?;
    return Ok(ReceiveOutcome::Discarded);
  }
  let conflict = tx
    .query_one(sql(
      "SELECT identity_conflict FROM contacts WHERE id=$1",
      vec![claim.contact.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Receipt Contact unavailable".into()))?
    .try_get::<bool>("", "identity_conflict")?;
  let doctor = doctor_contact(&tx).await?;
  if let Some(doctor) = doctor.filter(|doctor| *doctor != claim.contact) {
    let _ = lock_contact(&tx, doctor).await?;
  }
  expire_candidates(&tx, &claim.candidates, origin, now).await?;
  lock_appointments(&tx, &claim.candidates).await?;

  let mut current = Vec::with_capacity(claim.candidates.len());
  for candidate in &claim.candidates {
    if let Some(appointment) = appointment_store::get(&tx, candidate.id).await? {
      if appointment.contact_id == claim.contact && eligible_candidate(&appointment) {
        current.push(appointment);
      }
    }
  }
  let single_hold = if claim.candidates.len() == 1 && current.len() == 1 {
    Some(active_hold(&tx, current[0].id).await?)
  } else {
    None
  };
  let marker = lock_marker(&tx, &claim.inbox_id).await?;
  let state: String = marker.try_get("", "state")?;
  if state == "complete" {
    tx.commit().await?;
    return Ok(ReceiveOutcome::Duplicate);
  }
  if marker.try_get::<i32>("", "attempts")? != claim.attempt {
    tx.commit().await?;
    return Err(Unavailable);
  }
  if conflict {
    complete_locked_marker(&tx, &claim.inbox_id, now, None).await?;
    tx.commit().await?;
    return Ok(ReceiveOutcome::Discarded);
  }
  if current.is_empty() {
    complete_locked_marker(&tx, &claim.inbox_id, now, None).await?;
    enqueue_text(&tx, Message { contact: claim.contact, appointment: None, key: &format!("receipt-intake:{}:unmatched-after-copy", claim.inbox_id), text: "No encontré una cita con pago pendiente para este comprobante. No conservé el archivo. Si necesita ayuda, escriba 5 para pedir seguimiento.", purpose: "status", revision: None }, origin, now)
    .await?;
    tx.commit().await?;
    return Ok(ReceiveOutcome::Unmatched);
  }
  current.sort_by(|left, right| {
    left
      .starts_at
      .cmp(&right.starts_at)
      .then_with(|| left.id.cmp(&right.id))
  });

  if claim.candidates.len() > 1 {
    let receipt = Uuid::now_v7();
    tx.execute(sql(
      "INSERT INTO receipts(id,contact_id,inbox_message_id,appointment_id,mime,bytes,sent_at,arrived_at,timely,late,created_at) VALUES ($1,$2,$3,NULL,$4,$5,$6,$7,NULL,NULL,$8)",
      vec![receipt.into(), claim.contact.into(), claim.inbox_id.clone().into(), mime.into(), bytes.into(), marker.try_get::<i64>("", "sent_at")?.into(), marker.try_get::<i64>("", "arrived_at")?.into(), now.into()],
    )).await?;
    for appointment in &current {
      tx.execute(sql(
        "INSERT INTO receipt_candidates(receipt_id,appointment_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
        vec![receipt.into(), appointment.id.into()],
      ))
      .await?;
    }
    let candidates = candidate_json(&current);
    set_selection_state(&tx, claim.contact, receipt, candidates.clone(), now).await?;
    complete_locked_marker(&tx, &claim.inbox_id, now, None).await?;
    appointment_store::attention(&tx, None, Some(claim.contact), "receipt_selection", now).await?;
    let text = selection_prompt(&current);
    enqueue_text(
      &tx,
      Message {
        contact: claim.contact,
        appointment: None,
        key: &format!("receipt:{receipt}:selection"),
        text: &text,
        purpose: "status",
        revision: None,
      },
      origin,
      now,
    )
    .await?;
    if let Some(doctor) = doctor {
      notify_doctor(&tx, doctor, None, None, receipt, origin, now).await?;
    }
    tx.commit().await?;
    return Ok(ReceiveOutcome::AwaitingSelection {
      receipt_id: receipt,
      candidate_count: current.len(),
    });
  }

  let appointment = current.remove(0);
  let hold = single_hold.ok_or_else(|| DbErr::Custom("Receipt Hold unavailable".into()))?;
  let sent_at: i64 = marker.try_get("", "sent_at")?;
  let arrived_at: i64 = marker.try_get("", "arrived_at")?;
  let timely = appointment
    .receipt_deadline
    .is_some_and(|deadline| sent_at <= deadline);
  let before_expiry =
    hold.exists && (hold.unbounded || hold.latest_expiry.is_some_and(|expiry| arrived_at < expiry));
  let protects = appointment.status == "awaiting_payment"
    && appointment.payment == "awaiting_receipt"
    && timely
    && before_expiry;

  let late = appointment.status == "expired"
    || hold.expired_at(arrived_at)
    || (!protects && hold.due_at(now));
  let revision = appointment
    .revision
    .checked_add(1)
    .ok_or_else(|| DbErr::Custom("Appointment revision exhausted".into()))?;
  let payment = if protects {
    "under_review"
  } else {
    appointment.payment.as_str()
  };
  tx.execute(sql(
    "UPDATE appointments SET payment=$2,revision=$3,updated_at=$4 WHERE id=$1 AND revision=$5",
    vec![
      appointment.id.into(),
      payment.into(),
      revision.into(),
      now.into(),
      appointment.revision.into(),
    ],
  ))
  .await?;
  if protects {
    tx.execute(sql(
      "UPDATE appointment_holds SET expires_at=NULL WHERE appointment_id=$1 AND active",
      vec![appointment.id.into()],
    ))
    .await?;
  }
  let receipt = Uuid::now_v7();
  tx.execute(sql(
    "INSERT INTO receipts(id,contact_id,inbox_message_id,appointment_id,mime,bytes,sent_at,arrived_at,timely,late,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    vec![receipt.into(), claim.contact.into(), claim.inbox_id.clone().into(), appointment.id.into(), mime.into(), bytes.into(), sent_at.into(), arrived_at.into(), timely.into(), late.into(), now.into()],
  )).await?;
  complete_locked_marker(&tx, &claim.inbox_id, now, None).await?;
  appointment_store::history(
    &tx,
    appointment.id,
    "Contact",
    if late {
      "receipt_late_received"
    } else {
      "receipt_received"
    },
    now,
  )
  .await?;
  appointment_store::attention(
    &tx,
    Some(appointment.id),
    Some(claim.contact),
    if late {
      "late_receipt"
    } else {
      "receipt_review"
    },
    now,
  )
  .await?;
  schedule_pending_payment_jobs(&tx, appointment.id, appointment.starts_at, revision, now).await?;
  let message = if protects {
    "Su pago está en revisión.".to_owned()
  } else if late {
    late_message(timely)
  } else {
    let deadline = appointment
      .receipt_deadline
      .map(|value| appointment_store::local_time(value, &appointment.timezone))
      .unwrap_or_else(|| "la hora indicada".into());
    format!("Recibí el comprobante después de la hora límite ({deadline}). Esto no amplía el horario reservado; el doctor lo revisará mientras siga disponible.")
  };
  enqueue_text(
    &tx,
    Message {
      contact: claim.contact,
      appointment: Some(appointment.id),
      key: &format!("receipt:{receipt}:received"),
      text: &message,
      purpose: "status",
      revision: Some(revision),
    },
    origin,
    now,
  )
  .await?;
  if let Some(doctor) = doctor {
    notify_doctor(
      &tx,
      doctor,
      Some(appointment.id),
      Some(revision),
      receipt,
      origin,
      now,
    )
    .await?;
  }
  tx.commit().await?;
  Ok(ReceiveOutcome::Attached {
    receipt_id: receipt,
    appointment_id: appointment.id,
    timely,
    late,
  })
}

pub async fn select_receipt(
  tx: &DatabaseTransaction,
  contact: Uuid,
  receipt: Uuid,
  appointment_id: Uuid,
  actor: SelectionActor,
  origin: &str,
  now: i64,
) -> Result<SelectOutcome, DbErr> {
  let Some(receipt_snapshot) = tx
    .query_one(sql(
      "SELECT contact_id,appointment_id,sent_at,arrived_at,reviewed_at,deleted_at,bytes IS NOT NULL AS has_bytes FROM receipts WHERE id=$1",
      vec![receipt.into()],
    ))
    .await?
  else {
    return Ok(SelectOutcome::Stale);
  };
  if receipt_snapshot.try_get::<Uuid>("", "contact_id")? != contact
    || receipt_snapshot
      .try_get::<Option<Uuid>>("", "appointment_id")?
      .is_some()
    || receipt_snapshot
      .try_get::<Option<i64>>("", "reviewed_at")?
      .is_some()
    || receipt_snapshot
      .try_get::<Option<i64>>("", "deleted_at")?
      .is_some()
    || !receipt_snapshot.try_get::<bool>("", "has_bytes")?
  {
    return Ok(SelectOutcome::AlreadySelected);
  }
  lock_contact(tx, contact).await?;
  let is_candidate = tx
    .query_one(sql(
      "SELECT 1 AS present FROM receipt_candidates rc JOIN appointments a ON a.id=rc.appointment_id WHERE rc.receipt_id=$1 AND rc.appointment_id=$2 AND a.contact_id=$3",
      vec![receipt.into(), appointment_id.into(), contact.into()],
    ))
    .await?
    .is_some();
  if !is_candidate {
    return stale_choice(tx, contact, receipt, appointment_id, actor, origin, now).await;
  }
  let expiry_check = timers::expire_if_due(tx, appointment_id, origin, now).await?;
  let Some(row) = tx
    .query_one(sql(
      &format!("{} WHERE id=$1 FOR UPDATE", appointment_select()),
      vec![appointment_id.into()],
    ))
    .await?
  else {
    return stale_choice(tx, contact, receipt, appointment_id, actor, origin, now).await;
  };
  let appointment = Appointment::from_query_result(&row, "")?;
  if appointment.contact_id != contact || !eligible_candidate(&appointment) {
    return stale_choice(tx, contact, receipt, appointment_id, actor, origin, now).await;
  }
  let hold = active_hold(tx, appointment_id).await?;
  let Some(receipt_row) = tx
    .query_one(sql(
      "SELECT contact_id,appointment_id,sent_at,arrived_at,reviewed_at,deleted_at,bytes IS NOT NULL AS has_bytes FROM receipts WHERE id=$1 FOR UPDATE",
      vec![receipt.into()],
    ))
    .await?
  else {
    return Ok(SelectOutcome::Stale);
  };
  if receipt_row.try_get::<Uuid>("", "contact_id")? != contact
    || receipt_row
      .try_get::<Option<Uuid>>("", "appointment_id")?
      .is_some()
    || receipt_row
      .try_get::<Option<i64>>("", "reviewed_at")?
      .is_some()
    || receipt_row
      .try_get::<Option<i64>>("", "deleted_at")?
      .is_some()
    || !receipt_row.try_get::<bool>("", "has_bytes")?
    || receipt_row.try_get::<i64>("", "sent_at")?
      != receipt_snapshot.try_get::<i64>("", "sent_at")?
    || receipt_row.try_get::<i64>("", "arrived_at")?
      != receipt_snapshot.try_get::<i64>("", "arrived_at")?
  {
    return Ok(SelectOutcome::AlreadySelected);
  }
  let is_candidate = tx
    .query_one(sql(
      "SELECT 1 AS present FROM receipt_candidates WHERE receipt_id=$1 AND appointment_id=$2",
      vec![receipt.into(), appointment_id.into()],
    ))
    .await?
    .is_some();
  if !is_candidate {
    return stale_choice(tx, contact, receipt, appointment_id, actor, origin, now).await;
  }
  if actor == SelectionActor::Contact {
    let state = tx
      .query_one(sql(
        "SELECT data FROM conversation_state WHERE contact_id=$1 AND stage='receipt_selection' FOR UPDATE",
        vec![contact.into()],
      ))
      .await?;
    if state
      .as_ref()
      .and_then(|row| row.try_get::<serde_json::Value>("", "data").ok())
      .and_then(|data| data["receipt_id"].as_str().map(str::to_owned))
      .as_deref()
      != Some(receipt.to_string().as_str())
    {
      return stale_choice(tx, contact, receipt, appointment_id, actor, origin, now).await;
    }
  }
  let sent_at: i64 = receipt_row.try_get("", "sent_at")?;
  let arrived_at: i64 = receipt_row.try_get("", "arrived_at")?;
  let timely = appointment
    .receipt_deadline
    .is_some_and(|deadline| sent_at <= deadline);
  let before_expiry =
    hold.exists && (hold.unbounded || hold.latest_expiry.is_some_and(|expiry| arrived_at < expiry));
  let current_hold =
    hold.exists && (hold.unbounded || hold.latest_expiry.is_some_and(|expiry| now < expiry));
  let expiry_deferred = matches!(&expiry_check, ExpiryCheck::Deferred);
  let protects = appointment.status == "awaiting_payment"
    && appointment.payment == "awaiting_receipt"
    && timely
    && before_expiry
    && current_hold
    && !expiry_deferred;
  let late = appointment.status == "expired"
    || hold.expired_at(arrived_at)
    || (!expiry_deferred && hold.due_at(now));
  let revision = appointment
    .revision
    .checked_add(1)
    .ok_or_else(|| DbErr::Custom("Appointment revision exhausted".into()))?;
  let payment = if protects {
    "under_review"
  } else {
    appointment.payment.as_str()
  };
  tx.execute(sql(
    "UPDATE appointments SET payment=$2,revision=$3,updated_at=$4 WHERE id=$1 AND revision=$5",
    vec![
      appointment_id.into(),
      payment.into(),
      revision.into(),
      now.into(),
      appointment.revision.into(),
    ],
  ))
  .await?;
  if protects {
    tx.execute(sql(
      "UPDATE appointment_holds SET expires_at=NULL WHERE appointment_id=$1 AND active",
      vec![appointment_id.into()],
    ))
    .await?;
  }
  tx.execute(sql(
    "UPDATE receipts SET appointment_id=$2,timely=$3,late=$4 WHERE id=$1 AND appointment_id IS NULL AND reviewed_at IS NULL AND deleted_at IS NULL",
    vec![receipt.into(), appointment_id.into(), timely.into(), late.into()],
  ))
  .await?;
  tx.execute(sql(
    "DELETE FROM receipt_candidates WHERE receipt_id=$1",
    vec![receipt.into()],
  ))
  .await?;
  resolve_attention(tx, contact, Some(appointment_id), "receipt_selection", now).await?;
  appointment_store::history(
    tx,
    appointment_id,
    match actor {
      SelectionActor::Contact => "Contact",
      SelectionActor::Doctor => "Doctor",
    },
    "receipt_associated",
    now,
  )
  .await?;
  appointment_store::history(
    tx,
    appointment_id,
    "Contact",
    if late {
      "receipt_late_received"
    } else {
      "receipt_received"
    },
    now,
  )
  .await?;
  appointment_store::attention(
    tx,
    Some(appointment_id),
    Some(contact),
    if late {
      "late_receipt"
    } else {
      "receipt_review"
    },
    now,
  )
  .await?;
  schedule_pending_payment_jobs(tx, appointment_id, appointment.starts_at, revision, now).await?;
  let text = if protects {
    "Su pago está en revisión.".to_owned()
  } else if late {
    late_message(timely)
  } else {
    let deadline = appointment
      .receipt_deadline
      .map(|value| appointment_store::local_time(value, &appointment.timezone))
      .unwrap_or_else(|| "la hora indicada".into());
    format!("Recibí el comprobante después de la hora límite ({deadline}). Esto no amplía el horario reservado; el doctor lo revisará mientras siga disponible.")
  };
  enqueue_text(
    tx,
    Message {
      contact,
      appointment: Some(appointment_id),
      key: &format!("receipt:{receipt}:associated"),
      text: &text,
      purpose: "status",
      revision: Some(revision),
    },
    origin,
    now,
  )
  .await?;
  restore_selection_state(tx, contact, receipt, now).await?;
  Ok(SelectOutcome::Attached { timely, late })
}

async fn retry_or_finish(
  db: &Db,
  claim: &Claim,
  origin: &str,
  now: i64,
) -> Result<ReceiveOutcome, Unavailable> {
  let tx = db.conn.begin().await?;
  whatsapp::bound_transaction(&tx).await?;
  lock_contact(&tx, claim.contact).await?;
  lock_appointments(&tx, &claim.candidates).await?;
  let marker = lock_marker(&tx, &claim.inbox_id).await?;
  if marker.try_get::<String>("", "state")? == "complete" {
    tx.commit().await?;
    return Ok(ReceiveOutcome::Duplicate);
  }
  if marker.try_get::<i32>("", "attempts")? != claim.attempt {
    tx.commit().await?;
    return Err(Unavailable);
  }
  if claim.attempt < 5 {
    let delay = RETRY_DELAYS_MS[(claim.attempt as usize - 1).min(RETRY_DELAYS_MS.len() - 1)];
    let lease_until: i64 = marker.try_get("", "available_at")?;
    tx.execute(sql(
      "UPDATE receipt_intake_state SET available_at=$2,last_error_code='media_unavailable',updated_at=$3 WHERE inbox_message_id=$1 AND state='pending'",
      vec![claim.inbox_id.clone().into(), lease_until.saturating_add(delay).into(), now.into()],
    ))
    .await?;
    tx.commit().await?;
    return Err(Unavailable);
  }
  finish_copy_failure(
    &tx,
    claim.contact,
    &claim.inbox_id,
    &claim.candidates,
    origin,
    now,
  )
  .await?;
  tx.commit().await?;
  Ok(ReceiveOutcome::MediaUnavailable)
}

async fn finish_invalid(
  db: &Db,
  claim: &Claim,
  origin: &str,
  now: i64,
) -> Result<ReceiveOutcome, Unavailable> {
  let tx = db.conn.begin().await?;
  whatsapp::bound_transaction(&tx).await?;
  lock_contact(&tx, claim.contact).await?;
  lock_appointments(&tx, &claim.candidates).await?;
  let marker = lock_marker(&tx, &claim.inbox_id).await?;
  if marker.try_get::<String>("", "state")? == "complete" {
    tx.commit().await?;
    return Ok(ReceiveOutcome::Duplicate);
  }
  if marker.try_get::<i32>("", "attempts")? != claim.attempt {
    tx.commit().await?;
    return Err(Unavailable);
  }
  complete_locked_marker(&tx, &claim.inbox_id, now, Some("invalid_media")).await?;
  enqueue_unsupported(&tx, claim.contact, &claim.inbox_id, origin, now).await?;
  tx.commit().await?;
  Ok(ReceiveOutcome::Unsupported)
}

async fn finish_copy_failure(
  tx: &DatabaseTransaction,
  contact: Uuid,
  inbox_id: &str,
  candidates: &[Appointment],
  origin: &str,
  now: i64,
) -> Result<(), DbErr> {
  complete_locked_marker(tx, inbox_id, now, Some("media_unavailable")).await?;
  let single = (candidates.len() == 1).then(|| &candidates[0]);
  appointment_store::attention(
    tx,
    single.map(|appointment| appointment.id),
    Some(contact),
    "receipt_copy_failed",
    now,
  )
  .await?;
  let revision = single.map(|appointment| appointment.revision);
  enqueue_text(tx, Message { contact, appointment: single.map(|appointment| appointment.id), key: &format!("receipt-intake:{inbox_id}:copy-failed"), text: "No pude guardar el comprobante. Puede volver a enviarlo; si necesita ayuda, escriba 5 para pedir seguimiento.", purpose: "status", revision }, origin, now)
  .await?;
  Ok(())
}

async fn enqueue_unsupported(
  tx: &DatabaseTransaction,
  contact: Uuid,
  inbox_id: &str,
  origin: &str,
  now: i64,
) -> Result<(), DbErr> {
  enqueue_text(tx, Message { contact, appointment: None, key: &format!("receipt-intake:{inbox_id}:unsupported"), text: "No pude usar ese archivo. Envíe un JPEG, PNG, WebP o PDF de hasta 10 MiB; si necesita ayuda, escriba 5 para pedir seguimiento.", purpose: "status", revision: None }, origin, now)
  .await
}

async fn expire_candidates(
  tx: &DatabaseTransaction,
  candidates: &[Appointment],
  origin: &str,
  now: i64,
) -> Result<(), DbErr> {
  let mut ids = candidates
    .iter()
    .map(|appointment| appointment.id)
    .collect::<Vec<_>>();
  ids.sort();
  ids.dedup();
  for id in ids {
    match timers::expire_if_due(tx, id, origin, now).await? {
      ExpiryCheck::Expired | ExpiryCheck::Deferred | ExpiryCheck::NotDue => {}
    }
  }
  Ok(())
}

pub(crate) async fn schedule_pending_payment_jobs(
  tx: &DatabaseTransaction,
  appointment_id: Uuid,
  starts_at: i64,
  revision: i64,
  now: i64,
) -> Result<(), DbErr> {
  let current = tx
    .query_one(sql(
      "SELECT status,payment,revision,starts_at FROM appointments WHERE id=$1 FOR UPDATE",
      vec![appointment_id.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Appointment unavailable for payment jobs".into()))?;
  let status: String = current.try_get("", "status")?;
  let payment: String = current.try_get("", "payment")?;
  if current.try_get::<i64>("", "revision")? != revision
    || current.try_get::<i64>("", "starts_at")? != starts_at
  {
    return Err(DbErr::Custom(
      "Appointment changed before payment jobs were scheduled".into(),
    ));
  }
  if status != "awaiting_payment"
    || !matches!(
      payment.as_str(),
      "awaiting_receipt" | "under_review" | "rejected"
    )
  {
    return Ok(());
  }
  let holds = tx
    .query_all(sql(
      "SELECT expires_at FROM appointment_holds WHERE appointment_id=$1 AND active ORDER BY id FOR UPDATE",
      vec![appointment_id.into()],
    ))
    .await?;
  let mut has_active_hold = false;
  let mut has_unbounded_hold = false;
  let mut all_holds_finite = true;
  let mut latest_expiry = None;
  for hold in &holds {
    has_active_hold = true;
    if let Some(expiry) = hold.try_get::<Option<i64>>("", "expires_at")? {
      latest_expiry = Some(latest_expiry.map_or(expiry, |current: i64| current.max(expiry)));
    } else {
      has_unbounded_hold = true;
      all_holds_finite = false;
    }
  }
  if matches!(payment.as_str(), "awaiting_receipt" | "rejected") {
    if let Some(expiry) = latest_expiry {
      appointment_store::job(
        tx,
        appointment_id,
        "expiry",
        expiry,
        &format!("{appointment_id}:{revision}:expiry:deadline"),
      )
      .await?;
    }
  }
  let hold_allows_review = match payment.as_str() {
    "under_review" => has_unbounded_hold,
    "awaiting_receipt" => {
      has_active_hold && all_holds_finite && latest_expiry.is_some_and(|expiry| expiry > now)
    }
    _ => false,
  };
  if !hold_allows_review {
    return Ok(());
  }
  let receipt_query = match payment.as_str() {
    "under_review" => {
      "SELECT EXISTS (SELECT 1 FROM receipts WHERE appointment_id=$1 AND bytes IS NOT NULL AND reviewed_at IS NULL AND deleted_at IS NULL) AS pending"
    }
    "awaiting_receipt" => {
      "SELECT EXISTS (SELECT 1 FROM receipts WHERE appointment_id=$1 AND bytes IS NOT NULL AND reviewed_at IS NULL AND deleted_at IS NULL AND timely=false AND late=false) AS pending"
    }
    _ => return Ok(()),
  };
  let has_unreviewed_receipt = tx
    .query_one(sql(receipt_query, vec![appointment_id.into()]))
    .await?
    .ok_or_else(|| DbErr::Custom("Appointment Receipt evidence unavailable".into()))?
    .try_get::<bool>("", "pending")?;
  if !has_unreviewed_receipt {
    return Ok(());
  }
  let Some(settings) = tx
    .query_one(sql(
      "SELECT review_interval_minutes,urgent_notice_minutes FROM practice WHERE id=1",
      vec![],
    ))
    .await?
  else {
    return Err(DbErr::Custom("Practice review settings unavailable".into()));
  };
  let interval: i32 = settings.try_get("", "review_interval_minutes")?;
  let urgent: i32 = settings.try_get("", "urgent_notice_minutes")?;
  appointment_store::job(
    tx,
    appointment_id,
    "review",
    now.saturating_add(i64::from(interval).saturating_mul(60_000)),
    &format!("{appointment_id}:{revision}:review:periodic"),
  )
  .await?;
  let urgent_at = starts_at.saturating_sub(i64::from(urgent).saturating_mul(60_000));
  if urgent_at > now {
    appointment_store::job(
      tx,
      appointment_id,
      "review",
      urgent_at,
      &format!("{appointment_id}:{revision}:review:urgent"),
    )
    .await?;
  }
  Ok(())
}

pub(crate) async fn active_hold(
  tx: &DatabaseTransaction,
  appointment: Uuid,
) -> Result<HoldState, DbErr> {
  let rows = tx
    .query_all(sql(
      "SELECT expires_at FROM appointment_holds WHERE appointment_id=$1 AND active ORDER BY id FOR UPDATE",
      vec![appointment.into()],
    ))
    .await?;
  let mut latest_expiry = None;
  let mut unbounded = false;
  for row in &rows {
    let expiry: Option<i64> = row.try_get("", "expires_at")?;
    if let Some(expiry) = expiry {
      latest_expiry = Some(latest_expiry.map_or(expiry, |current: i64| current.max(expiry)));
    } else {
      unbounded = true;
    }
  }
  Ok(HoldState {
    exists: !rows.is_empty(),
    unbounded,
    latest_expiry,
  })
}

async fn lock_contact(tx: &DatabaseTransaction, contact: Uuid) -> Result<Option<Uuid>, DbErr> {
  tx.query_one(sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![contact.into()],
  ))
  .await?
  .map(|row| row.try_get("", "id"))
  .transpose()
}

async fn doctor_contact(tx: &DatabaseTransaction) -> Result<Option<Uuid>, DbErr> {
  tx.query_one(sql(
    "SELECT contact_id FROM doctor_number_state WHERE id=1 AND verified_phone IS NOT NULL",
    vec![],
  ))
  .await?
  .map(|row| row.try_get("", "contact_id"))
  .transpose()
}

async fn candidate_appointments(
  tx: &DatabaseTransaction,
  contact: Uuid,
) -> Result<Vec<Appointment>, DbErr> {
  let rows = tx
    .query_all(sql(
      &format!(
        "{} WHERE contact_id=$1 AND format='video' AND ((status='awaiting_payment' AND payment='awaiting_receipt') OR (status='expired' AND payment IN ('awaiting_receipt','under_review','rejected'))) ORDER BY id FOR UPDATE",
        appointment_select()
      ),
      vec![contact.into()],
    ))
    .await?;
  rows
    .iter()
    .map(|row| Appointment::from_query_result(row, ""))
    .collect()
}

fn appointment_select() -> &'static str {
  "SELECT id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,booking_calendar,google_generation,google_selection_version,event_id,event_etag,meet_url,revision,attendance_confirmed_at,cancelled_at,expired_at,created_at,updated_at FROM appointments"
}

fn eligible_candidate(appointment: &Appointment) -> bool {
  appointment.format == "video"
    && ((appointment.status == "awaiting_payment" && appointment.payment == "awaiting_receipt")
      || (appointment.status == "expired"
        && matches!(
          appointment.payment.as_str(),
          "awaiting_receipt" | "under_review" | "rejected"
        )))
}

async fn lock_appointments(
  tx: &DatabaseTransaction,
  appointments: &[Appointment],
) -> Result<(), DbErr> {
  let mut ids = appointments
    .iter()
    .map(|appointment| appointment.id)
    .collect::<Vec<_>>();
  ids.sort();
  ids.dedup();
  for id in ids {
    tx.query_one(sql(
      "SELECT id FROM appointments WHERE id=$1 FOR UPDATE",
      vec![id.into()],
    ))
    .await?;
  }
  Ok(())
}

async fn lock_marker(
  tx: &DatabaseTransaction,
  inbox_id: &str,
) -> Result<sea_orm::QueryResult, DbErr> {
  tx.query_one(sql(
    "SELECT state,attempts,available_at,sent_at,arrived_at FROM receipt_intake_state WHERE inbox_message_id=$1 FOR UPDATE",
    vec![inbox_id.into()],
  ))
  .await?
  .ok_or_else(|| DbErr::Custom("Receipt intake marker unavailable".into()))
}

async fn complete_marker(
  tx: &DatabaseTransaction,
  inbox_id: &str,
  now: i64,
  error: Option<&str>,
) -> Result<(), DbErr> {
  let marker = lock_marker(tx, inbox_id).await?;
  if marker.try_get::<String>("", "state")? == "pending" {
    complete_locked_marker(tx, inbox_id, now, error).await?;
  }
  Ok(())
}

async fn complete_locked_marker(
  tx: &DatabaseTransaction,
  inbox_id: &str,
  now: i64,
  error: Option<&str>,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE receipt_intake_state SET state='complete',available_at=$2,last_error_code=$3,updated_at=$2 WHERE inbox_message_id=$1 AND state='pending'",
    vec![inbox_id.into(), now.into(), error.map(str::to_owned).into()],
  ))
  .await?;
  Ok(())
}

async fn notify_doctor(
  tx: &DatabaseTransaction,
  doctor: Uuid,
  appointment: Option<Uuid>,
  revision: Option<i64>,
  receipt: Uuid,
  origin: &str,
  now: i64,
) -> Result<(), DbErr> {
  let base = origin.trim_end_matches('/');
  let text =
    format!("Llegó un comprobante para revisar: {base}/dashboard/payments/receipts/{receipt}");
  enqueue_text(
    tx,
    Message {
      contact: doctor,
      appointment,
      key: &format!("receipt:{receipt}:doctor-arrival"),
      text: &text,
      purpose: "doctor_notification",
      revision,
    },
    origin,
    now,
  )
  .await
}

fn late_message(timely: bool) -> String {
  if timely {
    "Recibí el comprobante; la cita ya venció y el horario se liberó. WhatsApp indica que fue enviado a tiempo. El doctor lo revisará; no se restablece ni se reserva automáticamente.".into()
  } else {
    "Recibí el comprobante; la cita ya venció y el horario se liberó. El doctor lo revisará; no se restablece ni se reserva automáticamente.".into()
  }
}

fn selection_prompt(appointments: &[Appointment]) -> String {
  let mut text =
    String::from("Recibí su comprobante. ¿A cuál cita corresponde? Responda con un número:\n");
  for (index, appointment) in appointments.iter().enumerate() {
    let local = appointment_store::local_time(appointment.starts_at, &appointment.timezone);
    let _ = std::fmt::Write::write_fmt(
      &mut text,
      format_args!(
        "{}. {} — {}, {} ({})\n",
        index + 1,
        appointment.patient_name,
        appointment.service_name,
        local,
        appointment.timezone
      ),
    );
  }
  text.push_str("Si necesita ayuda, escriba 5 para pedir seguimiento.");
  text
}

fn candidate_json(appointments: &[Appointment]) -> serde_json::Value {
  serde_json::Value::Array(
    appointments
      .iter()
      .map(|appointment| {
        serde_json::json!({
          "appointment_id": appointment.id,
          "patient_name": appointment.patient_name,
          "service_name": appointment.service_name,
          "starts_at": appointment.starts_at,
          "timezone": appointment.timezone,
        })
      })
      .collect(),
  )
}

async fn set_selection_state(
  tx: &DatabaseTransaction,
  contact: Uuid,
  receipt: Uuid,
  candidates: serde_json::Value,
  now: i64,
) -> Result<(), DbErr> {
  let previous = tx
    .query_one(sql(
      "SELECT stage,data FROM conversation_state WHERE contact_id=$1 FOR UPDATE",
      vec![contact.into()],
    ))
    .await?;
  let previous = match previous {
    Some(row) => {
      let stage: String = row.try_get("", "stage")?;
      let data: serde_json::Value = row.try_get("", "data")?;
      (stage != "idle").then_some(serde_json::json!({"stage": stage, "data": data}))
    }
    None => None,
  };
  let data = serde_json::json!({
    "receipt_id": receipt,
    "candidates": candidates,
    "previous": previous,
  });
  tx.execute(sql(
    "INSERT INTO conversation_state(contact_id,stage,data,updated_at) VALUES ($1,'receipt_selection',$2,$3) ON CONFLICT (contact_id) DO UPDATE SET stage='receipt_selection',data=EXCLUDED.data,updated_at=EXCLUDED.updated_at",
    vec![contact.into(), data.into(), now.into()],
  ))
  .await?;
  Ok(())
}

async fn restore_selection_state(
  tx: &DatabaseTransaction,
  contact: Uuid,
  receipt: Uuid,
  now: i64,
) -> Result<(), DbErr> {
  let row = tx
    .query_one(sql(
      "SELECT data FROM conversation_state WHERE contact_id=$1 AND stage='receipt_selection' FOR UPDATE",
      vec![contact.into()],
    ))
    .await?;
  let Some(row) = row else {
    return Ok(());
  };
  let data: serde_json::Value = row.try_get("", "data")?;
  if data["receipt_id"].as_str() != Some(receipt.to_string().as_str()) {
    return Ok(());
  }
  if let Some(previous) = data.get("previous").filter(|value| !value.is_null()) {
    let stage = previous["stage"]
      .as_str()
      .filter(|stage| !stage.is_empty() && *stage != "receipt_selection")
      .unwrap_or("idle");
    let state = previous
      .get("data")
      .cloned()
      .filter(serde_json::Value::is_object)
      .unwrap_or_else(|| serde_json::json!({}));
    tx.execute(sql(
      "UPDATE conversation_state SET stage=$2,data=$3,updated_at=$4 WHERE contact_id=$1",
      vec![contact.into(), stage.into(), state.into(), now.into()],
    ))
    .await?;
  } else {
    tx.execute(sql(
      "UPDATE conversation_state SET stage='idle',data='{}'::jsonb,updated_at=$2 WHERE contact_id=$1",
      vec![contact.into(), now.into()],
    ))
    .await?;
  }
  Ok(())
}

async fn stale_choice(
  tx: &DatabaseTransaction,
  contact: Uuid,
  receipt: Uuid,
  appointment: Uuid,
  actor: SelectionActor,
  origin: &str,
  now: i64,
) -> Result<SelectOutcome, DbErr> {
  if actor == SelectionActor::Contact {
    enqueue_text(tx, Message { contact, appointment: None, key: &format!("receipt:{receipt}:stale-choice:{appointment}"), text: "Esa cita ya no admite el comprobante. Elija otra opción de la lista o escriba 5 para pedir seguimiento.", purpose: "status", revision: None }, origin, now)
    .await?;
  }
  Ok(SelectOutcome::Stale)
}

async fn resolve_attention(
  tx: &DatabaseTransaction,
  contact: Uuid,
  appointment: Option<Uuid>,
  kind: &str,
  now: i64,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE attention_items SET resolved_at=$4 WHERE contact_id=$1 AND appointment_id IS NOT DISTINCT FROM $2 AND kind=$3 AND resolved_at IS NULL",
    vec![contact.into(), appointment.into(), kind.into(), now.into()],
  ))
  .await?;
  Ok(())
}
