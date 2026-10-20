use crate::{database::Db, modules::whatsapp::inbox::sql};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use sea_orm::{ConnectionTrait, DbErr};
use uuid::Uuid;

const PAGE_SIZE: i64 = 25;

pub struct InboxContact {
  pub id: Uuid,
  pub label: String,
  pub last_activity: String,
  pub identity_conflict: bool,
  pub has_open_followup: bool,
  pub takeover_until: Option<String>,
}

pub struct InboxPage {
  pub contacts: Vec<InboxContact>,
  pub next_page: bool,
}

pub struct Message {
  pub incoming: bool,
  pub human_outgoing: bool,
  pub occurred_at: String,
  pub received_at: Option<String>,
  pub dispatch_at: Option<String>,
  pub ack_at: Option<String>,
  pub text: Option<String>,
  pub kind: Option<String>,
  pub text_deleted_at: Option<String>,
  pub outbox_state: Option<String>,
  pub positive_ack: i32,
  pub ack_error: bool,
  pub appointment_ids: Vec<Uuid>,
  pub receipt_id: Option<Uuid>,
  pub receipt_deleted: bool,
}

pub struct ContactHistory {
  pub id: Uuid,
  pub label: String,
  pub identity_conflict: bool,
  pub takeover_until: Option<String>,
  pub open_followup: Option<Uuid>,
  pub messages: Vec<Message>,
  pub previous_page: Option<u32>,
  pub next_page: Option<u32>,
}

fn timezone(value: Option<String>) -> Tz {
  value
    .as_deref()
    .and_then(|value| value.parse().ok())
    .unwrap_or(chrono_tz::UTC)
}

fn local_time(timestamp: i64, zone: Tz) -> Result<String, DbErr> {
  DateTime::<Utc>::from_timestamp_millis(timestamp)
    .map(|time| {
      time
        .with_timezone(&zone)
        .format("%Y-%m-%d %H:%M %Z")
        .to_string()
    })
    .ok_or_else(|| DbErr::Custom("Stored message timestamp is invalid".into()))
}

async fn practice_timezone(db: &Db) -> Result<Tz, DbErr> {
  let row = db
    .conn
    .query_one(sql("SELECT timezone FROM practice WHERE id=1", vec![]))
    .await?;
  Ok(timezone(row.and_then(|row| {
    row.try_get::<Option<String>>("", "timezone").ok().flatten()
  })))
}

fn contact_label(phone: Option<String>, identity_conflict: bool) -> String {
  if identity_conflict {
    "Contacto con identidad en conflicto".to_owned()
  } else {
    phone
      .map(|phone| format!("+{}", phone.trim_end_matches("@c.us")))
      .unwrap_or_else(|| "Contacto sin número resuelto".to_owned())
  }
}

