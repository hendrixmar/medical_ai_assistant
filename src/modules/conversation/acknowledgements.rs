use super::dispatch;
use crate::{
  database::Db,
  modules::{
    doctor::Clock,
    whatsapp::{bound_transaction, inbox::sql, messaging::parse_message_id},
  },
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

pub(crate) struct Ack {
  envelope: String,
  raw: String,
  exact: String,
  peer: String,
  ack: i32,
}
pub(crate) fn parse(value: &Value) -> Result<Option<Ack>, ()> {
  if value["event"].as_str().ok_or(())? != "message.ack"
    || value["session"].as_str().ok_or(())? != "default"
    || value["engine"].as_str().ok_or(())? != "GOWS"
  {
    return Ok(None);
  }
  let p = &value["payload"];
  if !p["fromMe"].as_bool().ok_or(())? {
    return Ok(None);
  }
  let peer = p["from"].as_str().ok_or(())?;
  if peer.ends_with("@g.us") || peer.ends_with("@broadcast") || peer.ends_with("@newsletter") {
    return Ok(None);
  }
  let id = parse_message_id(p["id"].as_str().ok_or(())?).ok_or(())?;
  if !id.from_me || id.peer != peer || !p["to"].is_null() || !p["participant"].is_null() {
    return Err(());
  }
  let ack = p["ack"]
    .as_i64()
    .filter(|n| (-1..=4).contains(n))
    .ok_or(())? as i32;
  if let Some(name) = p.get("ackName") {
    let expected = match ack {
      -1 => "ERROR",
      0 => "PENDING",
      1 => "SERVER",
      2 => "DEVICE",
      3 => "READ",
      _ => "PLAYED",
    };
    if name.as_str() != Some(expected) {
      return Err(());
    }
  }
  let envelope = value["id"]
    .as_str()
    .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
    .ok_or(())?;
  Ok(Some(Ack {
    envelope: envelope.into(),
    raw: id.raw,
    exact: id.serialized,
    peer: peer.into(),
    ack,
  }))
}

// Lookup without locks, then Contact -> outbox. Never lock evidence first.
async fn candidate(tx: &DatabaseTransaction, raw: &str) -> Result<Option<(Uuid, Uuid)>, DbErr> {
  let row = tx
    .query_one(sql(
      "SELECT id,contact_id FROM outbox_messages WHERE raw_provider_id=$1",
      vec![raw.into()],
    ))
    .await?;
  let Some(row) = row else {
    return Ok(None);
  };
  let id: Uuid = row.try_get("", "id")?;
  let contact: Uuid = row.try_get("", "contact_id")?;
  let row = tx
    .query_one(sql(
      "SELECT identity_conflict FROM contacts WHERE id=$1 FOR UPDATE",
      vec![contact.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Contact unavailable".into()))?;
  let conflict: bool = row.try_get("", "identity_conflict")?;
  let row = tx
    .query_one(sql(
      "SELECT dispatch_committed_at,raw_provider_id FROM outbox_messages WHERE id=$1 FOR UPDATE",
      vec![id.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Outbox unavailable".into()))?;
  if conflict
    || row
      .try_get::<Option<i64>>("", "dispatch_committed_at")?
      .is_none()
    || row
      .try_get::<Option<String>>("", "raw_provider_id")?
      .as_deref()
      != Some(raw)
  {
    return Ok(None);
  }
  Ok(Some((contact, id)))
}
async fn project(
  tx: &DatabaseTransaction,
  contact: Uuid,
  outbox: Uuid,
  evidence: Uuid,
  ack: i32,
  now: i64,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE outgoing_ack_evidence SET matched_outbox_id=$2 WHERE id=$1",
    vec![evidence.into(), outbox.into()],
  ))
  .await?;
  tx.execute(sql("UPDATE outbox_messages SET positive_ack=GREATEST(positive_ack,$2),ack_error=ack_error OR $2=-1,state=CASE WHEN $2>0 THEN 'sent' ELSE state END WHERE id=$1",vec![outbox.into(),ack.into()])).await?;
  if ack > 0 {
    tx.execute(sql("UPDATE contacts SET privacy_first_sent_at=COALESCE(privacy_first_sent_at,$2) WHERE id=$1 AND EXISTS (SELECT FROM outbox_messages WHERE id=$3 AND privacy_prefix)",vec![contact.into(),now.into(),outbox.into()])).await?;
  }
  Ok(())
}
pub(crate) async fn accept(db: &Db, ack: Ack, now: i64) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let candidate = candidate(&tx, &ack.raw).await?;
  let id = Uuid::now_v7();
  let inserted=tx.execute(sql("INSERT INTO outgoing_ack_evidence(id,envelope_id,session,raw_provider_id,exact_id,peer,from_me,ack,arrived_at) VALUES ($1,$2,'default',$3,$4,$5,true,$6,$7) ON CONFLICT DO NOTHING",vec![id.into(),ack.envelope.into(),ack.raw.into(),ack.exact.into(),ack.peer.clone().into(),ack.ack.into(),now.into()])).await?.rows_affected()==1;
  if inserted {
    if let Some((contact, outbox)) = candidate {
      if dispatch::trusted(&tx, contact, &ack.peer).await? {
        project(&tx, contact, outbox, id, ack.ack, now).await?;
      }
    }
  }
  tx.commit().await?;
  Ok(())
}

/// Bounded maintenance reconciles authenticated early/alias evidence, not provider history.
pub async fn reconcile(db: &Db, clock: &dyn Clock) -> Result<usize, DbErr> {
  let rows=db.conn.query_all(sql("SELECT e.id,e.raw_provider_id FROM outgoing_ack_evidence e JOIN outbox_messages o ON o.raw_provider_id=e.raw_provider_id JOIN contacts c ON c.id=o.contact_id JOIN contact_aliases a ON a.contact_id=c.id AND a.jid=e.peer WHERE e.matched_outbox_id IS NULL AND o.dispatch_committed_at IS NOT NULL AND NOT c.identity_conflict ORDER BY e.arrived_at,e.id LIMIT 25",vec![])).await?;
  let mut matched = 0;
  for row in rows {
    let id: Uuid = row.try_get("", "id")?;
    let raw: String = row.try_get("", "raw_provider_id")?;
    let tx = db.conn.begin().await?;
    bound_transaction(&tx).await?;
    if let Some((contact, outbox)) = candidate(&tx, &raw).await? {
      if let Some(e)=tx.query_one(sql("SELECT peer,ack FROM outgoing_ack_evidence WHERE id=$1 AND matched_outbox_id IS NULL FOR UPDATE",vec![id.into()])).await? {
        if dispatch::trusted(&tx,contact,&e.try_get::<String>("","peer")?).await? {
          project(&tx,contact,outbox,id,e.try_get("","ack")?,clock.now_millis()).await?;matched+=1;
        }
      }
    }
    tx.commit().await?;
  }
  Ok(matched)
}
