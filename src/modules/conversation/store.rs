use super::templates;
use crate::{
  database::Db,
  modules::whatsapp::{bound_transaction, inbox::sql},
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};
use uuid::Uuid;

pub struct Reply<'a> {
  pub contact: Uuid,
  pub inbox: &'a str,
  pub target: &'a str,
  pub followup: Option<Uuid>,
  pub template: &'a str,
}

pub async fn enqueue(
  tx: &DatabaseTransaction,
  reply: Reply<'_>,
  origin: &str,
  now: i64,
) -> Result<(), DbErr> {
  let row = tx
    .query_one(sql(
      "SELECT privacy_first_sent_at FROM contacts WHERE id=$1",
      vec![reply.contact.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Contact unavailable".into()))?;
  let privacy = row
    .try_get::<Option<i64>>("", "privacy_first_sent_at")?
    .is_none();
  let text = templates::render(reply.template, privacy, origin);
  let delay = typing_delay(&text)?;
  let id = Uuid::now_v7();
  let inserted=tx.execute(sql("INSERT INTO outbox_messages(id,contact_id,wrote_first_inbox_id,original_target,reply_inbox_id,resolved_followup_id,template,purpose,rendered_text,privacy_prefix,created_at,typing_delay_ms,eligible_at) VALUES ($1,$2,$3,$4,$5,$6,$7,CASE WHEN $7='resolved' THEN 'status' ELSE 'conversation' END,$8,$9,$10,$11,$10) ON CONFLICT DO NOTHING",vec![id.into(),reply.contact.into(),reply.inbox.into(),reply.target.into(),reply.followup.is_none().then_some(reply.inbox).into(),reply.followup.into(),reply.template.into(),text.into(),privacy.into(),now.into(),delay.into()])).await?;
  if inserted.rows_affected() == 1 {
    tx.execute(sql("INSERT INTO processing_jobs(outbox_message_id,kind,available_at) VALUES ($1,'send_outbox',to_timestamp($2::double precision/1000))",vec![id.into(),now.into()])).await?;
  }
  Ok(())
}

pub struct Message<'a> {
  pub contact: Uuid,
  pub appointment: Option<Uuid>,
  pub key: &'a str,
  pub text: &'a str,
  pub purpose: &'a str,
  pub revision: Option<i64>,
}

/// A factual application message uses the same durable, wrote-first sender.
pub async fn enqueue_text(
  tx: &DatabaseTransaction,
  message: Message<'_>,
  origin: &str,
  now: i64,
) -> Result<(), DbErr> {
  if !matches!(
    message.purpose,
    "conversation" | "status" | "reminder" | "doctor_notification" | "payment_details"
  ) || message.key.is_empty()
    || message.text.is_empty()
    || (message.revision.is_some() && message.appointment.is_none())
  {
    return Err(DbErr::Custom("Invalid application message".into()));
  }
  let destination_query = if message.purpose == "doctor_notification" {
    "SELECT c.privacy_first_sent_at,i.message_id,i.original_chat_identity FROM contacts c JOIN doctor_number_state d ON d.contact_id=c.id AND d.verified_phone IS NOT NULL JOIN inbox_messages i ON i.message_id=d.inbox_message_id AND i.contact_id=c.id AND i.original_chat_identity=d.original_target WHERE c.id=$1"
  } else {
    "SELECT c.privacy_first_sent_at,i.message_id,i.original_chat_identity FROM contacts c JOIN LATERAL (SELECT message_id,original_chat_identity FROM inbox_messages WHERE contact_id=c.id AND EXISTS (SELECT FROM contact_aliases WHERE contact_id=c.id AND jid=original_chat_identity) ORDER BY intake_order DESC LIMIT 1) i ON true WHERE c.id=$1"
  };
  let row = tx
    .query_one(sql(destination_query, vec![message.contact.into()]))
    .await?
    .ok_or_else(|| DbErr::Custom("Trusted incoming conversation unavailable".into()))?;
  let privacy = message.purpose != "payment_details"
    && row
      .try_get::<Option<i64>>("", "privacy_first_sent_at")?
      .is_none();
  let text = if privacy {
    format!("{}{}", templates::privacy_notice(origin), message.text)
  } else {
    message.text.to_owned()
  };
  let id = Uuid::now_v7();
  let delay = typing_delay(&text)?;
  let incoming: String = row.try_get("", "message_id")?;
  let target: String = row.try_get("", "original_chat_identity")?;
  let inserted = tx
    .execute(sql(
      "INSERT INTO outbox_messages(id,contact_id,wrote_first_inbox_id,original_target,template,rendered_text,privacy_prefix,created_at,typing_delay_ms,eligible_at,appointment_id,dedupe_key,purpose,expected_revision) VALUES ($1,$2,$3,$4,'appointment',$5,$6,$7,$8,$7,$9,$10,$11,$12) ON CONFLICT (dedupe_key) DO NOTHING",
      vec![
        id.into(),
        message.contact.into(),
        incoming.into(),
        target.into(),
        text.into(),
        privacy.into(),
        now.into(),
        delay.into(),
        message.appointment.into(),
        message.key.into(),
        message.purpose.into(),
        message.revision.into(),
      ],
    ))
    .await?;
  if inserted.rows_affected() == 1 {
    tx.execute(sql(
      "INSERT INTO processing_jobs(outbox_message_id,kind,available_at) VALUES ($1,'send_outbox',to_timestamp($2::double precision/1000))",
      vec![id.into(), now.into()],
    ))
    .await?;
  }
  Ok(())
}

fn typing_delay(text: &str) -> Result<i32, DbErr> {
  let max = 6000.min(2000 + text.chars().count() * 1000 / 80);
  let range = (max - 2000 + 1) as u32;
  // Rejection sampling avoids modulo bias; the sample is persisted exactly once.
  loop {
    let mut bytes = [0; 4];
    getrandom::fill(&mut bytes).map_err(|_| DbErr::Custom("Randomness unavailable".into()))?;
    let sample = u32::from_le_bytes(bytes);
    if sample < u32::MAX - u32::MAX % range {
      return Ok(2000 + (sample % range) as i32);
    }
  }
}

pub async fn resolve(db: &Db, id: Uuid, origin: &str, now: i64) -> Result<bool, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let Some(candidate) = tx
    .query_one(sql(
      "SELECT contact_id FROM followup_requests WHERE id=$1",
      vec![id.into()],
    ))
    .await?
  else {
    tx.commit().await?;
    return Ok(false);
  };
  let contact: Uuid = candidate.try_get("", "contact_id")?;
  tx.query_one(sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![contact.into()],
  ))
  .await?;
  let row=tx.query_one(sql("UPDATE followup_requests SET resolved_at=$2 WHERE id=$1 AND resolved_at IS NULL RETURNING inbox_message_id,original_target",vec![id.into(),now.into()])).await?;
  if let Some(row) = row {
    let inbox: String = row.try_get("", "inbox_message_id")?;
    let target: String = row.try_get("", "original_target")?;
    enqueue(
      &tx,
      Reply {
        contact,
        inbox: &inbox,
        target: &target,
        followup: Some(id),
        template: "resolved",
      },
      origin,
      now,
    )
    .await?;
    crate::modules::whatsapp::takeover::end(&tx, contact, now).await?;
  }
  tx.commit().await?;
  Ok(true)
}
