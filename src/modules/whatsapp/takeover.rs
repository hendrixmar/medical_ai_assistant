use super::{
  inbox::{self, sql},
  messaging::{parse_message_id, MessageId},
};
use crate::{database::Db, modules::appointments::store};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const TAKEOVER_MS: i64 = 12 * 60 * 60 * 1000;
const MAX_BODY: usize = 1024 * 1024;

struct PhoneEvent {
  fingerprint: Vec<u8>,
  timestamp: Option<i64>,
  body: Option<String>,
  parsed_id: Option<MessageId>,
  peer: Option<String>,
  recipient_phone: Option<String>,
  unavailable_reason: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
  Ignored,
  Accepted,
  Duplicate,
}

pub async fn active<C: ConnectionTrait>(conn: &C, contact: Uuid, now: i64) -> Result<bool, DbErr> {
  Ok(
    conn
      .query_one(sql(
        "SELECT id FROM contacts WHERE id=$1 AND takeover_until>$2",
        vec![contact.into(), now.into()],
      ))
      .await?
      .is_some(),
  )
}

pub async fn end(tx: &DatabaseTransaction, contact: Uuid, _now: i64) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE contacts SET takeover_until=NULL WHERE id=$1",
    vec![contact.into()],
  ))
  .await?;
  Ok(())
}

pub(crate) async fn accept_phone_event(
  db: &Db,
  raw: &[u8],
  enabled: bool,
  now: i64,
) -> Result<Option<Outcome>, DbErr> {
  let Some(mut event) = phone_event(raw) else {
    return Ok(None);
  };
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;

  let contact = if event.unavailable_reason.is_none() {
    let peer = event.peer.as_deref().unwrap_or_default();
    let row = tx
      .query_one(sql(
        "SELECT a.contact_id,c.identity_conflict FROM contact_aliases a JOIN contacts c ON c.id=a.contact_id WHERE a.jid=$1",
        vec![peer.into()],
      ))
      .await?;
    match row {
      None => {
        event.unavailable_reason = Some("unknown_chat");
        None
      }
      Some(row) if row.try_get::<bool>("", "identity_conflict")? => {
        event.unavailable_reason = Some("identity_conflict");
        None
      }
      Some(row) => {
        let contact: Uuid = row.try_get("", "contact_id")?;
        if let Some(phone) = &event.recipient_phone {
          let trusted = tx
            .query_one(sql(
              "SELECT jid FROM contact_aliases WHERE contact_id=$1 AND jid=$2",
              vec![contact.into(), phone.clone().into()],
            ))
            .await?
            .is_some();
          if !trusted {
            event.unavailable_reason = Some("recipient_mismatch");
            None
          } else {
            Some(contact)
          }
        } else {
          Some(contact)
        }
      }
    }
  } else {
    None
  };

  if let (Some(contact), Some(parsed)) = (contact, event.parsed_id.as_ref()) {
    // GOWS phone echoes may arrive before the API response; match only the same Contact.
    let app_send = tx
      .query_one(sql(
        "SELECT id FROM outbox_messages WHERE contact_id=$3 AND (raw_provider_id=$1 OR returned_id=$2) LIMIT 1",
        vec![parsed.raw.clone().into(), parsed.serialized.clone().into(), contact.into()],
      ))
      .await?
      .is_some();
    if app_send {
      tx.commit().await?;
      return Ok(Some(Outcome::Ignored));
    }
    if enabled {
      let inserted = tx
        .execute(sql(
          "INSERT INTO phone_takeover_events(message_fingerprint,contact_id,attribution_status,unavailable_reason,server_timestamp,body,created_at) VALUES ($1,$2,'attributed',NULL,$3,$4,$5) ON CONFLICT DO NOTHING",
          vec![event.fingerprint.clone().into(), contact.into(), event.timestamp.into(), event.body.clone().into(), now.into()],
        ))
        .await?
        .rows_affected()
        == 1;
      if inserted {
        tx.execute(sql(
          "UPDATE contacts SET takeover_until=GREATEST(COALESCE(takeover_until,0),$2) WHERE id=$1",
          vec![contact.into(), now.saturating_add(TAKEOVER_MS).into()],
        ))
        .await?;
      }
      tx.commit().await?;
      return Ok(Some(if inserted {
        Outcome::Accepted
      } else {
        Outcome::Duplicate
      }));
    }
    event.unavailable_reason = Some("operator_disabled");
  }

  let reason = event.unavailable_reason.unwrap_or("unknown_chat");
  let inserted = tx
    .execute(sql(
      "INSERT INTO phone_takeover_events(message_fingerprint,contact_id,attribution_status,unavailable_reason,server_timestamp,body,created_at) VALUES ($1,NULL,'unavailable',$2,$3,NULL,$4) ON CONFLICT DO NOTHING",
      vec![event.fingerprint.into(), reason.into(), event.timestamp.into(), now.into()],
    ))
    .await?
    .rows_affected()
    == 1;
  if inserted && reason != "operator_disabled" {
    store::attention(&tx, None, None, "whatsapp_identity_missing", now).await?;
  }
  tx.commit().await?;
  Ok(Some(Outcome::Ignored))
}

