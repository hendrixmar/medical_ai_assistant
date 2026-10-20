use super::{crypto, store, Error};
use crate::{common::config::Config, database::Db};
use sea_orm::{ConnectionTrait, TransactionTrait};

/// Offline Doctor key rotation. The app must be stopped and quiescent.
/// Re-encrypts the same TOTP secret and the stable Doctor Number fingerprint
/// subkey under `new` in one transaction; sessions, password and binding stay.
/// This is maintenance, not compromise remediation.
pub async fn rotate_key(db: &Db, cfg: &Config, new: &[u8; 32]) -> Result<u64, Error> {
  let old = cfg.doctor_totp_key.as_ref().ok_or(Error::Unavailable)?;
  if new == old {
    return Err(Error::Unavailable);
  }
  if cfg
    .google
    .as_ref()
    .is_some_and(|google| google.same_key(new))
  {
    return Err(Error::Unavailable);
  }
  let tx = db.conn.begin().await?;
  crate::modules::whatsapp::bound_transaction(&tx).await?;
  let doctor = tx
    .query_one(store::sql(
      "SELECT password_hash, totp_ciphertext, totp_nonce FROM doctor WHERE id=1 FOR UPDATE",
      vec![],
    ))
    .await?
    .ok_or(Error::Unavailable)?;
  // Decrypt first: a missing Doctor or wrong current key fails before any write.
  let secret = crypto::decrypt(
    old,
    &doctor.try_get::<Vec<u8>>("", "totp_ciphertext")?,
    &doctor.try_get::<Vec<u8>>("", "totp_nonce")?,
  )?;
  crate::modules::doctor_number::store::lock(&tx)
    .await
    .map_err(|_| Error::Unavailable)?;
  let retired = crate::modules::doctor_number::store::rotate_key(&tx, old, new)
    .await
    .map_err(|_| Error::Unavailable)?;
  let (ciphertext, nonce) = crypto::encrypt(new, &secret)?;
  tx.execute(store::sql(
    "UPDATE doctor SET totp_ciphertext=$1, totp_nonce=$2 WHERE id=1",
    vec![ciphertext.into(), nonce.into()],
  ))
  .await?;
  tx.commit().await?;
  Ok(retired)
}
