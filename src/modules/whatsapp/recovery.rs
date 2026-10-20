use super::{
  client::{Client, HistoryMessage},
  inbox::{self, sql, ReceiptIntakeError},
  views::ConnectionState,
};
use crate::{
  common::config::Config,
  database::Db,
  modules::{appointments::store, doctor::Clock},
};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};
use serde_json::{Map, Value};
use uuid::Uuid;

const STATUS_INTERVAL_MS: i64 = 15_000;
const CLAIM_MS: i64 = 30_000;
const PAGE_LIMIT: u16 = 100;
const MAX_OFFSET: i64 = 100_000;
const OVERLAP_MS: i64 = 5 * 60 * 1000;

struct Claim {
  token: Uuid,
  provider_state: String,
  next_status_check_at: i64,
}

/// Polls one provider status, receipt retry, or bounded history page without holding SQL over HTTP.
pub async fn run_once(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
) -> Result<bool, DbErr> {
  let Some(client) = client else {
    return Ok(false);
  };
  if cfg.waha.is_none() {
    return Ok(false);
  }
  let now = clock.now_millis();
  let Some(claim) = claim(db, now).await? else {
    return Ok(false);
  };
  let working = if claim.next_status_check_at <= now {
    let state = client
      .status()
      .await
      .map(|status| status.state)
      .unwrap_or(ConnectionState::Unavailable);
    persist_status(db, claim.token, claim.provider_state.as_str(), state, now).await?;
    state == ConnectionState::Working
  } else {
    claim.provider_state == "working"
  };

  if !working {
    release(db, claim.token).await?;
    return Ok(true);
  }

  let mut did_work = false;
  match inbox::retry_pending_media(db, Some(client), now, cfg.public_origin.as_deref()).await {
    Ok(done) => did_work |= done,
    Err(ReceiptIntakeError::Database(error)) => return Err(error),
    Err(ReceiptIntakeError::Provider) => did_work = true,
  }
  did_work |= recover_one_page(db, clock, cfg, client, claim.token, now).await?;
  release(db, claim.token).await?;
  Ok(did_work)
}

async fn claim(db: &Db, now: i64) -> Result<Option<Claim>, DbErr> {
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  let row = tx
    .query_one(sql(
      "SELECT provider_state,next_status_check_at FROM provider_recovery_state WHERE singleton=true AND (claim_until IS NULL OR claim_until<=$1) FOR UPDATE",
      vec![now.into()],
    ))
    .await?;
  let Some(row) = row else {
    tx.commit().await?;
    return Ok(None);
  };
  let token = Uuid::now_v7();
  tx.execute(sql(
    "UPDATE provider_recovery_state SET claim_token=$1,claim_until=$2 WHERE singleton=true",
    vec![token.into(), now.saturating_add(CLAIM_MS).into()],
  ))
  .await?;
  let claim = Claim {
    token,
    provider_state: row.try_get("", "provider_state")?,
    next_status_check_at: row.try_get("", "next_status_check_at")?,
  };
  tx.commit().await?;
  Ok(Some(claim))
}

