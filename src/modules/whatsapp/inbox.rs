use crate::database::Db;
use crate::modules::payments::intake::PendingReceipt;
use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement, TransactionTrait, Value};
use serde_json::Value as Json;
use sha2::Sha512;

pub(super) const MAX_BODY: usize = 2 * 1024 * 1024;
pub(crate) fn sql(query: &str, values: Vec<Value>) -> Statement {
  Statement::from_sql_and_values(DbBackend::Postgres, query, values)
}

pub(super) fn authenticate(headers: &HeaderMap, raw: &[u8], key: &str) -> bool {
  let signatures = headers.get_all("x-webhook-hmac");
  if signatures.iter().count() != 1 {
    return false;
  }
  let Some(signature) = signatures.iter().next().and_then(|h| h.to_str().ok()) else {
    return false;
  };
  let algorithms = headers.get_all("x-webhook-hmac-algorithm");
  if algorithms.iter().count() > 1 || algorithms.iter().any(|v| v != "sha512") {
    return false;
  }
  if signature.len() != 128
    || !signature
      .bytes()
      .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
  {
    return false;
  }
  let mut decoded = [0u8; 64];
  for (i, pair) in signature.as_bytes().chunks_exact(2).enumerate() {
    let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
    decoded[i] = digit(pair[0]) * 16 + digit(pair[1]);
  }
  let Ok(mut mac) = Hmac::<Sha512>::new_from_slice(key.as_bytes()) else {
    return false;
  };
  mac.update(raw);
  mac.verify_slice(&decoded).is_ok()
}

pub(super) struct Incoming {
  pub id: String,
  pub original: String,
  pub phone: Option<String>,
  pub timestamp: i64,
}
pub(crate) fn phone_identity(value: &str) -> Option<String> {
  let digits = value
    .strip_suffix("@c.us")
    .or_else(|| value.strip_suffix("@s.whatsapp.net"))?;
  valid_digits(digits, 20).then(|| format!("{digits}@c.us"))
}
fn valid_digits(value: &str, max: usize) -> bool {
  !value.is_empty() && value.len() <= max && value.bytes().all(|b| b.is_ascii_digit())
}

/// None is an authenticated but out-of-scope event, never a processing job.
pub(super) fn incoming(raw: &[u8]) -> Result<Option<Incoming>, ()> {
  let envelope: Json = serde_json::from_slice(raw).map_err(|_| ())?;
  let event = envelope["event"].as_str().ok_or(())?;
  let session = envelope["session"].as_str().ok_or(())?;
  let engine = envelope["engine"].as_str().ok_or(())?;
  if event != "message" || session != "default" || engine != "GOWS" {
    return Ok(None);
  }
  let payload = &envelope["payload"];
  if payload["fromMe"].as_bool().ok_or(())? {
    return Ok(None);
  }
  let original = payload["from"].as_str().ok_or(())?;
  if original.ends_with("@g.us")
    || original.ends_with("@broadcast")
    || original.ends_with("@newsletter")
  {
    return Ok(None);
  }
  let digits = original
    .strip_suffix("@c.us")
    .or_else(|| original.strip_suffix("@lid"))
    .ok_or(())?;
  if !valid_digits(digits, if original.ends_with("@c.us") { 20 } else { 32 }) {
    return Err(());
  }
  let id = payload["id"].as_str().ok_or(())?;
  if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
    return Err(());
  }
  let timestamp = payload["timestamp"]
    .as_i64()
    .filter(|t| (0..=253402300799).contains(t))
    .ok_or(())?;
  // Validate optional known fields without discarding unfamiliar message content.
  canonical(&envelope)?;
  let phone = phone_identity(original).or_else(|| {
    payload
      .pointer("/_data/Info/SenderAlt")
      .and_then(Json::as_str)
      .and_then(phone_identity)
  });
  Ok(Some(Incoming {
    id: id.to_owned(),
    original: original.to_owned(),
    phone,
    timestamp,
  }))
}