fn phone_event(raw: &[u8]) -> Option<PhoneEvent> {
  let envelope: Value = serde_json::from_slice(raw).ok()?;
  if envelope["event"].as_str()? != "message"
    || envelope["session"].as_str()? != "default"
    || envelope["engine"].as_str()? != "GOWS"
  {
    return None;
  }
  let payload = envelope.get("payload")?;
  if !payload.get("fromMe")?.as_bool()? || payload.get("source")?.as_str()? != "app" {
    return None;
  }
  let id = payload
    .get("id")
    .and_then(Value::as_str)
    .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control));
  let fingerprint = id
    .map(|id| Sha256::digest(id.as_bytes()).to_vec())
    .unwrap_or_else(|| Sha256::digest(raw).to_vec());
  let mut unavailable_reason = id.is_none().then_some("invalid_message");
  let timestamp = payload
    .get("timestamp")
    .and_then(Value::as_i64)
    .filter(|timestamp| (0..=253_402_300_799).contains(timestamp));
  if timestamp.is_none() {
    unavailable_reason = Some("invalid_message");
  }
  let body = match payload.get("body") {
    None | Some(Value::Null) => None,
    Some(Value::String(body)) if body.len() <= MAX_BODY && !body.contains('\0') => {
      Some(body.clone())
    }
    _ => {
      unavailable_reason = Some("invalid_message");
      None
    }
  };

  // Pinned GOWS direct messages use Info.Chat for `from` and set `to` to null.
  if !payload.get("to").is_some_and(Value::is_null) {
    unavailable_reason.get_or_insert("wrong_chat");
  }
  let peer = payload
    .get("from")
    .and_then(Value::as_str)
    .map(str::to_owned);
  let parsed_id = id.and_then(parse_message_id);
  let info = payload.pointer("/_data/Info");
  let info_matches = parsed_id.as_ref().is_some_and(|parsed| {
    parsed.from_me
      && peer.as_deref() == Some(parsed.peer.as_str())
      && info.is_some_and(|info| {
        info.get("IsFromMe").and_then(Value::as_bool) == Some(true)
          && info.get("IsGroup").and_then(Value::as_bool) == Some(false)
          && info.get("Chat").and_then(Value::as_str) == Some(parsed.peer.as_str())
          && info.get("ID").and_then(Value::as_str) == Some(parsed.raw.as_str())
      })
  });
  if !info_matches {
    unavailable_reason.get_or_insert("wrong_chat");
  }
  let recipient_phone = match info.and_then(|info| info.get("RecipientAlt")) {
    None | Some(Value::Null) => None,
    Some(Value::String(value)) => match inbox::phone_identity(value) {
      Some(phone) => Some(phone),
      None => {
        unavailable_reason.get_or_insert("recipient_mismatch");
        None
      }
    },
    Some(_) => {
      unavailable_reason.get_or_insert("recipient_mismatch");
      None
    }
  };
  Some(PhoneEvent {
    fingerprint,
    timestamp,
    body,
    parsed_id: if info_matches { parsed_id } else { None },
    peer: if info_matches { peer } else { None },
    recipient_phone,
    unavailable_reason,
  })
}