async fn persist_status(
  db: &Db,
  token: Uuid,
  previous: &str,
  state: ConnectionState,
  now: i64,
) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  let row = tx
    .query_one(sql(
      "SELECT disconnected_at,recovery_id,recovery_status,recovery_started_at,recovery_until,last_error_code FROM provider_recovery_state WHERE singleton=true AND claim_token=$1 FOR UPDATE",
      vec![token.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("WhatsApp recovery lease expired".into()))?;
  let disconnected_at: Option<i64> = row.try_get("", "disconnected_at")?;
  let mut recovery_id: Option<Uuid> = row.try_get("", "recovery_id")?;
  let mut recovery_status: String = row.try_get("", "recovery_status")?;
  let mut recovery_started_at: Option<i64> = row.try_get("", "recovery_started_at")?;
  let mut recovery_until: Option<i64> = row.try_get("", "recovery_until")?;
  let mut last_error_code: Option<String> = row.try_get("", "last_error_code")?;
  let state_name = state_name(state);
  let next_status = now.saturating_add(STATUS_INTERVAL_MS);

  if state == ConnectionState::Working {
    if disconnected_at.is_some() {
      if recovery_status != "running" && previous != "working" {
        recovery_id = Some(Uuid::now_v7());
        recovery_status = "running".to_owned();
        recovery_started_at = Some(now);
        recovery_until = Some(floor_second(now));
        last_error_code = None;
      } else if recovery_status == "running" && previous != "working" {
        let current_recovery = recovery_id
          .ok_or_else(|| DbErr::Custom("WhatsApp recovery cycle is unavailable".into()))?;
        extend_targets_after_reconnect(&tx, current_recovery, now).await?;
        recovery_until = Some(floor_second(now));
      }
    }
    resolve_attention(&tx, "whatsapp_disconnected", None, now).await?;
  } else {
    let disconnected_at = disconnected_at.unwrap_or(now);
    if recovery_status != "running" {
      recovery_status = "disconnected".to_owned();
    }
    last_error_code = if state == ConnectionState::Unavailable {
      Some("provider_status_unavailable".to_owned())
    } else {
      last_error_code
    };
    tx.execute(sql(
      "UPDATE provider_recovery_state SET provider_state=$2,disconnected_at=$3,recovery_status=$4,next_status_check_at=$5,last_error_code=$6,updated_at=$7 WHERE singleton=true AND claim_token=$1",
      vec![token.into(), state_name.into(), disconnected_at.into(), recovery_status.clone().into(), next_status.into(), last_error_code.clone().into(), now.into()],
    ))
    .await?;
    store::attention(&tx, None, None, "whatsapp_disconnected", now).await?;
    tx.commit().await?;
    return Ok(());
  }

  tx.execute(sql(
    "UPDATE provider_recovery_state SET provider_state=$2,disconnected_at=$3,recovery_id=$4,recovery_status=$5,recovery_started_at=$6,recovery_until=$7,next_status_check_at=$8,last_error_code=$9,updated_at=$10 WHERE singleton=true AND claim_token=$1",
    vec![token.into(), state_name.into(), disconnected_at.into(), recovery_id.into(), recovery_status.clone().into(), recovery_started_at.into(), recovery_until.into(), next_status.into(), last_error_code.clone().into(), now.into()],
  ))
  .await?;
  if state == ConnectionState::Working && recovery_status == "running" {
    discover_targets(
      &tx,
      token,
      recovery_id.ok_or_else(|| DbErr::Custom("WhatsApp recovery cycle is unavailable".into()))?,
      disconnected_at
        .ok_or_else(|| DbErr::Custom("WhatsApp interruption is unavailable".into()))?,
      recovery_started_at
        .ok_or_else(|| DbErr::Custom("WhatsApp recovery start is unavailable".into()))?,
      now,
    )
    .await?;
  }
  tx.commit().await?;
  Ok(())
}

fn state_name(state: ConnectionState) -> &'static str {
  match state {
    ConnectionState::Unavailable => "unavailable",
    ConnectionState::Stopped => "stopped",
    ConnectionState::Starting => "starting",
    ConnectionState::Qr => "qr",
    ConnectionState::PasskeyRequired => "passkey_required",
    ConnectionState::PasskeyConfirmationRequired => "passkey_confirmation_required",
    ConnectionState::Working => "working",
    ConnectionState::Failed => "failed",
    ConnectionState::Unknown => "unknown",
    ConnectionState::Unconfigured => "unknown",
  }
}

