use super::dispatch::{self, Claim};
use crate::{
  common::config::Config,
  database::Db,
  modules::{
    doctor::Clock,
    whatsapp::{
      bound_transaction,
      client::Client,
      inbox::sql,
      messaging::{MessageId, SendError},
    },
  },
};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
  Idle,
  Typing,
  RetryScheduled,
  Dispatched,
}
#[derive(Debug, thiserror::Error)]
#[error("Outbox storage unavailable; no retry of committed sends")]
pub struct Unavailable;
impl From<DbErr> for Unavailable {
  fn from(_: DbErr) -> Self {
    Self
  }
}

/// A durable phase per call: never sleeps for typing and never retries sendText.
pub async fn run_once(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
) -> Result<Outcome, Unavailable> {
  if !dispatch::enabled(cfg) {
    return Ok(Outcome::Idle);
  }
  let Some(client) = client else {
    return Ok(Outcome::Idle);
  };
  let Some(mut claim) = dispatch::claim(db, clock, cfg).await? else {
    return Ok(Outcome::Idle);
  };
  if claim.state == "queued" {
    if claim.raw.is_none() {
      match client.new_message_id().await {
        Ok(raw) => {
          if !dispatch::generated(db, clock, &claim, &raw).await? {
            return Ok(Outcome::Idle);
          }
          claim.raw = Some(raw);
        }
        Err(_) => return failed(db, clock, &claim).await,
      }
    }
    let seen_peer = crate::modules::whatsapp::messaging::parse_message_id(&claim.incoming)
      .ok_or(Unavailable)?
      .peer;
    if client.seen(&seen_peer, &claim.incoming).await.is_err()
      || client.start_typing(&claim.target).await.is_err()
    {
      return failed(db, clock, &claim).await;
    }
    return Ok(if dispatch::typing(db, clock, &claim).await? {
      Outcome::Typing
    } else {
      Outcome::Idle
    });
  }
  if client.stop_typing(&claim.target).await.is_err() {
    return failed(db, clock, &claim).await;
  }
  let Some(deadline) = dispatch::commit_wire(db, clock, cfg, &claim).await? else {
    return Ok(Outcome::Idle);
  };
  // A paused worker gets only this persisted dispatch window. There is no atomic
  // database/network fence: a process crash here intentionally sacrifices delivery.
  if !dispatch::enabled(cfg) || clock.now_millis() >= deadline {
    return Ok(Outcome::Dispatched);
  }
  let raw = claim.raw.as_deref().ok_or(Unavailable)?;
  let result = client.send_text(&claim.target, &claim.text, raw).await;
  record(db, clock, &claim, result).await?;
  Ok(Outcome::Dispatched)
}
async fn failed(db: &Db, clock: &dyn Clock, claim: &Claim) -> Result<Outcome, Unavailable> {
  dispatch::failure(db, clock, claim).await?;
  Ok(Outcome::RetryScheduled)
}
async fn record(
  db: &Db,
  clock: &dyn Clock,
  claim: &Claim,
  result: Result<MessageId, SendError>,
) -> Result<(), DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  tx.query_one(sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![claim.contact.into()],
  ))
  .await?;
  let row = tx
    .query_one(sql(
      "SELECT dispatch_committed_at,dispatch_deadline FROM outbox_messages WHERE id=$1 FOR UPDATE",
      vec![claim.id.into()],
    ))
    .await?;
  let Some(row) = row else {
    tx.rollback().await?;
    return Ok(());
  };
  if row
    .try_get::<Option<i64>>("", "dispatch_committed_at")?
    .is_none()
  {
    tx.rollback().await?;
    return Ok(());
  }
  let deadline: Option<i64> = row.try_get("", "dispatch_deadline")?;
  let mut definitive = false;
  match result {
    Ok(id) if dispatch::trusted(&tx, claim.contact, &id.peer).await? => {
      tx.execute(sql("UPDATE outbox_messages SET state='sent',positive_ack=GREATEST(positive_ack,1),returned_id=$2 WHERE id=$1",vec![claim.id.into(),id.serialized.into()])).await?;
      tx.execute(sql("UPDATE contacts SET privacy_first_sent_at=COALESCE(privacy_first_sent_at,$2) WHERE id=$1 AND EXISTS (SELECT FROM outbox_messages WHERE id=$3 AND privacy_prefix)",vec![claim.contact.into(),clock.now_millis().into(),claim.id.into()])).await?;
      definitive = true;
    }
    Err(SendError::Rejected) => {
      tx.execute(sql("UPDATE outbox_messages SET state=CASE WHEN positive_ack>0 THEN 'sent' ELSE 'failed' END,last_error_code='provider_rejected' WHERE id=$1",vec![claim.id.into()])).await?;
      definitive = true;
    }
    _ => {} // Preserve uncertainty and any independently received positive/error ack.
  }
  if definitive {
    tx.execute(sql(
      "UPDATE contacts SET dispatch_until=NULL WHERE id=$1 AND dispatch_until=$2",
      vec![claim.contact.into(), deadline.into()],
    ))
    .await?;
  }
  tx.commit().await?;
  Ok(())
}
