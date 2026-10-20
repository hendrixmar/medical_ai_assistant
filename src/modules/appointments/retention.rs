use std::collections::BTreeSet;

use crate::{
  database::Db,
  modules::{doctor::Clock, whatsapp::inbox::sql},
};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};
use uuid::Uuid;

use super::store;

const NINETY_DAYS_MS: i64 = 90 * 24 * 60 * 60 * 1000;
const BATCH_SIZE: i64 = 100;
const RETENTION_ATTENTION: &str = "retention_policy_review";
const REDACTED_ENVELOPE: &[u8] = b"{\"retention_redacted\":true}";
const REDACTED_TEXT: &str = "[message text removed by retention policy]";

pub async fn run_once(db: &Db, clock: &dyn Clock) -> Result<bool, DbErr> {
  let now = clock.now_millis();
  let cutoff = now.saturating_sub(NINETY_DAYS_MS);
  let tx = db.conn.begin().await?;

  // Serialize this durable pass across application processes; a later pass resumes any remainder.
  let locked = tx
    .query_one(sql(
      "SELECT pg_try_advisory_xact_lock(1297239380,1) AS acquired",
      vec![],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Retention lock result unavailable".into()))?
    .try_get::<bool>("", "acquired")?;
  if !locked {
    tx.rollback().await?;
    return Ok(false);
  }

  let mut changed = prune_receipts(&tx, cutoff, now).await?;
  changed |= prune_inbox_text(&tx, cutoff, now).await?;
  changed |= prune_outbox_text(&tx, cutoff, now).await?;

  let attention_contacts = policy_attention_contacts(&tx, cutoff).await?;
  for contact in attention_contacts.iter().copied() {
    changed |= ensure_policy_attention(&tx, contact, now).await?;
  }
  changed |= resolve_stale_policy_attention(&tx, &attention_contacts, now).await?;

  tx.commit().await?;
  Ok(changed)
}

#[derive(Default, PartialEq, Eq)]
struct MessageGraph {
  contacts: BTreeSet<Uuid>,
  appointments: BTreeSet<Uuid>,
}

async fn add_appointment(
  tx: &sea_orm::DatabaseTransaction,
  graph: &mut MessageGraph,
  appointment: Uuid,
) -> Result<(), DbErr> {
  graph.appointments.insert(appointment);
  if let Some(row) = tx
    .query_one(sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![appointment.into()],
    ))
    .await?
  {
    graph
      .contacts
      .insert(row.try_get::<Uuid>("", "contact_id")?);
  }
  Ok(())
}

async fn message_graph(
  tx: &sea_orm::DatabaseTransaction,
  message_id: &str,
) -> Result<MessageGraph, DbErr> {
  let mut graph = MessageGraph::default();
  for row in tx
    .query_all(sql(
      "SELECT contact_id FROM inbox_messages WHERE message_id=$1 UNION SELECT contact_id FROM receipts WHERE inbox_message_id=$1",
      vec![message_id.into()],
    ))
    .await?
  {
    if let Some(contact) = row.try_get::<Option<Uuid>>("", "contact_id")? {
      graph.contacts.insert(contact);
    }
  }
  for row in tx
    .query_all(sql(
      "SELECT appointment_id FROM message_appointments WHERE inbox_message_id=$1 UNION SELECT appointment_id FROM receipts WHERE inbox_message_id=$1 AND appointment_id IS NOT NULL",
      vec![message_id.into()],
    ))
    .await?
  {
    add_appointment(
      tx,
      &mut graph,
      row.try_get::<Uuid>("", "appointment_id")?,
    )
    .await?;
  }
  Ok(graph)
}

async fn lock_graph(
  tx: &sea_orm::DatabaseTransaction,
  graph: &MessageGraph,
) -> Result<bool, DbErr> {
  for contact in &graph.contacts {
    if tx
      .query_one(sql(
        "SELECT id FROM contacts WHERE id=$1 FOR UPDATE SKIP LOCKED",
        vec![(*contact).into()],
      ))
      .await?
      .is_none()
    {
      return Ok(false);
    }
  }
  for appointment in &graph.appointments {
    if tx
      .query_one(sql(
        "SELECT id FROM appointments WHERE id=$1 FOR UPDATE SKIP LOCKED",
        vec![(*appointment).into()],
      ))
      .await?
      .is_none()
    {
      return Ok(false);
    }
  }
  Ok(true)
}

async fn lock_inbox_row(
  tx: &sea_orm::DatabaseTransaction,
  message_id: &str,
) -> Result<bool, DbErr> {
  Ok(
    tx.query_one(sql(
      "SELECT message_id FROM inbox_messages WHERE message_id=$1 FOR UPDATE SKIP LOCKED",
      vec![message_id.into()],
    ))
    .await?
    .is_some(),
  )
}

async fn lock_message_side_rows(
  tx: &sea_orm::DatabaseTransaction,
  message_id: &str,
) -> Result<bool, DbErr> {
  let has_intake = tx
    .query_one(sql(
      "SELECT 1 FROM receipt_intake_state WHERE inbox_message_id=$1",
      vec![message_id.into()],
    ))
    .await?
    .is_some();
  if has_intake
    && tx
      .query_one(sql(
        "SELECT inbox_message_id FROM receipt_intake_state WHERE inbox_message_id=$1 FOR UPDATE SKIP LOCKED",
        vec![message_id.into()],
      ))
      .await?
      .is_none()
  {
    return Ok(false);
  }
  let rows = tx
    .query_all(sql(
      "SELECT id FROM receipts WHERE inbox_message_id=$1 ORDER BY id",
      vec![message_id.into()],
    ))
    .await?;
  for row in rows {
    let id: Uuid = row.try_get("", "id")?;
    if tx
      .query_one(sql(
        "SELECT id FROM receipts WHERE id=$1 FOR UPDATE SKIP LOCKED",
        vec![id.into()],
      ))
      .await?
      .is_none()
    {
      return Ok(false);
    }
  }
  Ok(true)
}

async fn lock_message_for_retention(
  tx: &sea_orm::DatabaseTransaction,
  message_id: &str,
) -> Result<bool, DbErr> {
  let graph = message_graph(tx, message_id).await?;
  if graph.appointments.is_empty() || !lock_graph(tx, &graph).await? {
    return Ok(false);
  }
  if graph != message_graph(tx, message_id).await? {
    return Ok(false);
  }
  if !lock_inbox_row(tx, message_id).await? || !lock_message_side_rows(tx, message_id).await? {
    return Ok(false);
  }
  Ok(graph == message_graph(tx, message_id).await?)
}

#[derive(PartialEq, Eq)]
struct OutboxGraph {
  contact: Uuid,
  purpose: String,
  message_ids: BTreeSet<String>,
  links: MessageGraph,
}

async fn outbox_graph(
  tx: &sea_orm::DatabaseTransaction,
  outbox_id: Uuid,
) -> Result<Option<OutboxGraph>, DbErr> {
  let Some(row) = tx
    .query_one(sql(
      "SELECT contact_id,purpose,appointment_id FROM outbox_messages WHERE id=$1",
      vec![outbox_id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  let contact: Uuid = row.try_get("", "contact_id")?;
  let purpose: String = row.try_get("", "purpose")?;
  let appointment: Option<Uuid> = row.try_get("", "appointment_id")?;
  let mut links = MessageGraph::default();
  links.contacts.insert(contact);
  if let Some(appointment) = appointment {
    add_appointment(tx, &mut links, appointment).await?;
  }
  let mut message_ids = BTreeSet::new();
  for row in tx
    .query_all(sql(
      r#"SELECT message_id FROM (
           SELECT o.wrote_first_inbox_id AS message_id FROM outbox_messages o WHERE o.id=$1
           UNION
           SELECT o.reply_inbox_id FROM outbox_messages o
           WHERE o.id=$1 AND o.reply_inbox_id IS NOT NULL
           UNION
           SELECT f.inbox_message_id FROM outbox_messages o
           JOIN followup_requests f ON f.id=o.resolved_followup_id WHERE o.id=$1
         ) sources ORDER BY message_id"#,
      vec![outbox_id.into()],
    ))
    .await?
  {
    message_ids.insert(row.try_get::<String>("", "message_id")?);
  }
  for message_id in &message_ids {
    let graph = message_graph(tx, message_id).await?;
    links.contacts.extend(graph.contacts);
    links.appointments.extend(graph.appointments);
  }
  Ok(Some(OutboxGraph {
    contact,
    purpose,
    message_ids,
    links,
  }))
}

async fn lock_outbox_for_retention(
  tx: &sea_orm::DatabaseTransaction,
  outbox_id: Uuid,
) -> Result<bool, DbErr> {
  let Some(graph) = outbox_graph(tx, outbox_id).await? else {
    return Ok(false);
  };
  if graph.links.appointments.is_empty() || !lock_graph(tx, &graph.links).await? {
    return Ok(false);
  }
  if outbox_graph(tx, outbox_id).await?.as_ref() != Some(&graph) {
    return Ok(false);
  }
  for message_id in &graph.message_ids {
    if !lock_inbox_row(tx, message_id).await? {
      return Ok(false);
    }
  }
  for message_id in &graph.message_ids {
    if !lock_message_side_rows(tx, message_id).await? {
      return Ok(false);
    }
  }
  if tx
    .query_one(sql(
      "SELECT id FROM outbox_messages WHERE id=$1 FOR UPDATE SKIP LOCKED",
      vec![outbox_id.into()],
    ))
    .await?
    .is_none()
  {
    return Ok(false);
  }
  Ok(outbox_graph(tx, outbox_id).await?.as_ref() == Some(&graph))
}

async fn prune_receipts(
  tx: &sea_orm::DatabaseTransaction,
  cutoff: i64,
  now: i64,
) -> Result<bool, DbErr> {
  let candidates = tx
    .query_all(sql(
      r#"SELECT r.id,r.inbox_message_id
         FROM receipts r
         JOIN inbox_messages i ON i.message_id=r.inbox_message_id
         WHERE r.bytes IS NOT NULL AND r.deleted_at IS NULL AND r.appointment_id IS NOT NULL
           AND NOT EXISTS (
             SELECT 1 FROM receipt_intake_state s
             WHERE s.inbox_message_id=i.message_id AND s.state='pending'
           )
           AND NOT EXISTS (
             SELECT 1
             FROM (
               SELECT r.appointment_id
               UNION
               SELECT ma.appointment_id FROM message_appointments ma
               WHERE ma.inbox_message_id=r.inbox_message_id
             ) links
             JOIN appointments a ON a.id=links.appointment_id
             WHERE a.contact_id IS DISTINCT FROM r.contact_id
                OR i.contact_id IS DISTINCT FROM r.contact_id
                OR NOT CASE
                  WHEN a.status='cancelled' THEN COALESCE(a.cancelled_at <= $1,FALSE)
                  WHEN a.status='expired' THEN COALESCE(a.expired_at <= $1,FALSE)
                  ELSE a.ends_at <= $1
                END
           )
         ORDER BY r.arrived_at,r.id
LIMIT $2"#,
      vec![cutoff.into(), BATCH_SIZE.into()],
    ))
    .await?;

  let mut changed = false;
  for row in candidates {
    let id: Uuid = row.try_get("", "id")?;
    let inbox_id: String = row.try_get("", "inbox_message_id")?;
    if !lock_message_for_retention(tx, &inbox_id).await? {
      continue;
    }
    let result = tx
      .execute(sql(
        r#"UPDATE receipts r
           SET bytes=NULL,deleted_at=$3
           WHERE r.id=$1 AND r.inbox_message_id=$2
             AND r.bytes IS NOT NULL AND r.deleted_at IS NULL AND r.appointment_id IS NOT NULL
             AND NOT EXISTS (
               SELECT 1 FROM receipt_intake_state s
               WHERE s.inbox_message_id=r.inbox_message_id AND s.state='pending'
             )
             AND NOT EXISTS (
               SELECT 1
               FROM (
                 SELECT r.appointment_id
                 UNION
                 SELECT ma.appointment_id FROM message_appointments ma
                 WHERE ma.inbox_message_id=r.inbox_message_id
               ) links
               JOIN appointments a ON a.id=links.appointment_id
               JOIN inbox_messages i ON i.message_id=r.inbox_message_id
               WHERE a.contact_id IS DISTINCT FROM r.contact_id
                  OR i.contact_id IS DISTINCT FROM r.contact_id
                  OR NOT CASE
                    WHEN a.status='cancelled' THEN COALESCE(a.cancelled_at <= $4,FALSE)
                    WHEN a.status='expired' THEN COALESCE(a.expired_at <= $4,FALSE)
                    ELSE a.ends_at <= $4
                  END
             )"#,
        vec![id.into(), inbox_id.into(), now.into(), cutoff.into()],
      ))
      .await?;
    changed |= result.rows_affected() == 1;
  }
  Ok(changed)
}

async fn prune_inbox_text(
  tx: &sea_orm::DatabaseTransaction,
  cutoff: i64,
  now: i64,
) -> Result<bool, DbErr> {
  let candidates = tx
    .query_all(sql(
      r#"SELECT i.message_id
         FROM inbox_messages i
         WHERE i.text_deleted_at IS NULL AND i.processed_at IS NOT NULL
           AND NOT EXISTS (
             SELECT 1 FROM processing_jobs j
             WHERE j.inbox_message_id=i.message_id AND j.status='pending'
           )
           AND NOT EXISTS (
             SELECT 1 FROM receipt_intake_state s
             WHERE s.inbox_message_id=i.message_id AND s.state='pending'
           )
           AND NOT EXISTS (
             SELECT 1 FROM receipts r
             WHERE r.inbox_message_id=i.message_id
               AND r.bytes IS NOT NULL AND r.appointment_id IS NULL
           )
           AND EXISTS (
             SELECT 1 FROM (
               SELECT ma.appointment_id FROM message_appointments ma
               WHERE ma.inbox_message_id=i.message_id
               UNION
               SELECT r.appointment_id FROM receipts r
               WHERE r.inbox_message_id=i.message_id AND r.appointment_id IS NOT NULL
             ) links
           )
           AND NOT EXISTS (
             SELECT 1 FROM (
               SELECT ma.appointment_id FROM message_appointments ma
               WHERE ma.inbox_message_id=i.message_id
               UNION
               SELECT r.appointment_id FROM receipts r
               WHERE r.inbox_message_id=i.message_id AND r.appointment_id IS NOT NULL
             ) links
             JOIN appointments a ON a.id=links.appointment_id
             WHERE a.contact_id IS DISTINCT FROM i.contact_id
                OR NOT CASE
                  WHEN a.status='cancelled' THEN COALESCE(a.cancelled_at <= $1,FALSE)
                  WHEN a.status='expired' THEN COALESCE(a.expired_at <= $1,FALSE)
                  ELSE a.ends_at <= $1
                END
           )
         ORDER BY i.arrived_at,i.message_id
LIMIT $2"#,
      vec![cutoff.into(), BATCH_SIZE.into()],
    ))
    .await?;

  let mut changed = false;
  for row in candidates {
    let message_id: String = row.try_get("", "message_id")?;
    if !lock_message_for_retention(tx, &message_id).await? {
      continue;
    }
    let result = tx
      .execute(sql(
        r#"UPDATE inbox_messages i
           SET raw_envelope=$2,body=NULL,media_metadata=NULL,text_deleted_at=$3
           WHERE i.message_id=$1 AND i.text_deleted_at IS NULL AND i.processed_at IS NOT NULL
             AND NOT EXISTS (
               SELECT 1 FROM processing_jobs j
               WHERE j.inbox_message_id=i.message_id AND j.status='pending'
             )
             AND NOT EXISTS (
               SELECT 1 FROM receipt_intake_state s
               WHERE s.inbox_message_id=i.message_id AND s.state='pending'
             )
             AND NOT EXISTS (
               SELECT 1 FROM receipts r
               WHERE r.inbox_message_id=i.message_id
                 AND r.bytes IS NOT NULL AND r.appointment_id IS NULL
             )
             AND EXISTS (
               SELECT 1 FROM (
                 SELECT ma.appointment_id FROM message_appointments ma
                 WHERE ma.inbox_message_id=i.message_id
                 UNION
                 SELECT r.appointment_id FROM receipts r
                 WHERE r.inbox_message_id=i.message_id AND r.appointment_id IS NOT NULL
               ) links
             )
             AND NOT EXISTS (
               SELECT 1 FROM (
                 SELECT ma.appointment_id FROM message_appointments ma
                 WHERE ma.inbox_message_id=i.message_id
                 UNION
                 SELECT r.appointment_id FROM receipts r
                 WHERE r.inbox_message_id=i.message_id AND r.appointment_id IS NOT NULL
               ) links
               JOIN appointments a ON a.id=links.appointment_id
               WHERE a.contact_id IS DISTINCT FROM i.contact_id
                  OR NOT CASE
                    WHEN a.status='cancelled' THEN COALESCE(a.cancelled_at <= $4,FALSE)
                    WHEN a.status='expired' THEN COALESCE(a.expired_at <= $4,FALSE)
                    ELSE a.ends_at <= $4
                  END
             )"#,
        vec![
          message_id.into(),
          REDACTED_ENVELOPE.to_vec().into(),
          now.into(),
          cutoff.into(),
        ],
      ))
      .await?;
    changed |= result.rows_affected() == 1;
  }
  Ok(changed)
}

async fn prune_outbox_text(
  tx: &sea_orm::DatabaseTransaction,
  cutoff: i64,
  now: i64,
) -> Result<bool, DbErr> {
  let candidates = tx
    .query_all(sql(
      r#"WITH sources AS (
           SELECT o.id AS outbox_id,o.wrote_first_inbox_id AS message_id
           FROM outbox_messages o
           UNION
           SELECT o.id,o.reply_inbox_id FROM outbox_messages o
           WHERE o.reply_inbox_id IS NOT NULL
           UNION
           SELECT o.id,f.inbox_message_id FROM outbox_messages o
           JOIN followup_requests f ON f.id=o.resolved_followup_id
         ), associations AS (
           SELECT o.id AS outbox_id,o.contact_id,o.appointment_id
           FROM outbox_messages o WHERE o.appointment_id IS NOT NULL
           UNION
           SELECT s.outbox_id,o.contact_id,ma.appointment_id
           FROM sources s JOIN outbox_messages o ON o.id=s.outbox_id
           JOIN message_appointments ma ON ma.inbox_message_id=s.message_id
           UNION
           SELECT s.outbox_id,o.contact_id,r.appointment_id
           FROM sources s JOIN outbox_messages o ON o.id=s.outbox_id
           JOIN receipts r ON r.inbox_message_id=s.message_id
           WHERE r.appointment_id IS NOT NULL
         )
         SELECT o.id
         FROM outbox_messages o
         WHERE o.text_deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM associations a WHERE a.outbox_id=o.id)
           AND NOT EXISTS (
             SELECT 1 FROM associations link
             JOIN appointments a ON a.id=link.appointment_id
             WHERE link.outbox_id=o.id
               AND ((o.purpose<>'doctor_notification' AND a.contact_id IS DISTINCT FROM o.contact_id) OR NOT CASE
                 WHEN a.status='cancelled' THEN COALESCE(a.cancelled_at <= $1,FALSE)
                 WHEN a.status='expired' THEN COALESCE(a.expired_at <= $1,FALSE)
                 ELSE a.ends_at <= $1
               END)
           )
           AND NOT EXISTS (
             SELECT 1 FROM sources s JOIN inbox_messages i ON i.message_id=s.message_id
             WHERE s.outbox_id=o.id AND o.purpose<>'doctor_notification'
               AND i.contact_id IS DISTINCT FROM o.contact_id
           )
           AND NOT EXISTS (
             SELECT 1 FROM sources s JOIN receipts r ON r.inbox_message_id=s.message_id
             WHERE s.outbox_id=o.id AND r.bytes IS NOT NULL AND r.appointment_id IS NULL
           )
           AND NOT EXISTS (
             SELECT 1 FROM sources s JOIN receipt_intake_state state
               ON state.inbox_message_id=s.message_id
             WHERE s.outbox_id=o.id AND state.state='pending'
           )
         ORDER BY o.created_at,o.id
         LIMIT $2"#,
      vec![cutoff.into(), BATCH_SIZE.into()],
    ))
    .await?;

  let mut changed = false;
  for row in candidates {
    let id: Uuid = row.try_get("", "id")?;
    if !lock_outbox_for_retention(tx, id).await? {
      continue;
    }
    let state = outbox_association_state(tx, id, cutoff).await?;
    if !state.ready() {
      continue;
    }
    let result = tx
      .execute(sql(
        r#"WITH sources AS (
             SELECT o.wrote_first_inbox_id AS message_id FROM outbox_messages o WHERE o.id=$1
             UNION
             SELECT o.reply_inbox_id FROM outbox_messages o
             WHERE o.id=$1 AND o.reply_inbox_id IS NOT NULL
             UNION
             SELECT f.inbox_message_id FROM outbox_messages o
             JOIN followup_requests f ON f.id=o.resolved_followup_id WHERE o.id=$1
           ), associations AS (
             SELECT o.appointment_id FROM outbox_messages o
             WHERE o.id=$1 AND o.appointment_id IS NOT NULL
             UNION
             SELECT ma.appointment_id FROM sources s
             JOIN message_appointments ma ON ma.inbox_message_id=s.message_id
             UNION
             SELECT r.appointment_id FROM sources s
             JOIN receipts r ON r.inbox_message_id=s.message_id
             WHERE r.appointment_id IS NOT NULL
           )
           UPDATE outbox_messages o
           SET rendered_text=$2,text_deleted_at=$3
           WHERE o.id=$1 AND o.text_deleted_at IS NULL
             AND EXISTS (SELECT 1 FROM associations)
            AND NOT EXISTS (
              SELECT 1 FROM sources s JOIN inbox_messages i ON i.message_id=s.message_id
              WHERE o.purpose<>'doctor_notification' AND i.contact_id IS DISTINCT FROM o.contact_id
            )
            AND NOT EXISTS (
              SELECT 1 FROM sources s JOIN receipts r ON r.inbox_message_id=s.message_id
              WHERE r.bytes IS NOT NULL AND r.appointment_id IS NULL
            )
            AND NOT EXISTS (
              SELECT 1 FROM sources s JOIN receipt_intake_state state
                ON state.inbox_message_id=s.message_id
              WHERE state.state='pending'
            )
            AND NOT EXISTS (
              SELECT 1 FROM associations links
              JOIN appointments a ON a.id=links.appointment_id
              WHERE (o.purpose<>'doctor_notification' AND a.contact_id IS DISTINCT FROM o.contact_id)
                 OR NOT CASE
                   WHEN a.status='cancelled' THEN COALESCE(a.cancelled_at <= $4,FALSE)
                   WHEN a.status='expired' THEN COALESCE(a.expired_at <= $4,FALSE)
                   ELSE a.ends_at <= $4
                 END
            )"#,
        vec![id.into(), REDACTED_TEXT.into(), now.into(), cutoff.into()],
      ))
      .await?;
    changed |= result.rows_affected() == 1;
  }
  Ok(changed)
}

