use crate::modules::whatsapp::inbox::sql;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr};
use uuid::Uuid;

/// Serialize evidence for the same aliases before locking Contacts in UUID order.
/// Ack and send results never call this function or create identity evidence.
pub async fn assign(
  tx: &DatabaseTransaction,
  inbox: &str,
  original: &str,
  phone: Option<&str>,
  now: i64,
) -> Result<Uuid, DbErr> {
  let mut aliases = vec![original];
  if let Some(phone) = phone {
    aliases.push(phone);
  }
  aliases.sort_unstable();
  aliases.dedup();
  for alias in &aliases {
    tx.execute(sql(
      "SELECT pg_advisory_xact_lock(hashtextextended($1,7))",
      vec![(*alias).into()],
    ))
    .await?;
  }
  let mut ids = Vec::new();
  for alias in &aliases {
    if let Some(row) = tx
      .query_one(sql(
        "SELECT contact_id FROM contact_aliases WHERE jid=$1",
        vec![(*alias).into()],
      ))
      .await?
    {
      ids.push(row.try_get::<Uuid>("", "contact_id")?);
    }
  }
  ids.sort_unstable();
  ids.dedup();
  for id in &ids {
    tx.query_one(sql(
      "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
      vec![(*id).into()],
    ))
    .await?;
  }
  let id = if ids.len() > 1 {
    for id in &ids {
      tx.execute(sql(
        "UPDATE contacts SET identity_conflict=true WHERE id=$1",
        vec![(*id).into()],
      ))
      .await?;
    }
    tx.query_one(sql(
      "SELECT contact_id FROM contact_aliases WHERE jid=$1",
      vec![original.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Identity evidence unavailable".into()))?
    .try_get("", "contact_id")?
  } else if let Some(id) = ids.first() {
    *id
  } else {
    let id = Uuid::now_v7();
    tx.execute(sql(
      "INSERT INTO contacts(id,created_at) VALUES ($1,$2)",
      vec![id.into(), now.into()],
    ))
    .await?;
    id
  };
  if ids.len() <= 1 {
    for alias in aliases {
      tx.execute(sql("INSERT INTO contact_aliases(jid,contact_id,inbox_message_id) VALUES ($1,$2,$3) ON CONFLICT (jid) DO NOTHING",vec![alias.into(),id.into(),inbox.into()])).await?;
    }
  }
  tx.execute(sql(
    "UPDATE inbox_messages SET contact_id=$2 WHERE message_id=$1",
    vec![inbox.into(), id.into()],
  ))
  .await?;
  Ok(id)
}