pub(super) struct Canonical {
  pub kind: &'static str,
  pub body: Option<String>,
  pub media: Option<Json>,
}
pub(super) fn canonical(envelope: &Json) -> Result<Canonical, ()> {
  let payload = envelope
    .get("payload")
    .and_then(Json::as_object)
    .ok_or(())?;
  let body = match payload.get("body") {
    None | Some(Json::Null) => None,
    Some(Json::String(body)) if body.len() <= 1024 * 1024 && !body.contains('\0') => {
      Some(body.clone())
    }
    _ => return Err(()),
  };
  let has_media = match payload.get("hasMedia") {
    None => false,
    Some(Json::Bool(value)) => *value,
    _ => return Err(()),
  };
  let media = match payload.get("media") {
    None | Some(Json::Null) => None,
    Some(Json::Object(object)) => {
      let mut projected = serde_json::Map::new();
      // WAMedia in pinned src/structures/media.dto.ts. Never fetch its URL.
      for (key, max) in [("url", 8192), ("mimetype", 256), ("filename", 1024)] {
        match object.get(key) {
          None | Some(Json::Null) => {}
          Some(Json::String(value)) if value.len() <= max && !value.contains('\0') => {
            projected.insert(key.into(), Json::String(value.clone()));
          }
          _ => return Err(()),
        }
      }
      Some(Json::Object(projected))
    }
    _ => return Err(()),
  };
  let kind = if has_media {
    "media"
  } else if body.as_ref().is_some_and(|s| !s.is_empty()) {
    "text"
  } else {
    "other"
  };
  Ok(Canonical { kind, body, media })
}

pub(super) async fn accept(
  db: &Db,
  message: Incoming,
  raw: Vec<u8>,
  now_ms: i64,
) -> Result<bool, DbErr> {
  accept_from_source(db, message, raw, now_ms, "webhook").await
}

pub(super) async fn accept_history(
  db: &Db,
  message: Incoming,
  raw: Vec<u8>,
  now_ms: i64,
) -> Result<bool, DbErr> {
  accept_from_source(db, message, raw, now_ms, "provider_history").await
}