struct AssociationState {
  count: i64,
  all_eligible: bool,
  invalid: bool,
  pending_receipt: bool,
  pending_intake: bool,
}
impl AssociationState {
  fn ready(&self) -> bool {
    self.count > 0
      && self.all_eligible
      && !self.invalid
      && !self.pending_receipt
      && !self.pending_intake
  }
}

async fn outbox_association_state(
  tx: &sea_orm::DatabaseTransaction,
  outbox_id: Uuid,
  cutoff: i64,
) -> Result<AssociationState, DbErr> {
  let row = tx
    .query_one(sql(
      r#"WITH sources AS (
           SELECT o.wrote_first_inbox_id AS message_id FROM outbox_messages o WHERE o.id=$1
           UNION
           SELECT o.reply_inbox_id FROM outbox_messages o
           WHERE o.id=$1 AND o.reply_inbox_id IS NOT NULL
           UNION
           SELECT f.inbox_message_id FROM outbox_messages o
           JOIN followup_requests f ON f.id=o.resolved_followup_id WHERE o.id=$1
         ), associations AS (
           SELECT o.appointment_id FROM outbox_messages o
           WHERE o.id=$1 AND o.appointment_id IS NOT NULL
           UNION
           SELECT ma.appointment_id FROM sources s
           JOIN message_appointments ma ON ma.inbox_message_id=s.message_id
           UNION
           SELECT r.appointment_id FROM sources s
           JOIN receipts r ON r.inbox_message_id=s.message_id
           WHERE r.appointment_id IS NOT NULL
         ), flags AS (
           SELECT COUNT(*) AS association_count,
             COALESCE(bool_and(
               (o.purpose='doctor_notification' OR a.contact_id=o.contact_id) AND CASE
                 WHEN a.status='cancelled' THEN COALESCE(a.cancelled_at <= $2,FALSE)
                 WHEN a.status='expired' THEN COALESCE(a.expired_at <= $2,FALSE)
                 ELSE a.ends_at <= $2
               END
             ),FALSE) AS all_eligible,
             COALESCE(bool_or(
               (o.purpose<>'doctor_notification' AND a.contact_id IS DISTINCT FROM o.contact_id)
               OR (a.status='cancelled' AND a.cancelled_at IS NULL)
               OR (a.status='expired' AND a.expired_at IS NULL)
             ),FALSE) AS invalid
           FROM associations links
           JOIN appointments a ON a.id=links.appointment_id
           CROSS JOIN outbox_messages o
           WHERE o.id=$1
         )
         SELECT flags.*,
           EXISTS (
             SELECT 1 FROM sources s JOIN receipts r ON r.inbox_message_id=s.message_id
             WHERE r.bytes IS NOT NULL AND r.appointment_id IS NULL
           ) AS pending_receipt,
           EXISTS (
             SELECT 1 FROM sources s JOIN receipt_intake_state state
               ON state.inbox_message_id=s.message_id
             WHERE state.state='pending'
           ) AS pending_intake
         FROM flags"#,
      vec![outbox_id.into(), cutoff.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Retention association state unavailable".into()))?;
  Ok(AssociationState {
    count: row.try_get("", "association_count")?,
    all_eligible: row.try_get("", "all_eligible")?,
    invalid: row.try_get("", "invalid")?,
    pending_receipt: row.try_get("", "pending_receipt")?,
    pending_intake: row.try_get("", "pending_intake")?,
  })
}

async fn policy_attention_contacts(
  tx: &sea_orm::DatabaseTransaction,
  cutoff: i64,
) -> Result<BTreeSet<Option<Uuid>>, DbErr> {
  let mut contacts = BTreeSet::new();
  for row in tx
    .query_all(sql(INBOX_ATTENTION_SQL, vec![cutoff.into()]))
    .await?
  {
    contacts.insert(row.try_get::<Option<Uuid>>("", "contact_id")?);
  }
  for row in tx
    .query_all(sql(OUTBOX_ATTENTION_SQL, vec![cutoff.into()]))
    .await?
  {
    contacts.insert(row.try_get::<Option<Uuid>>("", "contact_id")?);
  }
  // Doctor-phone text has no explicit Appointment link, so preserve and escalate it.
  for row in tx
    .query_all(sql(
      "SELECT DISTINCT contact_id FROM phone_takeover_events WHERE attribution_status='attributed' AND body IS NOT NULL AND text_deleted_at IS NULL",
      vec![],
    ))
    .await?
  {
    contacts.insert(row.try_get::<Option<Uuid>>("", "contact_id")?);
  }
  Ok(contacts)
}

async fn ensure_policy_attention(
  tx: &sea_orm::DatabaseTransaction,
  contact: Option<Uuid>,
  now: i64,
) -> Result<bool, DbErr> {
  let exists = tx
    .query_one(sql(
      "SELECT 1 FROM attention_items WHERE kind=$1 AND appointment_id IS NULL AND contact_id IS NOT DISTINCT FROM $2 AND resolved_at IS NULL LIMIT 1",
      vec![RETENTION_ATTENTION.into(), contact.into()],
    ))
    .await?
    .is_some();
  if exists {
    return Ok(false);
  }
  store::attention(tx, None, contact, RETENTION_ATTENTION, now).await?;
  Ok(true)
}

async fn resolve_stale_policy_attention(
  tx: &sea_orm::DatabaseTransaction,
  current: &BTreeSet<Option<Uuid>>,
  now: i64,
) -> Result<bool, DbErr> {
  let mut changed = false;
  for row in tx
    .query_all(sql(
      "SELECT DISTINCT contact_id FROM attention_items WHERE kind=$1 AND appointment_id IS NULL AND resolved_at IS NULL",
      vec![RETENTION_ATTENTION.into()],
    ))
    .await?
  {
    let contact = row.try_get::<Option<Uuid>>("", "contact_id")?;
    if !current.contains(&contact) {
      let result = tx
        .execute(sql(
          "UPDATE attention_items SET resolved_at=$3 WHERE kind=$1 AND appointment_id IS NULL AND contact_id IS NOT DISTINCT FROM $2 AND resolved_at IS NULL",
          vec![RETENTION_ATTENTION.into(), contact.into(), now.into()],
        ))
        .await?;
      changed |= result.rows_affected() > 0;
    }
  }
  Ok(changed)
}

const INBOX_ATTENTION_SQL: &str = r#"WITH links AS (
  SELECT ma.inbox_message_id,ma.appointment_id FROM message_appointments ma
  UNION
  SELECT r.inbox_message_id,r.appointment_id FROM receipts r WHERE r.appointment_id IS NOT NULL
), state AS (
  SELECT i.message_id,i.contact_id,COUNT(a.id) AS association_count,
    COALESCE(bool_and(
      COALESCE(a.contact_id=i.contact_id,FALSE) AND CASE
        WHEN a.status='cancelled' THEN COALESCE(a.cancelled_at <= $1,FALSE)
        WHEN a.status='expired' THEN COALESCE(a.expired_at <= $1,FALSE)
        ELSE a.ends_at <= $1
      END
    ) FILTER (WHERE a.id IS NOT NULL),FALSE) AS all_eligible,
    COALESCE(bool_or(
      a.id IS NOT NULL AND (
        a.contact_id IS DISTINCT FROM i.contact_id
        OR (a.status='cancelled' AND a.cancelled_at IS NULL)
        OR (a.status='expired' AND a.expired_at IS NULL)
      )
    ),FALSE) AS invalid
  FROM inbox_messages i
  LEFT JOIN links l ON l.inbox_message_id=i.message_id
  LEFT JOIN appointments a ON a.id=l.appointment_id
  WHERE i.text_deleted_at IS NULL
  GROUP BY i.message_id,i.contact_id
)
SELECT DISTINCT s.contact_id
FROM state s
WHERE NOT EXISTS (
    SELECT 1 FROM receipt_intake_state pending
    WHERE pending.inbox_message_id=s.message_id AND pending.state='pending'
  )
  AND (
    s.association_count=0 OR s.invalid
    OR (s.association_count>1 AND NOT s.all_eligible)
    OR EXISTS (
      SELECT 1 FROM receipts r
      WHERE r.inbox_message_id=s.message_id
        AND r.bytes IS NOT NULL AND r.appointment_id IS NULL
    )
  )"#;

const OUTBOX_ATTENTION_SQL: &str = r#"WITH sources AS (
  SELECT o.id AS outbox_id,o.wrote_first_inbox_id AS message_id
  FROM outbox_messages o
  UNION
  SELECT o.id,o.reply_inbox_id FROM outbox_messages o
  WHERE o.reply_inbox_id IS NOT NULL
  UNION
  SELECT o.id,f.inbox_message_id FROM outbox_messages o
  JOIN followup_requests f ON f.id=o.resolved_followup_id
), links AS (
  SELECT o.id AS outbox_id,o.contact_id,o.appointment_id
  FROM outbox_messages o WHERE o.appointment_id IS NOT NULL
  UNION
  SELECT s.outbox_id,o.contact_id,ma.appointment_id
  FROM sources s JOIN outbox_messages o ON o.id=s.outbox_id
  JOIN message_appointments ma ON ma.inbox_message_id=s.message_id
  UNION
  SELECT s.outbox_id,o.contact_id,r.appointment_id
  FROM sources s JOIN outbox_messages o ON o.id=s.outbox_id
  JOIN receipts r ON r.inbox_message_id=s.message_id
  WHERE r.appointment_id IS NOT NULL
), state AS (
  SELECT o.id,o.contact_id,o.purpose,o.appointment_id,COUNT(a.id) AS association_count,
    COALESCE(bool_and(
      (o.purpose='doctor_notification' OR a.contact_id=o.contact_id) AND CASE
        WHEN a.status='cancelled' THEN COALESCE(a.cancelled_at <= $1,FALSE)
        WHEN a.status='expired' THEN COALESCE(a.expired_at <= $1,FALSE)
        ELSE a.ends_at <= $1
      END
    ) FILTER (WHERE a.id IS NOT NULL),FALSE) AS all_eligible,
    COALESCE(bool_or(
      a.id IS NOT NULL AND (
        (o.purpose<>'doctor_notification' AND a.contact_id IS DISTINCT FROM o.contact_id)
        OR (a.status='cancelled' AND a.cancelled_at IS NULL)
        OR (a.status='expired' AND a.expired_at IS NULL)
      )
    ),FALSE) AS invalid,
    EXISTS (
      SELECT 1 FROM sources s JOIN inbox_messages i ON i.message_id=s.message_id
      WHERE s.outbox_id=o.id AND o.purpose<>'doctor_notification'
        AND i.contact_id IS DISTINCT FROM o.contact_id
    ) AS invalid_source,
    EXISTS (
      SELECT 1 FROM sources s JOIN receipts r ON r.inbox_message_id=s.message_id
      WHERE s.outbox_id=o.id AND r.bytes IS NOT NULL AND r.appointment_id IS NULL
    ) AS pending_receipt,
    EXISTS (
      SELECT 1 FROM sources s JOIN receipt_intake_state pending
        ON pending.inbox_message_id=s.message_id
      WHERE s.outbox_id=o.id AND pending.state='pending'
    ) AS pending_intake
  FROM outbox_messages o
  LEFT JOIN links l ON l.outbox_id=o.id
  LEFT JOIN appointments a ON a.id=l.appointment_id
  WHERE o.text_deleted_at IS NULL
  GROUP BY o.id,o.contact_id,o.purpose,o.appointment_id
)
SELECT DISTINCT
  CASE WHEN s.purpose='doctor_notification'
    THEN COALESCE((SELECT a.contact_id FROM appointments a WHERE a.id=s.appointment_id),s.contact_id)
    ELSE s.contact_id
  END AS contact_id
