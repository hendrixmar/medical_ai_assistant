use super::{
  client::{Client, Failure},
  config::GoogleConfig,
  crypto,
  probes::{self, Probe},
  store::{self, sql},
  Error,
};
use crate::{database::Db, modules::doctor::Clock};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

pub(super) struct Access {
  pub token: String,
  pub generation: i64,
  pub version: i64,
}
/// Persist dispatch before refresh. A crashed dispatched rotation is never replayed.
pub(super) async fn access(
  db: &Db,
  clock: &dyn Clock,
  config: &GoogleConfig,
  client: &Client,
  force_version: Option<i64>,
  probe: Option<&Probe>,
) -> Result<Access, Error> {
  let tx = store::transaction(db).await?;
  let row = store::lock(&tx).await?;
  let probe_deadline = if let Some(probe) = probe {
    Some(probes::refresh_authorization(&tx, &row, clock, probe).await?)
  } else {
    None
  };
  if row.try_get::<String>("", "status")? != "connected" {
    return Err(Error::Reauthorize);
  }
  let generation: i64 = row.try_get("", "generation")?;
  let version: i64 = row.try_get("", "token_version")?;
  let tokens = store::decrypt(&row, config)?;
  let now = clock.now_millis();
  if let Some(deadline) = row.try_get::<Option<i64>>("", "refresh_deadline")? {
    if deadline <= now && row.try_get::<bool>("", "refresh_dispatched")? {
      tx.execute(sql("UPDATE google_connection SET status='reauthorize',notice='refresh_uncertain',refresh_claim=NULL,refresh_deadline=NULL,refresh_dispatched=false WHERE id=1",vec![])).await?;
      tx.commit().await?;
      return Err(Error::Reauthorize);
    }
    if deadline > now {
      return Err(Error::Unavailable);
    }
  }
  if tokens.expires_at > now + 30_000 && force_version != Some(version) {
    tx.commit().await?;
    return Ok(Access {
      token: tokens.access,
      generation,
      version,
    });
  }
  let claim = Uuid::now_v7();
  let deadline = now + 30_000;
  tx.execute(sql("UPDATE google_connection SET refresh_claim=$1,refresh_deadline=$2,refresh_dispatched=true WHERE id=1",vec![claim.into(),deadline.into()])).await?;
  if clock.now_millis() + 8_000 >= probe_deadline.unwrap_or(deadline).min(deadline) {
    return Err(Error::Unavailable);
  }
  tx.commit().await?; // An ambiguous COMMIT returns Err: forbid dispatch.
  let fence = store::transaction(db).await?;
  let current = store::lock(&fence).await?;
  let dispatch_deadline = if let Some(probe) = probe {
    probes::refresh_authorization(&fence, &current, clock, probe)
      .await?
      .min(deadline)
  } else {
    deadline
  };
  if clock.now_millis() + 8_000 >= dispatch_deadline
    || current.try_get::<i64>("", "generation")? != generation
    || current.try_get::<i64>("", "token_version")? != version
    || current.try_get::<Option<Uuid>>("", "refresh_claim")? != Some(claim)
    || current.try_get::<String>("", "status")? != "connected"
  {
    return Err(Error::Unavailable);
  }
  fence.commit().await?;
  if clock.now_millis() + 8_000 >= dispatch_deadline {
    return Err(Error::Unavailable);
  }
  let result = client
    .refresh(config, &tokens.refresh, clock.now_millis())
    .await;
  tokio::time::timeout(std::time::Duration::from_secs(3), async {
  let tx = store::transaction(db).await?;
  let current = store::lock(&tx).await?;
  if current.try_get::<i64>("", "generation")? != generation
    || current.try_get::<i64>("", "token_version")? != version
    || current.try_get::<Option<Uuid>>("", "refresh_claim")? != Some(claim)
    || clock.now_millis() >= deadline
  {
    return Err(Error::Unavailable);
  }
  match result {
    Ok(tokens) => {
      let (bytes, nonce) = crypto::encrypt(
        &config.key,
        "credentials",
        "1",
        generation,
        &serde_json::to_vec(&tokens).map_err(|_| Error::Unavailable)?,
      )?;
      tx.execute(sql("UPDATE google_connection SET credential_ciphertext=$1,credential_nonce=$2,credential_expires_at=$3,token_version=token_version+1,refresh_claim=NULL,refresh_deadline=NULL,refresh_dispatched=false WHERE id=1",vec![bytes.into(),nonce.into(),tokens.expires_at.into()])).await?;
      tx.commit().await?;
      Ok(Access {
        token: tokens.access,
        generation,
        version: version + 1,
      })
    }
    Err(failure) => {
      // Unavailable includes lost/malformed success. Rotation may have occurred.
      let uncertain = matches!(failure, Failure::Unavailable);
      let reauthorize =
        uncertain || matches!(failure, Failure::InvalidGrant | Failure::Unauthorized);
      tx.execute(sql("UPDATE google_connection SET status=CASE WHEN $1 THEN 'reauthorize' ELSE status END,notice=$2,refresh_claim=NULL,refresh_deadline=NULL,refresh_dispatched=false WHERE id=1",vec![reauthorize.into(),if uncertain {"refresh_uncertain"} else if reauthorize {"reauthorize"} else {"provider_unavailable"}.into()])).await?;
      tx.commit().await?;
      Err(if reauthorize {
        Error::Reauthorize
      } else {
        Error::Unavailable
      })
    }
  }
  }).await.map_err(|_|Error::Unavailable)?
}
pub(super) async fn unauthorized(db: &Db, access: &Access) -> Result<(), Error> {
  db.conn.execute(sql("UPDATE google_connection SET status='reauthorize',notice='reauthorize' WHERE id=1 AND generation=$1 AND token_version=$2",vec![access.generation.into(),access.version.into()])).await?;
  Ok(())
}