async fn accept_from_source(
  db: &Db,
  message: Incoming,
  raw: Vec<u8>,
  now_ms: i64,
  source: &str,
) -> Result<bool, DbErr> {
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  let id = message.id.clone();
  let inserted = tx
    .query_one(sql("INSERT INTO inbox_messages (message_id,original_chat_identity,resolved_phone_identity,server_timestamp,arrived_at,raw_envelope,intake_source) VALUES ($1,$2,$3,$4,to_timestamp($5::double precision/1000),$6,$7) ON CONFLICT DO NOTHING RETURNING message_id",
      vec![message.id.into(), message.original.into(), message.phone.into(), message.timestamp.into(), now_ms.into(), raw.into(), source.into()]))
    .await?
    .is_some();
  let stored = tx
    .query_one(sql(
      "SELECT original_chat_identity,resolved_phone_identity,server_timestamp,raw_envelope,floor(extract(epoch FROM arrived_at)*1000)::bigint AS arrived_ms FROM inbox_messages WHERE message_id=$1",
      vec![id.clone().into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Accepted inbox message is unavailable".into()))?;
  let original: String = stored.try_get("", "original_chat_identity")?;
  let phone: Option<String> = stored.try_get("", "resolved_phone_identity")?;
  let sent_at = stored
    .try_get::<i64>("", "server_timestamp")?
    .saturating_mul(1000);
  let arrived_at: i64 = stored.try_get("", "arrived_ms")?;
  let stored_raw: Vec<u8> = stored.try_get("", "raw_envelope")?;
  let envelope: Json = serde_json::from_slice(&stored_raw)
    .map_err(|_| DbErr::Custom("Stored inbox message is invalid".into()))?;
  if canonical(&envelope)
    .map_err(|_| DbErr::Custom("Stored inbox message is invalid".into()))?
    .kind
    == "media"
    && matches!(
      crate::modules::conversation::templates::classify(&envelope),
      Some("unsupported")
    )
  {
    crate::modules::conversation::identity::assign(&tx, &id, &original, phone.as_deref(), now_ms)
      .await?;
    tx.execute(sql(
      "INSERT INTO receipt_intake_state(inbox_message_id,state,sent_at,arrived_at,updated_at,available_at) VALUES ($1,'pending',$2,$3,$4,$4) ON CONFLICT DO NOTHING",
      vec![id.clone().into(), sent_at.into(), arrived_at.into(), now_ms.into()],
    ))
    .await?;
  }
  if inserted {
    tx.execute(sql(
      "INSERT INTO processing_jobs (inbox_message_id,kind,available_at) VALUES ($1,'prepare_inbox',to_timestamp($2))",
      vec![id.into(), now_ms.div_euclid(1000).into()],
    ))
    .await?;
  }
  tx.commit().await?;
  Ok(inserted)
}

pub(crate) enum ReceiptIntakeError {
  Database(DbErr),
  Provider,
}
impl From<DbErr> for ReceiptIntakeError {
  fn from(error: DbErr) -> Self {
    Self::Database(error)
  }
}

pub(crate) async fn copy_pending_media(
  db: &Db,
  client: Option<&super::client::Client>,
  inbox_id: &str,
  now_ms: i64,
  origin: Option<&str>,
) -> Result<Option<crate::modules::payments::ReceiveOutcome>, ReceiptIntakeError> {
  let Some(receipt) = pending_receipt(db, inbox_id).await? else {
    return Ok(None);
  };
  let outcome = crate::modules::payments::receive(db, client, inbox_id, &receipt, now_ms, origin)
    .await
    .map_err(|_| ReceiptIntakeError::Provider)?;
  Ok(Some(outcome))
}

pub(crate) async fn retry_pending_media(
  db: &Db,
  client: Option<&super::client::Client>,
  now_ms: i64,
  origin: Option<&str>,
) -> Result<bool, ReceiptIntakeError> {
  if origin.is_none() {
    return Ok(false);
  }
  let row = db
    .conn
    .query_one(sql(
      "SELECT inbox_message_id FROM receipt_intake_state WHERE state='pending' AND available_at <= $1 ORDER BY available_at,inbox_message_id LIMIT 1",
      vec![now_ms.into()],
    ))
    .await?;
  let Some(row) = row else {
    return Ok(false);
  };
  let id: String = row.try_get("", "inbox_message_id")?;
  Ok(
    copy_pending_media(db, client, &id, now_ms, origin)
      .await?
      .is_some(),
  )
}

async fn pending_receipt(db: &Db, id: &str) -> Result<Option<PendingReceipt>, DbErr> {
  let row = db
    .conn
    .query_one(sql(
      "SELECT i.contact_id,i.raw_envelope,r.sent_at,r.arrived_at FROM inbox_messages i JOIN receipt_intake_state r ON r.inbox_message_id=i.message_id WHERE i.message_id=$1 AND r.state='pending'",
      vec![id.into()],
    ))
    .await?;
  let Some(row) = row else {
    return Ok(None);
  };
  let contact: Option<uuid::Uuid> = row.try_get("", "contact_id")?;
  let raw: Vec<u8> = row.try_get("", "raw_envelope")?;
  let sent_at: i64 = row.try_get("", "sent_at")?;
  let arrived_at: i64 = row.try_get("", "arrived_at")?;
  let envelope: Json = serde_json::from_slice(&raw)
    .map_err(|_| DbErr::Custom("Stored inbox message is invalid".into()))?;
  let media = envelope.pointer("/payload/media");
  Ok(Some(PendingReceipt {
    contact,
    url: media
      .and_then(|media| media.get("url"))
      .and_then(Json::as_str)
      .map(str::to_owned),
    mimetype: media
      .and_then(|media| media.get("mimetype"))
      .and_then(Json::as_str)
      .map(str::to_owned),
    sent_at,
    arrived_at,
  }))
}