FROM state s
WHERE NOT s.pending_intake
  AND (s.association_count=0 OR s.invalid OR s.invalid_source OR s.pending_receipt
    OR (s.association_count>1 AND NOT s.all_eligible))"#;

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicI64, Ordering};

  use crate::{app::tests::with_database, database::Db, modules::doctor::Clock};
  use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
  use uuid::Uuid;

  use super::{run_once, NINETY_DAYS_MS};

  const NOW_MS: i64 = 1_800_000_000_000;
  const CONTACT: &str = "15550001111@c.us";

  struct TestClock(AtomicI64);
  impl Clock for TestClock {
    fn now(&self) -> i64 {
      self.now_millis().div_euclid(1000)
    }
    fn now_millis(&self) -> i64 {
      self.0.load(Ordering::SeqCst)
    }
  }

  fn test_sql(query: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, query, values)
  }

  async fn insert_appointment(
    db: &Db,
    id: Uuid,
    contact: Uuid,
    interval: (i64, i64),
    status: &str,
    cancelled_at: Option<i64>,
    expired_at: Option<i64>,
  ) {
    let (start, end) = interval;
    db.conn
    .execute(test_sql(
      "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,cancelled_at,expired_at,created_at,updated_at) VALUES ($1,$2,'Nombre conservado',$3,'Servicio conservado',30,12500,'in_person',$4,$5,'America/Mexico_City','Consultorio',0,0,$6,'not_required','synced','Banco conservado','Beneficiario conservado','001234567890123456',$7,$8,$9,$9)",
      vec![
        id.into(),
        contact.into(),
        Uuid::now_v7().into(),
        start.into(),
        end.into(),
        status.into(),
        cancelled_at.into(),
        expired_at.into(),
        (NOW_MS - NINETY_DAYS_MS - 60_000).into(),
      ],
    ))
    .await
    .unwrap();
  }

  async fn insert_inbox(db: &Db, id: &str, contact: Uuid, at: i64, body: &str) {
    db.conn
      .execute(test_sql(
        "INSERT INTO inbox_messages(message_id,original_chat_identity,server_timestamp,arrived_at,raw_envelope,preparation_state,message_kind,body,media_metadata,prepared_at,contact_id,processed_at) VALUES ($1,$2,1,to_timestamp($3::double precision/1000),convert_to($4,'UTF8'),'ready_for_conversation','text',$5,'{\"url\":\"https://media.invalid/private\",\"filename\":\"private.pdf\"}'::jsonb,to_timestamp($3::double precision/1000),$6,$3)",
        vec![
          id.into(),
          CONTACT.into(),
          at.into(),
          format!("{{\"payload\":{{\"body\":\"{body}\"}}}}",).into(),
          body.into(),
          contact.into(),
        ],
      ))
      .await
      .unwrap();
  }

  async fn associate(db: &Db, message: &str, appointment: Uuid) {
    db.conn
      .execute(test_sql(
        "INSERT INTO message_appointments(inbox_message_id,appointment_id) VALUES ($1,$2)",
        vec![message.into(), appointment.into()],
      ))
      .await
      .unwrap();
  }

  async fn insert_outbox(
    db: &Db,
    contact: Uuid,
    source: &str,
    appointment: Option<Uuid>,
    suffix: &str,
    created_at: i64,
    purpose: &str,
  ) {
    let id = Uuid::now_v7();
    let provider_id = format!("PROVIDER-{suffix}");
    let target = format!("true_15550001111@c.us_{suffix}");
    db.conn
      .execute(test_sql(
        "INSERT INTO outbox_messages(id,contact_id,wrote_first_inbox_id,original_target,reply_inbox_id,template,rendered_text,privacy_prefix,created_at,state,typing_delay_ms,eligible_at,raw_provider_id,returned_id,dispatch_committed_at,dispatch_deadline,positive_ack,appointment_id,dedupe_key,expected_revision,purpose) VALUES ($1,$2,$3,$4,$3,'appointment',$5,false,$6,'sent',3000,$6,$7,$8,$6,$6,1,$9,$10,CASE WHEN $9::uuid IS NULL THEN NULL ELSE 1 END,$11)",
        vec![
          id.into(),
          contact.into(),
          source.into(),
          CONTACT.into(),
          format!("Respuesta con datos {suffix}").into(),
          created_at.into(),
          provider_id.into(),
          target.into(),
          appointment.into(),
          format!("outbox-{suffix}").into(),
          purpose.into(),
        ],
      ))
      .await
      .unwrap();
  }

  #[tokio::test]
  #[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
  async fn ninety_day_boundary_redacts_only_fully_eligible_copies_and_preserves_facts() {
    with_database(|db| async move {
      db.run_migrations().await.unwrap();
      let contact = Uuid::now_v7();
      db.conn
        .execute(test_sql(
          "INSERT INTO contacts(id,created_at) VALUES ($1,$2)",
          vec![contact.into(), (NOW_MS - NINETY_DAYS_MS - 86_400_000).into()],
        ))
        .await
        .unwrap();

      let anchor = NOW_MS - NINETY_DAYS_MS;

      db.conn
        .execute(test_sql(
          "INSERT INTO phone_takeover_events(message_fingerprint,contact_id,attribution_status,server_timestamp,body,created_at) VALUES (decode(repeat('ab',32),'hex'),$1,'attributed',$2,'Mensaje del Doctor desde su teléfono',$3)",
          vec![contact.into(), anchor.div_euclid(1000).into(), anchor.into()],
        ))
        .await
        .unwrap();
      let past = Uuid::now_v7();
      let cancelled = Uuid::now_v7();
      let expired = Uuid::now_v7();
      let future = Uuid::now_v7();
      insert_appointment(&db, past, contact, (anchor - 60_000, anchor), "confirmed", None, None)
      .await;
      insert_appointment(&db, cancelled, contact, (anchor + 60_000, anchor + 120_000), "cancelled", Some(anchor), None)
      .await;
      insert_appointment(&db, expired, contact, (anchor + 180_000, anchor + 240_000), "expired", None, Some(anchor))
      .await;
      insert_appointment(&db, future, contact, (NOW_MS, NOW_MS + 3_600_000), "confirmed", None, None)
      .await;

      insert_inbox(&db, "eligible-past", contact, anchor, "dato reservado").await;
      insert_inbox(&db, "eligible-cancelled", contact, anchor, "cancelado").await;
      insert_inbox(&db, "eligible-expired", contact, anchor, "expirado").await;
      insert_inbox(&db, "shared", contact, anchor, "texto compartido").await;
      insert_inbox(&db, "unassociated", contact, anchor, "texto sin cita").await;
      associate(&db, "eligible-past", past).await;
      associate(&db, "eligible-cancelled", cancelled).await;
      associate(&db, "eligible-expired", expired).await;
      associate(&db, "shared", past).await;
      associate(&db, "shared", future).await;

      let receipt_id = Uuid::now_v7();
      db.conn
        .execute(test_sql(
          "INSERT INTO receipts(id,contact_id,inbox_message_id,appointment_id,mime,bytes,sent_at,arrived_at,timely,late,created_at) VALUES ($1,$2,'eligible-past',$3,'application/pdf',decode('25504446','hex'),$4,$4,true,false,$4)",
          vec![receipt_id.into(), contact.into(), past.into(), anchor.into()],
        ))
        .await
        .unwrap();
      insert_outbox(&db, contact, "eligible-past", Some(past), "eligible", anchor, "status").await;
      insert_outbox(&db, contact, "shared", Some(past), "shared", anchor, "status").await;
      insert_outbox(&db, contact, "unassociated", None, "unassociated", anchor, "status").await;
      db.conn
        .execute(test_sql(
          "INSERT INTO appointment_history(id,appointment_id,actor,kind,created_at,data) VALUES ($1,$2,'System','booked',$3,'{\"snapshot\":\"kept\"}'::jsonb)",
          vec![Uuid::now_v7().into(), past.into(), anchor.into()],
        ))
        .await
        .unwrap();

      let clock = TestClock(AtomicI64::new(NOW_MS - 1));
      assert!(run_once(&db, &clock).await.unwrap());
      let receipt = db
        .conn
        .query_one(test_sql("SELECT bytes,deleted_at FROM receipts WHERE id=$1", vec![receipt_id.into()]))
        .await
        .unwrap()
        .unwrap();
      assert!(receipt.try_get::<Option<Vec<u8>>>("", "bytes").unwrap().is_some());
      assert!(receipt.try_get::<Option<i64>>("", "deleted_at").unwrap().is_none());

      clock.0.store(NOW_MS, Ordering::SeqCst);
      let db_a = db.clone();
      let db_b = db.clone();
      let (a, b) = tokio::join!(run_once(&db_a, &clock), run_once(&db_b, &clock));
      let outcomes = [a.unwrap(), b.unwrap()];
      assert_eq!(outcomes.into_iter().filter(|worked| *worked).count(), 1);

      let receipt = db
        .conn
        .query_one(test_sql(
          "SELECT bytes,deleted_at FROM receipts WHERE id=$1",
          vec![receipt_id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
      assert!(receipt.try_get::<Option<Vec<u8>>>("", "bytes").unwrap().is_none());
      assert_eq!(receipt.try_get::<Option<i64>>("", "deleted_at").unwrap(), Some(NOW_MS));

      let rows = db
        .conn
        .query_all(test_sql(
          "SELECT message_id,raw_envelope,body,media_metadata,text_deleted_at FROM inbox_messages WHERE message_id IN ('eligible-past','shared','unassociated') ORDER BY message_id",
          vec![],
        ))
        .await
        .unwrap();
      let eligible = rows.iter().find(|row| row.try_get::<String>("", "message_id").unwrap() == "eligible-past").unwrap();
      assert_eq!(eligible.try_get::<Option<String>>("", "body").unwrap(), None);
      assert_eq!(eligible.try_get::<Option<serde_json::Value>>("", "media_metadata").unwrap(), None);
      assert_eq!(eligible.try_get::<Option<i64>>("", "text_deleted_at").unwrap(), Some(NOW_MS));
      assert_eq!(eligible.try_get::<Vec<u8>>("", "raw_envelope").unwrap(), b"{\"retention_redacted\":true}");
      let shared = rows.iter().find(|row| row.try_get::<String>("", "message_id").unwrap() == "shared").unwrap();
      assert_eq!(shared.try_get::<Option<String>>("", "body").unwrap().as_deref(), Some("texto compartido"));
      let unassociated = rows.iter().find(|row| row.try_get::<String>("", "message_id").unwrap() == "unassociated").unwrap();
      assert_eq!(unassociated.try_get::<Option<String>>("", "body").unwrap().as_deref(), Some("texto sin cita"));
      for id in ["eligible-cancelled", "eligible-expired"] {
        let row = db
          .conn
          .query_one(test_sql(
            "SELECT text_deleted_at FROM inbox_messages WHERE message_id=$1",
            vec![id.into()],
          ))
          .await
          .unwrap()
          .unwrap();
        assert_eq!(row.try_get::<Option<i64>>("", "text_deleted_at").unwrap(), Some(NOW_MS));
      }

      let outbox = db
        .conn
        .query_all(test_sql(
          "SELECT rendered_text,text_deleted_at,state,raw_provider_id FROM outbox_messages ORDER BY dedupe_key",
          vec![],
        ))
        .await
        .unwrap();
      let eligible_outbox = outbox.iter().find(|row| row.try_get::<String>("", "raw_provider_id").unwrap() == "PROVIDER-eligible").unwrap();
      assert!(eligible_outbox.try_get::<String>("", "rendered_text").unwrap().contains("removed by retention"));
      assert_eq!(eligible_outbox.try_get::<Option<i64>>("", "text_deleted_at").unwrap(), Some(NOW_MS));
      assert_eq!(eligible_outbox.try_get::<String>("", "state").unwrap(), "sent");
      for id in ["PROVIDER-shared", "PROVIDER-unassociated"] {
        let row = outbox.iter().find(|row| row.try_get::<String>("", "raw_provider_id").unwrap() == id).unwrap();
        assert!(row.try_get::<String>("", "rendered_text").unwrap().contains("Respuesta con datos"));
        assert!(row.try_get::<Option<i64>>("", "text_deleted_at").unwrap().is_none());
      }

      let facts = db
        .conn
        .query_one(test_sql(
          "SELECT patient_name,price_centavos,bank,beneficiary,clabe,(SELECT count(*) FROM appointment_history WHERE appointment_id=$1) AS history_count FROM appointments WHERE id=$1",
          vec![past.into()],
        ))
        .await
        .unwrap()
        .unwrap();
      assert_eq!(facts.try_get::<String>("", "patient_name").unwrap(), "Nombre conservado");
      assert_eq!(facts.try_get::<i64>("", "price_centavos").unwrap(), 12_500);
      assert_eq!(facts.try_get::<Option<String>>("", "bank").unwrap().as_deref(), Some("Banco conservado"));
      assert_eq!(facts.try_get::<Option<String>>("", "beneficiary").unwrap().as_deref(), Some("Beneficiario conservado"));
      assert_eq!(facts.try_get::<Option<String>>("", "clabe").unwrap().as_deref(), Some("001234567890123456"));
      assert_eq!(facts.try_get::<i64>("", "history_count").unwrap(), 1);
      assert_eq!(
        db.conn
          .query_one(test_sql(
            "SELECT count(*) AS count FROM attention_items WHERE kind='retention_policy_review' AND resolved_at IS NULL",
            vec![],
          ))
          .await
          .unwrap()
          .unwrap()
          .try_get::<i64>("", "count")
          .unwrap(),
        1
      );
      db.conn
        .execute(test_sql(
          "DELETE FROM message_appointments WHERE inbox_message_id='shared' AND appointment_id=$1",
          vec![future.into()],
        ))
        .await
        .unwrap();
      associate(&db, "unassociated", future).await;
      assert!(run_once(&db, &clock).await.unwrap());
      let resolved = db
        .conn
        .query_one(test_sql(
          "SELECT resolved_at FROM attention_items WHERE kind='retention_policy_review' AND contact_id=$1",
          vec![contact.into()],
        ))
        .await
        .unwrap()
        .unwrap();
      assert_eq!(resolved.try_get::<Option<i64>>("", "resolved_at").unwrap(), None);
      let phone_event = db
        .conn
        .query_one(test_sql(
          "SELECT body,text_deleted_at FROM phone_takeover_events WHERE message_fingerprint=decode(repeat('ab',32),'hex')",
          vec![],
        ))
        .await
        .unwrap()
        .unwrap();
      assert_eq!(
        phone_event.try_get::<Option<String>>("", "body").unwrap().as_deref(),
        Some("Mensaje del Doctor desde su teléfono")
      );
      assert_eq!(phone_event.try_get::<Option<i64>>("", "text_deleted_at").unwrap(), None);
      assert!(!run_once(&db, &clock).await.unwrap());
    })
    .await;
  }

  #[tokio::test]
  #[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
  async fn doctor_notification_retention_uses_appointment_owner_not_recipient() {
    with_database(|db| async move {
      db.run_migrations().await.unwrap();
      let patient = Uuid::now_v7();
      let doctor = Uuid::now_v7();
      let anchor = NOW_MS - NINETY_DAYS_MS;
      db.conn
        .execute(test_sql(
          "INSERT INTO contacts(id,created_at) VALUES ($1,$3),($2,$3)",
          vec![patient.into(), doctor.into(), anchor.into()],
        ))
        .await
        .unwrap();

      let appointment = Uuid::now_v7();
      insert_appointment(&db, appointment, patient, (anchor - 60_000, anchor), "confirmed", None, None)
      .await;
      insert_inbox(&db, "doctor-source", doctor, anchor, "binding").await;
      insert_inbox(&db, "ordinary-source", doctor, anchor, "binding").await;
      db.conn
        .execute(test_sql(
          "UPDATE inbox_messages SET raw_envelope=$1,body=NULL,media_metadata=NULL,text_deleted_at=$2 WHERE message_id IN ('doctor-source','ordinary-source')",
          vec![b"{\"retention_redacted\":true}".to_vec().into(), anchor.into()],
        ))
        .await
        .unwrap();
      insert_outbox(
        &db,
        doctor,
        "doctor-source",
        Some(appointment),
        "doctor-notice",
        anchor,
        "doctor_notification",
      )
      .await;
      insert_outbox(
        &db,
        doctor,
        "ordinary-source",
        Some(appointment),
        "ordinary-status",
        anchor,
        "status",
      )
      .await;

      let clock = TestClock(AtomicI64::new(NOW_MS));
      assert!(run_once(&db, &clock).await.unwrap());
      let rows = db
        .conn
        .query_all(test_sql(
          "SELECT raw_provider_id,rendered_text,text_deleted_at FROM outbox_messages ORDER BY raw_provider_id",
          vec![],
        ))
        .await
        .unwrap();
      let doctor_notice = rows
        .iter()
        .find(|row| row.try_get::<String>("", "raw_provider_id").unwrap() == "PROVIDER-doctor-notice")
        .unwrap();
      assert!(doctor_notice
        .try_get::<String>("", "rendered_text")
        .unwrap()
        .contains("removed by retention"));
      assert_eq!(
        doctor_notice.try_get::<Option<i64>>("", "text_deleted_at").unwrap(),
        Some(NOW_MS)
      );
      let ordinary = rows
        .iter()
        .find(|row| row.try_get::<String>("", "raw_provider_id").unwrap() == "PROVIDER-ordinary-status")
        .unwrap();
      assert!(ordinary
        .try_get::<String>("", "rendered_text")
        .unwrap()
        .contains("Respuesta con datos"));
      assert_eq!(
        ordinary.try_get::<Option<i64>>("", "text_deleted_at").unwrap(),
        None
      );
      for (contact, expected) in [(patient, 0_i64), (doctor, 1_i64)] {
        let count = db
          .conn
          .query_one(test_sql(
            "SELECT count(*) AS count FROM attention_items WHERE kind='retention_policy_review' AND contact_id=$1 AND resolved_at IS NULL",
            vec![contact.into()],
          ))
          .await
          .unwrap()
          .unwrap()
          .try_get::<i64>("", "count")
          .unwrap();
        assert_eq!(count, expected);
      }
    })
    .await;
  }

  #[tokio::test]
  #[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
  async fn retention_skips_an_appointment_locked_for_late_payment_recovery() {
    with_database(|db| async move {
      db.run_migrations().await.unwrap();
      let contact = Uuid::now_v7();
      let anchor = NOW_MS - NINETY_DAYS_MS;
      db.conn
        .execute(test_sql(
          "INSERT INTO contacts(id,created_at) VALUES ($1,$2)",
          vec![contact.into(), (NOW_MS - NINETY_DAYS_MS - 86_400_000).into()],
        ))
        .await
        .unwrap();
      let appointment = Uuid::now_v7();
      insert_appointment(&db, appointment, contact, (anchor - 60_000, anchor), "expired", None, Some(anchor))
      .await;
      insert_inbox(&db, "late-recovery", contact, anchor, "late payment proof").await;
      associate(&db, "late-recovery", appointment).await;
      let receipt = Uuid::now_v7();
      db.conn
        .execute(test_sql(
          "INSERT INTO receipts(id,contact_id,inbox_message_id,appointment_id,mime,bytes,sent_at,arrived_at,timely,late,created_at) VALUES ($1,$2,'late-recovery',$3,'application/pdf',decode('25504446','hex'),$4,$4,true,false,$4)",
          vec![receipt.into(), contact.into(), appointment.into(), anchor.into()],
        ))
        .await
        .unwrap();
      insert_outbox(
        &db,
        contact,
        "late-recovery",
        Some(appointment),
        "late-recovery",
        anchor,
        "status",
      )
      .await;

      // Simulate a payment-recovery transaction already holding the lifecycle locks
      // while retention's non-locking candidate scan still sees the old expired row.
      let recovery = db.conn.begin().await.unwrap();
      recovery
        .query_one(test_sql(
          "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
          vec![contact.into()],
        ))
        .await
        .unwrap()
        .unwrap();
      recovery
        .query_one(test_sql(
          "SELECT id FROM appointments WHERE id=$1 FOR UPDATE",
          vec![appointment.into()],
        ))
        .await
        .unwrap()
        .unwrap();
      recovery
        .execute(test_sql(
          "UPDATE appointments SET status='confirmed',payment='verified',starts_at=$2,ends_at=$3,expired_at=NULL,revision=revision+1,updated_at=$4 WHERE id=$1",
          vec![
            appointment.into(),
            (NOW_MS + 60_000).into(),
            (NOW_MS + 3_660_000).into(),
            NOW_MS.into(),
          ],
        ))
        .await
        .unwrap();

      let clock = TestClock(AtomicI64::new(NOW_MS));
      assert!(!run_once(&db, &clock).await.unwrap());
      let preserved = db
        .conn
        .query_one(test_sql(
          "SELECT rendered_text,text_deleted_at FROM outbox_messages WHERE raw_provider_id='PROVIDER-late-recovery'",
          vec![],
        ))
        .await
        .unwrap()
        .unwrap();
      assert!(preserved
        .try_get::<String>("", "rendered_text")
        .unwrap()
        .contains("Respuesta con datos late-recovery"));
      assert_eq!(
        preserved.try_get::<Option<i64>>("", "text_deleted_at").unwrap(),
        None
      );
      let preserved = db
        .conn
        .query_one(test_sql(
          "SELECT encode(bytes,'hex') AS bytes FROM receipts WHERE id=$1",
          vec![receipt.into()],
        ))
        .await
        .unwrap()
        .unwrap();
      assert_eq!(preserved.try_get::<String>("", "bytes").unwrap(), "25504446");
      let preserved = db
        .conn
        .query_one(test_sql(
          "SELECT body,text_deleted_at FROM inbox_messages WHERE message_id='late-recovery'",
          vec![],
        ))
        .await
        .unwrap()
        .unwrap();
      assert_eq!(
        preserved.try_get::<String>("", "body").unwrap(),
        "late payment proof"
      );
      assert_eq!(
        preserved.try_get::<Option<i64>>("", "text_deleted_at").unwrap(),
        None
      );
      recovery.commit().await.unwrap();

      assert!(!run_once(&db, &clock).await.unwrap());
      let preserved = db
        .conn
        .query_one(test_sql(
          "SELECT encode(bytes,'hex') AS bytes FROM receipts WHERE id=$1",
          vec![receipt.into()],
        ))
        .await
        .unwrap()
        .unwrap();
      assert_eq!(preserved.try_get::<String>("", "bytes").unwrap(), "25504446");
      let preserved = db
        .conn
        .query_one(test_sql(
          "SELECT body,text_deleted_at FROM inbox_messages WHERE message_id='late-recovery'",
          vec![],
        ))
        .await
        .unwrap()
        .unwrap();
      assert_eq!(
        preserved.try_get::<String>("", "body").unwrap(),
        "late payment proof"
      );
      assert_eq!(
        preserved.try_get::<Option<i64>>("", "text_deleted_at").unwrap(),
        None
      );
    })
    .await;
  }
}
