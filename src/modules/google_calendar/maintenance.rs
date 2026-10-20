use super::{crypto, store, Error};
use crate::{common::config::Config, database::Db};
use sea_orm::ConnectionTrait;

const SUPERSEDE_PENDING: &str = "UPDATE google_auth_attempts SET outcome='superseded',verifier_ciphertext=NULL,verifier_nonce=NULL WHERE outcome='pending'";

/// Offline Google key rotation. The app must be stopped and quiescent.
/// Re-encrypts stored credentials under `new` in one transaction and supersedes
/// every pending OAuth verifier; grant/selection/target metadata is unchanged.
/// This is maintenance, not compromise remediation or provider revocation.
pub async fn rotate_key(db: &Db, cfg: &Config, new: &[u8; 32]) -> Result<u64, Error> {
  let config = cfg.google.as_ref().ok_or(Error::Unavailable)?;
  let old = config.key;
  if new == &old {
    return Err(Error::Unavailable);
  }
  if cfg.doctor_totp_key.as_ref() == Some(new) {
    return Err(Error::Unavailable);
  }
  let tx = store::transaction(db).await?;
  let row = store::lock(&tx).await?;
  let generation: i64 = row.try_get("", "generation")?;
  let ciphertext: Option<Vec<u8>> = row.try_get("", "credential_ciphertext")?;
  let nonce: Option<Vec<u8>> = row.try_get("", "credential_nonce")?;
  // Decrypt first: a wrong current key fails (and rolls back) before any write.
  let reencrypted = match (ciphertext, nonce) {
    (Some(ciphertext), Some(nonce)) => {
      let plain = crypto::decrypt(&old, "credentials", "1", generation, &ciphertext, &nonce)?;
      Some(crypto::encrypt(
        new,
        "credentials",
        "1",
        generation,
        &plain,
      )?)
    }
    (None, None) => None,
    _ => return Err(Error::Unavailable),
  };
  let count = u64::from(reencrypted.is_some());
  if let Some((ciphertext, nonce)) = reencrypted {
    tx.execute(store::sql(
      "UPDATE google_connection SET credential_ciphertext=$1,credential_nonce=$2 WHERE id=1",
      vec![ciphertext.into(), nonce.into()],
    ))
    .await?;
  }
  tx.execute(store::sql(SUPERSEDE_PENDING, vec![])).await?;
  tx.commit().await?;
  Ok(count)
}

/// Lost-key recovery: discard local tokens without any old-key decrypt or
/// provider call, then reconnect through the ordinary authenticated flow.
/// This never revokes the provider-side grant.
pub async fn reconnect_only(db: &Db) -> Result<(), Error> {
  let tx = store::transaction(db).await?;
  store::lock(&tx).await?;
  tx.execute(store::sql("UPDATE google_connection SET status='reauthorize',notice='reauthorize',credential_ciphertext=NULL,credential_nonce=NULL,credential_expires_at=NULL,refresh_claim=NULL,refresh_deadline=NULL,refresh_dispatched=false WHERE id=1", vec![])).await?;
  tx.execute(store::sql(SUPERSEDE_PENDING, vec![])).await?;
  tx.commit().await?;
  Ok(())
}