async fn extend_targets_after_reconnect(
  tx: &sea_orm::DatabaseTransaction,
  recovery_id: Uuid,
  now: i64,
) -> Result<(), DbErr> {
  let end = floor_second(now);
  tx.execute(sql(
    "UPDATE provider_recovery_targets SET pending_end=GREATEST(range_end,COALESCE(pending_end,0),$1),updated_at=$2 WHERE recovery_id=$3 AND state='pending'",
    vec![end.into(), now.into(), recovery_id.into()],
  ))
  .await?;
  tx.execute(sql(
    "UPDATE provider_recovery_targets SET range_start=GREATEST(range_end-$1,0),range_end=$2,page_offset=0,state='pending',attempts=0,available_at=$3,last_error_code=NULL,updated_at=$3 WHERE recovery_id=$4 AND state='complete' AND range_end<$2",
    vec![OVERLAP_MS.into(), end.into(), now.into(), recovery_id.into()],
  ))
  .await?;
  Ok(())
}

async fn discover_targets(
  tx: &sea_orm::DatabaseTransaction,
  token: Uuid,
  recovery_id: Uuid,
  disconnected_at: i64,
  recovery_started_at: i64,
  now: i64,
) -> Result<(), DbErr> {
  let candidates = tx
    .query_all(sql(
      "SELECT c.id,c.identity_conflict,a.jid,MAX(p.expired_at) FILTER (WHERE p.status='expired' AND p.expired_at >= $1) AS latest_expired_at FROM appointments p JOIN contacts c ON c.id=p.contact_id LEFT JOIN contact_aliases a ON a.contact_id=c.id WHERE (p.status IN ('requested','awaiting_payment','confirmed') AND p.starts_at>$2) OR (p.status='expired' AND p.expired_at >= $1 AND p.expired_at <= $2) GROUP BY c.id,c.identity_conflict,a.jid ORDER BY c.id,a.jid",
      vec![disconnected_at.into(), now.into()],
    ))
    .await?;
  let end = floor_second(now);
  let mut identity_missing = false;
  for candidate in candidates {
    let contact: Uuid = candidate.try_get("", "id")?;
    let conflicted: bool = candidate.try_get("", "identity_conflict")?;
    let chat: Option<String> = candidate.try_get("", "jid")?;
    let latest_expired_at: Option<i64> = candidate.try_get("", "latest_expired_at")?;
    if conflicted || chat.is_none() {
      identity_missing = true;
      store::attention(tx, None, Some(contact), "whatsapp_identity_missing", now).await?;
      tx.execute(sql(
        "UPDATE provider_recovery_state SET last_error_code='history_identity_missing' WHERE singleton=true AND claim_token=$1",
        vec![token.into()],
      ))
      .await?;
      continue;
    }
    let chat = chat.unwrap();
    resolve_attention(tx, "whatsapp_identity_missing", Some(contact), now).await?;
    let existing = tx
      .query_one(sql(
        "SELECT recovery_id,range_start,range_end,pending_end,included_expired_at,page_offset,media_gap,state,attempts,available_at FROM provider_recovery_targets WHERE contact_id=$1 AND chat_identity=$2 FOR UPDATE",
        vec![contact.into(), chat.clone().into()],
      ))
      .await?;
    if existing
      .as_ref()
      .is_none_or(|row| row.try_get::<Uuid>("", "recovery_id").ok() != Some(recovery_id))
    {
      let checkpoint = tx
        .query_one(sql(
          "SELECT covered_through FROM provider_recovery_checkpoints WHERE contact_id=$1 AND chat_identity=$2",
          vec![contact.into(), chat.clone().into()],
        ))
        .await?
        .map(|row| row.try_get::<i64>("", "covered_through"))
        .transpose()?;
      let persisted = if checkpoint.is_some() {
        checkpoint
      } else {
        tx.query_one(sql(
          "SELECT server_timestamp*1000 AS covered_through FROM inbox_messages WHERE (original_chat_identity=$1 OR resolved_phone_identity=$1) AND server_timestamp*1000 <= $2 ORDER BY intake_order DESC LIMIT 1",
          vec![chat.clone().into(), disconnected_at.into()],
        ))
        .await?
        .map(|row| row.try_get::<i64>("", "covered_through"))
        .transpose()?
      };
      let start = persisted
        .unwrap_or(disconnected_at)
        .saturating_sub(OVERLAP_MS)
        .max(0)
        .min(end);
      tx.execute(sql(
        "INSERT INTO provider_recovery_targets(contact_id,chat_identity,recovery_id,range_start,range_end,pending_end,included_expired_at,page_offset,media_gap,state,attempts,available_at,last_error_code,updated_at) VALUES ($1,$2,$3,$4,$5,NULL,$6,0,false,'pending',0,$7,NULL,$7) ON CONFLICT(contact_id,chat_identity) DO UPDATE SET recovery_id=EXCLUDED.recovery_id,range_start=EXCLUDED.range_start,range_end=EXCLUDED.range_end,pending_end=NULL,included_expired_at=EXCLUDED.included_expired_at,page_offset=0,media_gap=false,state='pending',attempts=0,available_at=EXCLUDED.available_at,last_error_code=NULL,updated_at=EXCLUDED.updated_at WHERE provider_recovery_targets.recovery_id<>EXCLUDED.recovery_id",
        vec![contact.into(), chat.clone().into(), recovery_id.into(), start.into(), end.into(), latest_expired_at.into(), now.into()],
      ))
      .await?;
      tx.execute(sql(
        "UPDATE provider_recovery_state SET recovery_until=GREATEST(COALESCE(recovery_until,0),$2) WHERE singleton=true AND claim_token=$1",
        vec![token.into(), end.into()],
      ))
      .await?;
      continue;
    }
    let row = existing.unwrap();
    let old_range_end: i64 = row.try_get("", "range_end")?;
    let old_pending_end: Option<i64> = row.try_get("", "pending_end")?;
    let old_expired: Option<i64> = row.try_get("", "included_expired_at")?;
    let state: String = row.try_get("", "state")?;
    if latest_expired_at.is_some_and(|expired| {
      expired > recovery_started_at && old_expired.is_none_or(|seen| expired > seen)
    }) {
      if state == "complete" && end > old_range_end {
        tx.execute(sql(
          "UPDATE provider_recovery_targets SET range_start=GREATEST(range_end-$3,0),range_end=$4,pending_end=NULL,included_expired_at=$5,page_offset=0,media_gap=false,state='pending',attempts=0,available_at=$6,last_error_code=NULL,updated_at=$6 WHERE contact_id=$1 AND chat_identity=$2",
          vec![contact.into(), chat.clone().into(), OVERLAP_MS.into(), end.into(), latest_expired_at.into(), now.into()],
        ))
        .await?;
      } else if state == "pending" {
        tx.execute(sql(
          "UPDATE provider_recovery_targets SET pending_end=GREATEST(range_end,COALESCE(pending_end,0),$3),included_expired_at=$4,updated_at=$5 WHERE contact_id=$1 AND chat_identity=$2",
          vec![contact.into(), chat.clone().into(), end.into(), latest_expired_at.into(), now.into()],
        ))
        .await?;
      }
      tx.execute(sql(
        "UPDATE provider_recovery_state SET recovery_until=GREATEST(COALESCE(recovery_until,0),$2) WHERE singleton=true AND claim_token=$1",
        vec![token.into(), end.into()],
      ))
      .await?;
    } else if old_pending_end.is_some_and(|pending| pending <= old_range_end) {
      tx.execute(sql(
        "UPDATE provider_recovery_targets SET pending_end=NULL WHERE contact_id=$1 AND chat_identity=$2",
        vec![contact.into(), chat.into()],
      ))
      .await?;
    }
  }
  if !identity_missing {
    tx.execute(sql(
      "UPDATE provider_recovery_state SET last_error_code=NULL WHERE singleton=true AND claim_token=$1 AND last_error_code='history_identity_missing'",
      vec![token.into()],
    ))
    .await?;
  }
  Ok(())
}

