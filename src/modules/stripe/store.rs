use crate::modules::{appointments::store as appointment_store, whatsapp::inbox::sql};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr};
use uuid::Uuid;

pub async fn schedule_link(
  tx: &DatabaseTransaction,
  appointment: Uuid,
  revision: i64,
  amount_centavos: i64,
  deadline: i64,
  now: i64,
) -> Result<(), DbErr> {
  if amount_centavos <= 0 {
    return Ok(());
  }
  let Some(connection) = tx
    .query_one(sql(
      "SELECT account_id FROM stripe_connection WHERE id=1 AND charges_enabled AND account_id IS NOT NULL FOR SHARE",
      vec![],
    ))
    .await?
  else {
    return Ok(());
  };
  let account_id: String = connection.try_get("", "account_id")?;
  tx.execute(sql(
    "INSERT INTO stripe_payment_links(appointment_id,revision,account_id,amount_centavos,created_at,deactivate_at,outcome) VALUES ($1,$2,$3,$4,$5,$6,'pending') ON CONFLICT (appointment_id,revision) DO NOTHING",
    vec![
      appointment.into(),
      revision.into(),
      account_id.into(),
      amount_centavos.into(),
      now.into(),
      deadline.into(),
    ],
  ))
  .await?;
  appointment_store::job(
    tx,
    appointment,
    "stripe_link",
    now,
    &format!("{appointment}:{revision}:stripe_link"),
  )
  .await
}
