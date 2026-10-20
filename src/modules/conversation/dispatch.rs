use crate::{
  common::config::Config,
  database::Db,
  modules::{
    doctor::Clock,
    whatsapp::{bound_transaction, inbox::sql, messaging::parse_message_id},
  },
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};
use uuid::Uuid;

pub struct Claim {
  pub contact: Uuid,
  pub id: Uuid,
  pub token: Uuid,
  pub state: String,
  pub target: String,
  pub incoming: String,
  pub text: String,
  pub raw: Option<String>,
}

pub fn enabled(cfg: &Config) -> bool {
  cfg.waha_outbound_enabled
    && cfg.waha.is_some()
    && cfg
      .public_origin
      .as_deref()
      .is_some_and(|s| crate::common::config::parse_public_origin(s).is_ok())
    && (1..=1000).contains(&cfg.waha_contact_replies_per_hour)
}
pub async fn trusted(tx: &DatabaseTransaction, contact: Uuid, peer: &str) -> Result<bool, DbErr> {
  Ok(
    tx.query_one(sql(
      "SELECT 1 FROM contact_aliases WHERE contact_id=$1 AND jid=$2",
      vec![contact.into(), peer.into()],
    ))
    .await?
    .is_some(),
  )
}
pub(crate) async fn context(
  tx: &DatabaseTransaction,
  contact: Uuid,
  incoming: &str,
  target: &str,
) -> Result<bool, DbErr> {
  let Some(id) = parse_message_id(incoming) else {
    return Ok(false);
  };
  if id.from_me || !trusted(tx, contact, &id.peer).await? {
    return Ok(false);
  }
  Ok(trusted(tx,contact,target).await? && tx.query_one(sql("SELECT 1 FROM inbox_messages WHERE message_id=$1 AND contact_id=$2 AND original_chat_identity=$3",vec![incoming.into(),contact.into(),target.into()])).await?.is_some())
}
async fn budget(
  tx: &DatabaseTransaction,
  contact: Uuid,
  id: Uuid,
  cap: u32,
  now: i64,
) -> Result<bool, DbErr> {
  let row=tx.query_one(sql("SELECT dispatch_committed_at FROM outbox_messages WHERE contact_id=$1 AND dispatch_committed_at>$2 ORDER BY dispatch_committed_at DESC OFFSET $3 LIMIT 1",vec![contact.into(),(now-3_600_000).into(),i64::from(cap-1).into()])).await?;
  if let Some(row) = row {
    let due = row.try_get::<i64>("", "dispatch_committed_at")? + 3_600_000;
    tx.execute(sql("UPDATE outbox_messages SET state='queued',typing_due_at=NULL,eligible_at=$2,rate_deferred_until=$2 WHERE id=$1",vec![id.into(),due.into()])).await?;
    return Ok(false);
  }
  Ok(true)
}
async fn finish_job(
  tx: &DatabaseTransaction,
  id: Uuid,
  now: i64,
  failed: bool,
) -> Result<(), DbErr> {
  tx.execute(sql("UPDATE processing_jobs SET status=$2,finished_at=to_timestamp($3::double precision/1000) WHERE outbox_message_id=$1 AND kind='send_outbox'",vec![id.into(),if failed {"failed"} else {"succeeded"}.into(),now.into()])).await?;
  Ok(())
}

async fn current(tx: &DatabaseTransaction, id: Uuid, now: i64) -> Result<bool, DbErr> {
  Ok(tx
    .query_one(sql(
      "SELECT 1 FROM outbox_messages o JOIN contacts c ON c.id=o.contact_id LEFT JOIN appointments a ON a.id=o.appointment_id WHERE o.id=$1 AND o.text_deleted_at IS NULL AND (o.expected_revision IS NULL OR a.revision=o.expected_revision) AND (o.purpose<>'conversation' OR c.takeover_until IS NULL OR c.takeover_until<=$2) AND (o.purpose<>'doctor_notification' OR EXISTS (SELECT FROM doctor_number_state d WHERE d.verified_phone IS NOT NULL AND d.contact_id=o.contact_id AND d.original_target=o.original_target AND d.inbox_message_id=o.wrote_first_inbox_id FOR SHARE))",
      vec![id.into(), now.into()],
    ))
    .await?
    .is_some())
}

async fn suppress(tx: &DatabaseTransaction, id: Uuid, now: i64) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE outbox_messages SET state='cancelled' WHERE id=$1 AND state IN ('queued','typing')",
    vec![id.into()],
  ))
  .await?;
  finish_job(tx, id, now, false).await
}
pub async fn release(tx: &DatabaseTransaction, contact: Uuid) -> Result<(), DbErr> {
  tx.execute(sql(
    "UPDATE contacts SET claim_token=NULL,claim_until=NULL WHERE id=$1",
    vec![contact.into()],
  ))
  .await?;
  Ok(())
}