async fn recover_one_page(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: &Client,
  token: Uuid,
  now: i64,
) -> Result<bool, DbErr> {
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  let state = tx
    .query_one(sql(
      "SELECT recovery_id,recovery_status FROM provider_recovery_state WHERE singleton=true AND claim_token=$1",
      vec![token.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("WhatsApp recovery lease expired".into()))?;
  let recovery_id: Option<Uuid> = state.try_get("", "recovery_id")?;
  let recovery_status: String = state.try_get("", "recovery_status")?;
  if recovery_status != "running" || recovery_id.is_none() {
    tx.commit().await?;
    return Ok(false);
  }
  let recovery_id = recovery_id.unwrap();
  let target = tx
    .query_one(sql(
      "SELECT contact_id,chat_identity,range_start,range_end,pending_end,page_offset,media_gap,attempts FROM provider_recovery_targets WHERE recovery_id=$1 AND state='pending' AND available_at<=$2 ORDER BY available_at,contact_id,chat_identity LIMIT 1 FOR UPDATE SKIP LOCKED",
      vec![recovery_id.into(), now.into()],
    ))
    .await?;
  tx.commit().await?;

  let Some(target) = target else {
    finalize_cycle(db, token, recovery_id, now).await?;
    return Ok(false);
  };
  let contact: Uuid = target.try_get("", "contact_id")?;
  let chat: String = target.try_get("", "chat_identity")?;
  let range_start: i64 = target.try_get("", "range_start")?;
  let range_end: i64 = target.try_get("", "range_end")?;
  let offset: i64 = target.try_get("", "page_offset")?;
  let media_gap: bool = target.try_get("", "media_gap")?;
  let attempts: i32 = target.try_get("", "attempts")?;
  if offset > MAX_OFFSET {
    mark_incomplete(
      db,
      token,
      recovery_id,
      contact,
      &chat,
      "history_offset_limit",
      now,
    )
    .await?;
    finalize_cycle(db, token, recovery_id, now).await?;
    return Ok(true);
  }
  let messages = match client
    .messages(
      &chat,
      range_start.div_euclid(1000),
      range_end.div_euclid(1000),
      PAGE_LIMIT,
      offset as u32,
    )
    .await
  {
    Ok(messages) => messages,
    Err(_) => {
      retry_page(
        db,
        token,
        recovery_id,
        (contact, &chat),
        attempts,
        "history_page_unavailable",
        now,
      )
      .await?;
      return Ok(true);
    }
  };
  if invalid_page(&messages, &chat, range_start, range_end) {
    retry_page(
      db,
      token,
      recovery_id,
      (contact, &chat),
      attempts,
      "history_invalid_page",
      now,
    )
    .await?;
    return Ok(true);
  }

  let mut media_gap = media_gap;
  for message in &messages {
    let raw = history_envelope(message);
    let parsed = match inbox::incoming(&raw) {
      Ok(Some(parsed)) if parsed.id == message.id && parsed.original == chat => parsed,
      _ => {
        retry_page(
          db,
          token,
          recovery_id,
          (contact, &chat),
          attempts,
          "history_invalid_page",
          now,
        )
        .await?;
        return Ok(true);
      }
    };
    inbox::accept_history(db, parsed, raw.clone(), clock.now_millis()).await?;
    if message.has_media {
      match inbox::copy_pending_media(
        db,
        Some(client),
        &message.id,
        clock.now_millis(),
        cfg.public_origin.as_deref(),
      )
      .await
      {
        Ok(Some(crate::modules::payments::ReceiveOutcome::MediaUnavailable)) => media_gap = true,
        Ok(Some(_)) | Ok(None) => {}
        Err(ReceiptIntakeError::Database(error)) => return Err(error),
        Err(ReceiptIntakeError::Provider) => {
          retry_page(
            db,
            token,
            recovery_id,
            (contact, &chat),
            attempts,
            "history_media_unavailable",
            now,
          )
          .await?;
          return Ok(true);
        }
      }
      media_gap |= media_unavailable(db, &message.id).await?;
    }
  }

  let current = clock.now_millis();
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  let still_owned = tx
    .query_one(sql(
      "SELECT recovery_id FROM provider_recovery_state WHERE singleton=true AND claim_token=$1 FOR UPDATE",
      vec![token.into()],
    ))
    .await?
    .and_then(|row| row.try_get::<Option<Uuid>>("", "recovery_id").ok().flatten())
    == Some(recovery_id);
  if !still_owned {
    tx.rollback().await?;
    return Ok(true);
  }
  let next_offset = offset.saturating_add(messages.len() as i64);
  let drained = messages.len() < PAGE_LIMIT as usize;
  if drained {
    let target_state = tx
      .query_one(sql(
        "SELECT pending_end FROM provider_recovery_targets WHERE contact_id=$1 AND chat_identity=$2 AND recovery_id=$3 FOR UPDATE",
        vec![contact.into(), chat.clone().into(), recovery_id.into()],
      ))
      .await?
      .ok_or_else(|| DbErr::Custom("WhatsApp recovery target is unavailable".into()))?;
    let pending_end: Option<i64> = target_state.try_get("", "pending_end")?;
    if let Some(end) = pending_end.filter(|end| *end > range_end) {
      tx.execute(sql(
        "UPDATE provider_recovery_targets SET range_start=GREATEST(range_end-$4,0),range_end=$5,pending_end=NULL,page_offset=0,media_gap=$6,state='pending',attempts=0,available_at=$7,last_error_code=NULL,updated_at=$7 WHERE contact_id=$1 AND chat_identity=$2 AND recovery_id=$3",
        vec![contact.into(), chat.clone().into(), recovery_id.into(), OVERLAP_MS.into(), end.into(), media_gap.into(), current.into()],
      ))
      .await?;
    } else {
      tx.execute(sql(
        "UPDATE provider_recovery_targets SET page_offset=$4,media_gap=$5,state=$6,available_at=$7,last_error_code=$8,updated_at=$7 WHERE contact_id=$1 AND chat_identity=$2 AND recovery_id=$3",
        vec![contact.into(), chat.clone().into(), recovery_id.into(), next_offset.into(), media_gap.into(), if media_gap { "incomplete" } else { "complete" }.into(), current.into(), if media_gap { Some("history_media_unavailable") } else { None }.into()],
      ))
      .await?;
      if !media_gap {
        tx.execute(sql(
          "INSERT INTO provider_recovery_checkpoints(contact_id,chat_identity,covered_through,updated_at) VALUES ($1,$2,$3,$4) ON CONFLICT(contact_id,chat_identity) DO UPDATE SET covered_through=GREATEST(provider_recovery_checkpoints.covered_through,EXCLUDED.covered_through),updated_at=EXCLUDED.updated_at",
          vec![contact.into(), chat.clone().into(), range_end.into(), current.into()],
        ))
        .await?;
      }
      if media_gap {
        store::attention(
          &tx,
          None,
          Some(contact),
          "whatsapp_recovery_incomplete",
          current,
        )
        .await?;
      } else {
        resolve_contact_recovery_attention(&tx, recovery_id, contact, current).await?;
      }
    }
  } else {
    tx.execute(sql(
      "UPDATE provider_recovery_targets SET page_offset=$4,media_gap=$5,available_at=$6,last_error_code=NULL,updated_at=$6 WHERE contact_id=$1 AND chat_identity=$2 AND recovery_id=$3",
      vec![contact.into(), chat.clone().into(), recovery_id.into(), next_offset.into(), media_gap.into(), current.into()],
    ))
    .await?;
  }
  tx.commit().await?;
  finalize_cycle(db, token, recovery_id, current).await?;
  Ok(true)
}

fn invalid_page(messages: &[HistoryMessage], chat: &str, start: i64, end: i64) -> bool {
  messages.len() > PAGE_LIMIT as usize
    || messages.iter().any(|message| {
      message.from != chat
        || message.from_me
        || message.to.is_some()
        || message.timestamp < start.div_euclid(1000)
        || message.timestamp > end.div_euclid(1000)
        || !super::messaging::parse_message_id(&message.id)
          .is_some_and(|id| !id.from_me && id.peer == chat)
    })
    || messages
      .windows(2)
      .any(|pair| pair[0].timestamp > pair[1].timestamp)
}

fn history_envelope(message: &HistoryMessage) -> Vec<u8> {
  let mut payload = Map::new();
  payload.insert("id".into(), Value::String(message.id.clone()));
  payload.insert("timestamp".into(), Value::from(message.timestamp));
  payload.insert("from".into(), Value::String(message.from.clone()));
  payload.insert("fromMe".into(), Value::Bool(false));
  payload.insert("to".into(), Value::Null);
  payload.insert("hasMedia".into(), Value::Bool(message.has_media));
  if let Some(body) = &message.body {
    payload.insert("body".into(), Value::String(body.clone()));
  }
  if message.has_media || message.media.is_some() {
    let mut media = message
      .media
      .as_ref()
      .and_then(Value::as_object)
      .cloned()
      .unwrap_or_default();
    if !media.contains_key("url") {
      if let Some(url) = &message.media_url {
        media.insert("url".into(), Value::String(url.clone()));
      }
    }
    payload.insert("media".into(), Value::Object(media));
  }
  if let Some(phone) = &message.resolved_phone {
    payload.insert(
      "_data".into(),
      serde_json::json!({"Info":{"SenderAlt":phone}}),
    );
  }
  serde_json::to_vec(&serde_json::json!({
    "event":"message",
    "session":"default",
    "engine":"GOWS",
    "payload":Value::Object(payload),
  }))
  .expect("serializing a JSON value cannot fail")
}

async fn media_unavailable(db: &Db, id: &str) -> Result<bool, DbErr> {
  Ok(db
    .conn
    .query_one(sql(
      "SELECT 1 FROM receipt_intake_state WHERE inbox_message_id=$1 AND state='complete' AND last_error_code='media_unavailable'",
      vec![id.into()],
    ))
    .await?
    .is_some())
}

async fn retry_page(
  db: &Db,
  token: Uuid,
  recovery_id: Uuid,
  target: (Uuid, &str),
  attempts: i32,
  code: &str,
  now: i64,
) -> Result<(), DbErr> {
  let (contact, chat) = target;
  let delay = 5_000i64
    .saturating_mul(1i64 << attempts.clamp(0, 6))
    .min(300_000);
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  tx.execute(sql(
  "UPDATE provider_recovery_targets SET attempts=LEAST(attempts+1,2147483647),available_at=$4,last_error_code=$5,updated_at=$6 WHERE contact_id=$1 AND chat_identity=$2 AND recovery_id=$3 AND state='pending'",
  vec![contact.into(), chat.into(), recovery_id.into(), now.saturating_add(delay).into(), code.into(), now.into()],
))
.await?;
  tx.execute(sql(
  "UPDATE provider_recovery_state SET last_error_code=$2,updated_at=$3 WHERE singleton=true AND claim_token=$1",
  vec![token.into(), code.into(), now.into()],
))
.await?;
  store::attention(
    &tx,
    None,
    Some(contact),
    "whatsapp_recovery_incomplete",
    now,
  )
  .await?;
  tx.commit().await?;
  Ok(())
}

async fn mark_incomplete(
  db: &Db,
  token: Uuid,
  recovery_id: Uuid,
  contact: Uuid,
  chat: &str,
  code: &str,
  now: i64,
) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  tx.execute(sql(
    "UPDATE provider_recovery_targets SET state='incomplete',last_error_code=$4,updated_at=$5 WHERE contact_id=$1 AND chat_identity=$2 AND recovery_id=$3",
    vec![contact.into(), chat.into(), recovery_id.into(), code.into(), now.into()],
  ))
  .await?;
  tx.execute(sql(
    "UPDATE provider_recovery_state SET last_error_code=$2,updated_at=$3 WHERE singleton=true AND claim_token=$1",
    vec![token.into(), code.into(), now.into()],
  ))
  .await?;
  store::attention(
    &tx,
    None,
    Some(contact),
    "whatsapp_recovery_incomplete",
    now,
  )
  .await?;
  tx.commit().await?;
  Ok(())
}

async fn finalize_cycle(db: &Db, token: Uuid, recovery_id: Uuid, now: i64) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  let state = tx
    .query_one(sql(
      "SELECT recovery_until,last_error_code FROM provider_recovery_state WHERE singleton=true AND claim_token=$1 AND recovery_id=$2 AND recovery_status='running' FOR UPDATE",
      vec![token.into(), recovery_id.into()],
    ))
    .await?;
  let Some(state) = state else {
    tx.commit().await?;
    return Ok(());
  };
  let pending: i64 = tx
    .query_one(sql(
      "SELECT count(*) AS count FROM provider_recovery_targets WHERE recovery_id=$1 AND state='pending'",
      vec![recovery_id.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("WhatsApp recovery targets are unavailable".into()))?
    .try_get("", "count")?;
  if pending > 0 {
    tx.commit().await?;
    return Ok(());
  }
  let recovery_until: Option<i64> = state.try_get("", "recovery_until")?;
  if recovery_until.is_some_and(|until| now < until.saturating_add(STATUS_INTERVAL_MS)) {
    tx.commit().await?;
    return Ok(());
  }
  let incomplete: i64 = tx
    .query_one(sql(
      "SELECT count(*) AS count FROM provider_recovery_targets WHERE recovery_id=$1 AND state='incomplete'",
      vec![recovery_id.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("WhatsApp recovery targets are unavailable".into()))?
    .try_get("", "count")?;
  let last_error: Option<String> = state.try_get("", "last_error_code")?;
  if incomplete > 0 || last_error.as_deref() == Some("history_identity_missing") {
    tx.execute(sql(
      "UPDATE provider_recovery_state SET recovery_status='incomplete',updated_at=$3 WHERE singleton=true AND claim_token=$1 AND recovery_id=$2",
      vec![token.into(), recovery_id.into(), now.into()],
    ))
    .await?;
  } else {
    tx.execute(sql(
      "UPDATE provider_recovery_state SET recovery_status='complete',disconnected_at=NULL,last_error_code=NULL,updated_at=$3 WHERE singleton=true AND claim_token=$1 AND recovery_id=$2",
      vec![token.into(), recovery_id.into(), now.into()],
    ))
    .await?;
    resolve_attention(&tx, "whatsapp_disconnected", None, now).await?;
  }
  tx.commit().await?;
  Ok(())
}

async fn resolve_contact_recovery_attention(
  tx: &sea_orm::DatabaseTransaction,
  recovery_id: Uuid,
  contact: Uuid,
  now: i64,
) -> Result<(), DbErr> {
  let remaining = tx
    .query_one(sql(
      "SELECT 1 FROM provider_recovery_targets WHERE recovery_id=$1 AND contact_id=$2 AND state<>'complete' LIMIT 1",
      vec![recovery_id.into(), contact.into()],
    ))
    .await?
    .is_some();
  if !remaining {
    resolve_attention(tx, "whatsapp_recovery_incomplete", Some(contact), now).await?;
  }
  Ok(())
}

async fn resolve_attention(
  tx: &sea_orm::DatabaseTransaction,
  kind: &str,
  contact: Option<Uuid>,
  now: i64,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE attention_items SET resolved_at=$3 WHERE kind=$1 AND contact_id IS NOT DISTINCT FROM $2 AND resolved_at IS NULL",
    vec![kind.into(), contact.into(), now.into()],
  ))
  .await?;
  Ok(())
}

async fn release(db: &Db, token: Uuid) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  tx.execute(sql(
    "UPDATE provider_recovery_state SET claim_token=NULL,claim_until=NULL WHERE singleton=true AND claim_token=$1",
    vec![token.into()],
  ))
  .await?;
  tx.commit().await?;
  Ok(())
}

fn floor_second(now: i64) -> i64 {
  now.div_euclid(1000).saturating_mul(1000)
}