pub async fn inbox(db: &Db, page: u32, now: i64) -> Result<InboxPage, DbErr> {
  let zone = practice_timezone(db).await?;
  let rows = db
    .conn
    .query_all(sql(
      "WITH activity AS (
        SELECT contact_id, max(COALESCE(server_timestamp * 1000,floor(extract(epoch FROM arrived_at) * 1000)::bigint)) AS at FROM inbox_messages WHERE contact_id IS NOT NULL GROUP BY contact_id
        UNION ALL
        SELECT contact_id, max(created_at) AS at FROM outbox_messages GROUP BY contact_id
        UNION ALL
        SELECT p.contact_id, max(COALESCE(p.server_timestamp * 1000,p.created_at)) AS at
        FROM phone_takeover_events p
        WHERE p.attribution_status='attributed' AND p.contact_id IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM contacts c WHERE c.id=p.contact_id AND c.identity_conflict)
        GROUP BY p.contact_id
      ), latest AS (
        SELECT contact_id, max(at) AS at FROM activity GROUP BY contact_id
      )
      SELECT c.id, c.identity_conflict, c.takeover_until, latest.at AS last_activity,
        (SELECT min(jid) FROM contact_aliases WHERE contact_id=c.id AND jid LIKE '%@c.us') AS phone,
        EXISTS (SELECT FROM followup_requests WHERE contact_id=c.id AND resolved_at IS NULL) AS has_open_followup
      FROM contacts c JOIN latest ON latest.contact_id=c.id
      ORDER BY latest.at DESC,c.id DESC LIMIT 26 OFFSET $1",
      vec![(i64::from(page) * PAGE_SIZE).into()],
    ))
    .await?;
  let next_page = rows.len() > PAGE_SIZE as usize;
  let mut contacts = Vec::with_capacity(rows.len().min(PAGE_SIZE as usize));
  for row in rows.into_iter().take(PAGE_SIZE as usize) {
    let identity_conflict = row.try_get::<bool>("", "identity_conflict")?;
    let last_activity = local_time(row.try_get("", "last_activity")?, zone)?;
    let takeover_until = row
      .try_get::<Option<i64>>("", "takeover_until")?
      .filter(|until| *until > now)
      .map(|until| local_time(until, zone))
      .transpose()?;
    contacts.push(InboxContact {
      id: row.try_get("", "id")?,
      label: contact_label(row.try_get("", "phone")?, identity_conflict),
      last_activity,
      identity_conflict,
      has_open_followup: row.try_get("", "has_open_followup")?,
      takeover_until,
    });
  }
  Ok(InboxPage {
    contacts,
    next_page,
  })
}

