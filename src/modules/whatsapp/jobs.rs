use super::inbox::{self, sql};
use crate::{database::Db, modules::doctor::Clock};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
  Idle,
  Prepared(String),
  RetryScheduled(String),
  Failed(String),
}
#[derive(Debug, thiserror::Error)]
#[error("Inbox preparation database unavailable; pending work remains recoverable")]
pub struct Unavailable;
impl From<DbErr> for Unavailable {
  fn from(_: DbErr) -> Self {
    Self
  }
}

enum PreparationError {
  Invalid,
  Database,
}
impl From<DbErr> for PreparationError {
  fn from(_: DbErr) -> Self {
    Self::Database
  }
}

/// One short transaction, one job, no leases. Dropping this future rolls back its transaction.
/// IDs in outcomes are for controlled callers; never log them or provider/database errors.
pub async fn run_once(db: &Db, clock: &dyn Clock) -> Result<Outcome, Unavailable> {
  let now = clock.now();
  let tx = db.conn.begin().await?;
  super::bound_transaction(&tx).await?;
  let job = tx.query_one(sql("SELECT inbox_message_id, attempts FROM processing_jobs WHERE kind='prepare_inbox' AND status='pending' AND available_at <= to_timestamp($1) AND NOT EXISTS (SELECT 1 FROM receipt_intake_state r WHERE r.inbox_message_id=processing_jobs.inbox_message_id AND r.state='pending') ORDER BY available_at, inbox_message_id FOR UPDATE SKIP LOCKED LIMIT 1", vec![now.into()])).await?;
  let Some(job) = job else {
    tx.commit().await?;
    return Ok(Outcome::Idle);
  };
  let id: String = job.try_get("", "inbox_message_id")?;
  let attempts: i32 = job.try_get::<i32>("", "attempts")? + 1;
  // Savepoint is AFTER the claim: rolling back a failed write must retain the job lock.
  let work = tx.begin().await?;
  match prepare(&work, &id, attempts, now).await {
    Ok(()) => {
      work.commit().await?;
      tx.commit().await?;
      Ok(Outcome::Prepared(id))
    }
    Err(error) => {
      work.rollback().await?;
      let (code, terminal) = match error {
        PreparationError::Invalid => ("invalid_stored_message", true),
        PreparationError::Database => ("preparation_database_error", attempts >= 5),
      };
      let delay = (5_i64 * 2_i64.pow((attempts - 1) as u32)).min(300);
      tx.execute(sql("UPDATE processing_jobs SET attempts=$2, status=$3, last_error_code=$4, available_at=to_timestamp($5), finished_at=CASE WHEN $3='failed' THEN to_timestamp($6) ELSE NULL END WHERE inbox_message_id=$1 AND kind='prepare_inbox'",
        vec![id.clone().into(), attempts.into(), if terminal {"failed"} else {"pending"}.into(), code.into(), (now + delay).into(), now.into()])).await?;
      tx.commit().await?;
      Ok(if terminal {
        Outcome::Failed(id)
      } else {
        Outcome::RetryScheduled(id)
      })
    }
  }
}

async fn prepare(
  tx: &DatabaseTransaction,
  id: &str,
  attempts: i32,
  now: i64,
) -> Result<(), PreparationError> {
  let row = tx
    .query_one(sql(
      "SELECT raw_envelope FROM inbox_messages WHERE message_id=$1",
      vec![id.into()],
    ))
    .await?
    .ok_or(PreparationError::Invalid)?;
  let raw: Vec<u8> = row.try_get("", "raw_envelope")?;
  let incoming = inbox::incoming(&raw)
    .map_err(|_| PreparationError::Invalid)?
    .ok_or(PreparationError::Invalid)?;
  if incoming.id != id {
    return Err(PreparationError::Invalid);
  }
  let envelope = serde_json::from_slice(&raw).map_err(|_| PreparationError::Invalid)?;
  let canonical = inbox::canonical(&envelope).map_err(|_| PreparationError::Invalid)?;
  tx.execute(sql("UPDATE inbox_messages SET preparation_state='ready_for_conversation', message_kind=$2, body=$3, media_metadata=$4, prepared_at=to_timestamp($5) WHERE message_id=$1",
    vec![id.into(), canonical.kind.into(), canonical.body.into(), canonical.media.into(), now.into()])).await?;
  tx.execute(sql("UPDATE processing_jobs SET status='succeeded', attempts=$2, last_error_code=NULL, finished_at=to_timestamp($3) WHERE inbox_message_id=$1 AND kind='prepare_inbox'",
    vec![id.into(), attempts.into(), now.into()])).await?;
  tx.execute(sql("INSERT INTO processing_jobs (inbox_message_id,kind,available_at) VALUES ($1,'process_conversation',to_timestamp($2)) ON CONFLICT DO NOTHING",vec![id.into(),now.into()])).await?;
  Ok(())
}