pub async fn claim(db: &Db, clock: &dyn Clock, cfg: &Config) -> Result<Option<Claim>, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let candidate=tx.query_one(sql("SELECT c.id FROM contacts c JOIN LATERAL (SELECT eligible_at,typing_due_at,sequence FROM outbox_messages WHERE contact_id=c.id AND state IN ('queued','typing') ORDER BY sequence LIMIT 1) head ON true WHERE NOT c.identity_conflict AND (c.claim_until IS NULL OR c.claim_until<=$1) AND (c.dispatch_until IS NULL OR c.dispatch_until<=$1) AND head.eligible_at<=$1 AND (head.typing_due_at IS NULL OR head.typing_due_at<=$1) ORDER BY head.sequence FOR UPDATE OF c SKIP LOCKED LIMIT 1",vec![now.into()])).await?;
  let Some(candidate) = candidate else {
    tx.commit().await?;
    return Ok(None);
  };
  let contact: Uuid = candidate.try_get("", "id")?;
  let row=tx.query_one(sql("SELECT * FROM outbox_messages WHERE contact_id=$1 AND state IN ('queued','typing') ORDER BY sequence FOR UPDATE LIMIT 1",vec![contact.into()])).await?.ok_or_else(||DbErr::Custom("Outbox changed".into()))?;
  let id: Uuid = row.try_get("", "id")?;
  if !current(&tx, id, now).await? {
    suppress(&tx, id, now).await?;
    release(&tx, contact).await?;
    tx.commit().await?;
    return Ok(None);
  }
  if !context(
    &tx,
    contact,
    &row.try_get::<String>("", "wrote_first_inbox_id")?,
    &row.try_get::<String>("", "original_target")?,
  )
  .await?
  {
    tx.execute(sql(
      "UPDATE outbox_messages SET state='failed',last_error_code='invalid_incoming_id' WHERE id=$1",
      vec![id.into()],
    ))
    .await?;
    finish_job(&tx, id, now, true).await?;
    tx.commit().await?;
    return Ok(None);
  }
  if !budget(&tx, contact, id, cfg.waha_contact_replies_per_hour, now).await? {
    tx.commit().await?;
    return Ok(None);
  }
  let token = Uuid::now_v7();
  tx.execute(sql(
    "UPDATE contacts SET claim_token=$2,claim_until=$3 WHERE id=$1",
    vec![contact.into(), token.into(), (now + 30_000).into()],
  ))
  .await?;
  let claim = Claim {
    contact,
    id,
    token,
    state: row.try_get("", "state")?,
    target: row.try_get("", "original_target")?,
    incoming: row.try_get("", "wrote_first_inbox_id")?,
    text: row.try_get("", "rendered_text")?,
    raw: row.try_get("", "raw_provider_id")?,
  };
  tx.commit().await?;
  Ok(Some(claim))
}