pub async fn contact(
  db: &Db,
  id: Uuid,
  page: u32,
  now: i64,
) -> Result<Option<ContactHistory>, DbErr> {
  let zone = practice_timezone(db).await?;
  let Some(row) = db
    .conn
    .query_one(sql(
      "SELECT c.id,c.identity_conflict,c.takeover_until,
        (SELECT min(jid) FROM contact_aliases WHERE contact_id=c.id AND jid LIKE '%@c.us') AS phone,
        (SELECT id FROM followup_requests WHERE contact_id=c.id AND resolved_at IS NULL ORDER BY created_at,id LIMIT 1) AS open_followup
       FROM contacts c WHERE c.id=$1",
      vec![id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  let identity_conflict = row.try_get::<bool>("", "identity_conflict")?;
  let label = contact_label(row.try_get("", "phone")?, identity_conflict);
  let takeover_until = row
    .try_get::<Option<i64>>("", "takeover_until")?
    .filter(|until| *until > now)
    .map(|until| local_time(until, zone))
    .transpose()?;
  let open_followup = row.try_get("", "open_followup")?;
  let rows = db
    .conn
    .query_all(sql(
      "WITH messages AS (
        SELECT 'incoming'::text AS direction,i.message_id AS source_id,
          COALESCE(i.server_timestamp * 1000,floor(extract(epoch FROM i.arrived_at) * 1000)::bigint) AS occurred_at,
          floor(extract(epoch FROM i.arrived_at) * 1000)::bigint AS received_at,NULL::bigint AS dispatch_at,NULL::bigint AS ack_at,
          i.body AS message_text,i.message_kind AS message_kind,i.text_deleted_at,NULL::text AS outbox_state,
          0::integer AS positive_ack,false AS ack_error,
          COALESCE((SELECT string_agg(ma.appointment_id::text,',' ORDER BY ma.appointment_id)
            FROM message_appointments ma JOIN appointments a ON a.id=ma.appointment_id AND a.contact_id=i.contact_id
            WHERE ma.inbox_message_id=i.message_id),'') AS appointment_ids,
          (SELECT r.id::text FROM receipts r WHERE r.inbox_message_id=i.message_id AND r.contact_id=i.contact_id) AS receipt_id,
          COALESCE((SELECT r.deleted_at IS NOT NULL OR r.bytes IS NULL FROM receipts r
            WHERE r.inbox_message_id=i.message_id AND r.contact_id=i.contact_id),false) AS receipt_deleted
        FROM inbox_messages i WHERE i.contact_id=$1
        UNION ALL
        SELECT 'outgoing'::text,o.id::text,o.created_at,NULL::bigint,o.dispatch_committed_at,
          (SELECT max(e.arrived_at) FROM outgoing_ack_evidence e WHERE e.matched_outbox_id=o.id AND e.ack>0),
          o.rendered_text,'text'::text,o.text_deleted_at,o.state,o.positive_ack,o.ack_error,
          COALESCE((SELECT a.id::text FROM appointments a WHERE a.id=o.appointment_id AND a.contact_id=o.contact_id),'') AS appointment_ids,
          NULL::text AS receipt_id,false AS receipt_deleted
        FROM outbox_messages o WHERE o.contact_id=$1
        UNION ALL
        SELECT 'phone_outgoing'::text AS direction,encode(p.message_fingerprint,'hex') AS source_id,
          COALESCE(p.server_timestamp * 1000,p.created_at) AS occurred_at,p.created_at AS received_at,
          NULL::bigint AS dispatch_at,NULL::bigint AS ack_at,p.body AS message_text,'text'::text AS message_kind,
          p.text_deleted_at,NULL::text AS outbox_state,0::integer AS positive_ack,false AS ack_error,
          ''::text AS appointment_ids,NULL::text AS receipt_id,false AS receipt_deleted
        FROM phone_takeover_events p
        WHERE p.contact_id=$1 AND p.attribution_status='attributed'
          AND NOT EXISTS (SELECT 1 FROM contacts c WHERE c.id=p.contact_id AND c.identity_conflict)
      )
      SELECT * FROM messages ORDER BY occurred_at DESC,direction DESC,source_id DESC LIMIT 26 OFFSET $2",
      vec![id.into(), (i64::from(page) * PAGE_SIZE).into()],
    ))
    .await?;
  let next_page = rows.len() > PAGE_SIZE as usize;
  let mut messages = Vec::with_capacity(rows.len().min(PAGE_SIZE as usize));
  for row in rows.into_iter().take(PAGE_SIZE as usize) {
    let direction = row.try_get::<String>("", "direction")?;
    let incoming = direction == "incoming";
    let human_outgoing = direction == "phone_outgoing";
    let occurred_at = local_time(row.try_get("", "occurred_at")?, zone)?;
    let received_at = row
      .try_get::<Option<i64>>("", "received_at")?
      .map(|time| local_time(time, zone))
      .transpose()?;
    let dispatch_at = row
      .try_get::<Option<i64>>("", "dispatch_at")?
      .map(|time| local_time(time, zone))
      .transpose()?;
    let ack_at = row
      .try_get::<Option<i64>>("", "ack_at")?
      .map(|time| local_time(time, zone))
      .transpose()?;
    let text_deleted_at = row
      .try_get::<Option<i64>>("", "text_deleted_at")?
      .map(|time| local_time(time, zone))
      .transpose()?;
    let appointment_ids = row
      .try_get::<String>("", "appointment_ids")?
      .split(',')
      .filter_map(|id| Uuid::parse_str(id).ok())
      .collect();
    messages.push(Message {
      human_outgoing,
      incoming,
      occurred_at,
      received_at,
      dispatch_at,
      ack_at,
      text: row.try_get("", "message_text")?,
      kind: row.try_get("", "message_kind")?,
      text_deleted_at,
      outbox_state: row.try_get("", "outbox_state")?,
      positive_ack: row.try_get("", "positive_ack")?,
      ack_error: row.try_get("", "ack_error")?,
      appointment_ids,
      receipt_id: row
        .try_get::<Option<String>>("", "receipt_id")?
        .and_then(|id| Uuid::parse_str(&id).ok()),
      receipt_deleted: row.try_get("", "receipt_deleted")?,
    });
  }
  messages.reverse();
  Ok(Some(ContactHistory {
    id,
    label,
    identity_conflict,
    takeover_until,
    open_followup,
    messages,
    previous_page: page.checked_sub(1),
    next_page: next_page.then(|| page.saturating_add(1)),
  }))
}