/// Lock order for every phase: Contact, outbox, job. Clock is sampled after locks.
pub async fn fence(
  db: &Db,
  clock: &dyn Clock,
  claim: &Claim,
) -> Result<Option<DatabaseTransaction>, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let contact = tx
    .query_one(sql(
      "SELECT claim_token,claim_until,identity_conflict FROM contacts WHERE id=$1 FOR UPDATE",
      vec![claim.contact.into()],
    ))
    .await?;
  let row = tx
    .query_one(sql(
      "SELECT state FROM outbox_messages WHERE id=$1 FOR UPDATE",
      vec![claim.id.into()],
    ))
    .await?;
  let now = clock.now_millis();
  let valid = match (contact, row) {
    (Some(c), Some(o)) => {
      c.try_get::<Option<Uuid>>("", "claim_token")? == Some(claim.token)
        && c
          .try_get::<Option<i64>>("", "claim_until")?
          .is_some_and(|t| t > now)
        && !c.try_get::<bool>("", "identity_conflict")?
        && o.try_get::<String>("", "state")? == claim.state
    }
    _ => false,
  };
  if valid {
    if !current(&tx, claim.id, now).await? {
      suppress(&tx, claim.id, now).await?;
      release(&tx, claim.contact).await?;
      tx.commit().await?;
      return Ok(None);
    }
    Ok(Some(tx))
  } else {
    tx.rollback().await?;
    Ok(None)
  }
}
pub async fn generated(db: &Db, clock: &dyn Clock, c: &Claim, raw: &str) -> Result<bool, DbErr> {
  let Some(tx) = fence(db, clock, c).await? else {
    return Ok(false);
  };
  tx.execute(sql(
    "UPDATE outbox_messages SET raw_provider_id=$2 WHERE id=$1 AND raw_provider_id IS NULL",
    vec![c.id.into(), raw.into()],
  ))
  .await?;
  tx.commit().await?;
  Ok(true)
}
pub async fn typing(db: &Db, clock: &dyn Clock, c: &Claim) -> Result<bool, DbErr> {
  let Some(tx) = fence(db, clock, c).await? else {
    return Ok(false);
  };
  tx.execute(sql("UPDATE outbox_messages SET state='typing',typing_due_at=$2+typing_delay_ms,rate_deferred_until=NULL WHERE id=$1",vec![c.id.into(),clock.now_millis().into()])).await?;
  release(&tx, c.contact).await?;
  tx.commit().await?;
  Ok(true)
}
pub async fn failure(db: &Db, clock: &dyn Clock, c: &Claim) -> Result<(), DbErr> {
  let Some(tx) = fence(db, clock, c).await? else {
    return Ok(());
  };
  let now = clock.now_millis();
  let row=tx.query_one(sql("UPDATE outbox_messages SET preflight_failures=preflight_failures+1,last_error_code='preflight_unavailable',state=CASE WHEN preflight_failures>=4 THEN 'failed' ELSE 'queued' END,typing_due_at=NULL,eligible_at=$2+(5000::bigint << preflight_failures) WHERE id=$1 RETURNING preflight_failures",vec![c.id.into(),now.into()])).await?.ok_or_else(||DbErr::Custom("Outbox unavailable".into()))?;
  let attempts: i32 = row.try_get("", "preflight_failures")?;
  tx.execute(sql("UPDATE processing_jobs SET attempts=$2,last_error_code='preflight_unavailable' WHERE outbox_message_id=$1 AND kind='send_outbox'",vec![c.id.into(),attempts.into()])).await?;
  if attempts == 5 {
    finish_job(&tx, c.id, now, true).await?;
  }
  release(&tx, c.contact).await?;
  tx.commit().await?;
  Ok(())
}
pub async fn commit_wire(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  c: &Claim,
) -> Result<Option<i64>, DbErr> {
  let Some(tx) = fence(db, clock, c).await? else {
    return Ok(None);
  };
  let now = clock.now_millis();
  let row = tx
    .query_one(sql(
      "SELECT * FROM outbox_messages WHERE id=$1",
      vec![c.id.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Outbox unavailable".into()))?;
  let head=tx.query_one(sql("SELECT id FROM outbox_messages WHERE contact_id=$1 AND state IN ('queued','typing') ORDER BY sequence LIMIT 1",vec![c.contact.into()])).await?;
  if !enabled(cfg)
    || c.state != "typing"
    || !context(
      &tx,
      c.contact,
      &row.try_get::<String>("", "wrote_first_inbox_id")?,
      &row.try_get::<String>("", "original_target")?,
    )
    .await?
    || head.and_then(|r| r.try_get::<Uuid>("", "id").ok()) != Some(c.id)
    || !budget(&tx, c.contact, c.id, cfg.waha_contact_replies_per_hour, now).await?
  {
    release(&tx, c.contact).await?;
    tx.commit().await?;
    return Ok(None);
  }
  let now = clock.now_millis();
  let deadline = now + 125_000;
  // The lease predicate is evaluated again at the irreversible state transition.
  let changed=tx.execute(sql("UPDATE outbox_messages SET state='uncertain',dispatch_committed_at=$2,dispatch_deadline=$3,rate_deferred_until=NULL WHERE id=$1 AND state='typing' AND raw_provider_id IS NOT NULL AND EXISTS (SELECT FROM contacts WHERE id=$4 AND claim_token=$5 AND claim_until>$2 AND (dispatch_until IS NULL OR dispatch_until<=$2))",vec![c.id.into(),now.into(),deadline.into(),c.contact.into(),c.token.into()])).await?.rows_affected()==1;
  if !changed {
    tx.rollback().await?;
    return Ok(None);
  }
  tx.execute(sql(
    "UPDATE contacts SET dispatch_until=$2,claim_token=NULL,claim_until=NULL WHERE id=$1",
    vec![c.contact.into(), deadline.into()],
  ))
  .await?;
  finish_job(&tx, c.id, now, false).await?;
  // An error here is deliberately NOT permission to send, including ambiguous COMMIT.
  tx.commit().await?;
  Ok(Some(deadline))
}
