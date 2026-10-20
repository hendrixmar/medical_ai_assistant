use super::{client::Tokens, config::GoogleConfig, crypto, Error};
use crate::{
  database::Db,
  modules::doctor::{Clock, DoctorSession},
};
use sea_orm::{
  ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Statement, TransactionTrait, Value,
};
use uuid::Uuid;

pub(super) fn sql(query: &str, values: Vec<Value>) -> Statement {
  Statement::from_sql_and_values(DbBackend::Postgres, query, values)
}
pub(super) async fn transaction(db: &Db) -> Result<DatabaseTransaction, Error> {
  let tx = db.conn.begin().await?;
  tx.execute_unprepared("SET LOCAL statement_timeout='3s'; SET LOCAL lock_timeout='2s'")
    .await?;
  Ok(tx)
}
pub(super) async fn lock(tx: &DatabaseTransaction) -> Result<QueryResult, Error> {
  tx.query_one(sql(
    "SELECT * FROM google_connection WHERE id=1 FOR UPDATE",
    vec![],
  ))
  .await?
  .ok_or(Error::Unavailable)
}
pub(super) async fn live(tx: &DatabaseTransaction, hash: &[u8], now: i64) -> Result<(), Error> {
  if tx
    .query_one(sql(
      "SELECT token_hash FROM doctor_sessions WHERE token_hash=$1 AND expires_at>$2 FOR UPDATE",
      vec![hash.to_vec().into(), (now / 1000).into()],
    ))
    .await?
    .is_none()
  {
    return Err(Error::Invalid);
  }
  Ok(())
}
pub(super) async fn load(db: &Db) -> Result<QueryResult, Error> {
  db.conn
    .query_one(sql("SELECT * FROM google_connection WHERE id=1", vec![]))
    .await?
    .ok_or(Error::Unavailable)
}
pub(super) async fn issue(
  db: &Db,
  clock: &dyn Clock,
  config: &GoogleConfig,
  session: &DoctorSession,
) -> Result<(String, String), Error> {
  let tx = transaction(db).await?;
  live(&tx, &session.hash, clock.now_millis()).await?;
  let row = lock(&tx).await?;
  live(&tx, &session.hash, clock.now_millis()).await?;
  let now = clock.now_millis();
  let count = tx
    .query_one(sql(
      "SELECT count(*) AS n FROM google_auth_attempts WHERE issued_at>$1",
      vec![(now - 3_600_000).into()],
    ))
    .await?
    .ok_or(Error::Unavailable)?
    .try_get::<i64>("", "n")?;
  if count >= 5 {
    return Err(Error::Limited);
  }
  let generation: i64 = row.try_get("", "generation")?;
  let sequence = row.try_get::<i64>("", "auth_sequence")? + 1;
  let id = Uuid::now_v7();
  let state = crypto::token()?;
  let verifier = crypto::token()?;
  let (bytes, nonce) = crypto::encrypt(
    &config.key,
    "pkce",
    &id.to_string(),
    generation,
    verifier.as_bytes(),
  )?;
  tx.execute(sql("UPDATE google_auth_attempts SET outcome='superseded',verifier_ciphertext=NULL,verifier_nonce=NULL WHERE outcome='pending'",vec![])).await?;
  tx.execute(sql("INSERT INTO google_auth_attempts(id,state_hash,session_hash,generation,sequence,config_fingerprint,verifier_ciphertext,verifier_nonce,issued_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",vec![id.into(),crypto::hash(&state).into(),session.hash.clone().into(),generation.into(),sequence.into(),config.fingerprint.clone().into(),bytes.into(),nonce.into(),now.into(),(now+600_000).into()])).await?;
  tx.execute(sql(
    "UPDATE google_connection SET auth_sequence=$1 WHERE id=1",
    vec![sequence.into()],
  ))
  .await?;
  tx.commit().await?;
  Ok((state, verifier))
}
pub(super) struct Attempt {
  pub id: Uuid,
  pub generation: i64,
  pub sequence: i64,
  pub verifier: String,
}
pub(super) async fn consume(
  db: &Db,
  clock: &dyn Clock,
  config: &GoogleConfig,
  session: &DoctorSession,
  state: &str,
  denied: bool,
) -> Result<Attempt, Error> {
  let tx = transaction(db).await?;
  live(&tx, &session.hash, clock.now_millis()).await?;
  let connection = lock(&tx).await?;
  let row = tx
    .query_one(sql(
      "SELECT * FROM google_auth_attempts WHERE state_hash=$1 FOR UPDATE",
      vec![crypto::hash(state).into()],
    ))
    .await?
    .ok_or(Error::Invalid)?;
  live(&tx, &session.hash, clock.now_millis()).await?;
  let generation: i64 = row.try_get("", "generation")?;
  let sequence: i64 = row.try_get("", "sequence")?;
  if row.try_get::<Vec<u8>>("", "session_hash")? != session.hash
    || row.try_get::<Vec<u8>>("", "config_fingerprint")? != config.fingerprint
    || row.try_get::<String>("", "outcome")? != "pending"
    || row.try_get::<i64>("", "expires_at")? <= clock.now_millis()
    || connection.try_get::<i64>("", "generation")? != generation
    || connection.try_get::<i64>("", "auth_sequence")? != sequence
  {
    return Err(Error::Invalid);
  }
  let id: Uuid = row.try_get("", "id")?;
  let verifier = String::from_utf8(crypto::decrypt(
    &config.key,
    "pkce",
    &id.to_string(),
    generation,
    &row.try_get::<Vec<u8>>("", "verifier_ciphertext")?,
    &row.try_get::<Vec<u8>>("", "verifier_nonce")?,
  )?)
  .map_err(|_| Error::Unavailable)?;
  tx.execute(sql("UPDATE google_auth_attempts SET outcome=$2,verifier_ciphertext=NULL,verifier_nonce=NULL WHERE id=$1",vec![id.into(),if denied {"denied"} else {"consumed"}.into()])).await?;
  tx.execute(sql(
    "UPDATE google_connection SET notice=$1 WHERE id=1",
    vec![if denied {
      "denied"
    } else {
      "authorization_incomplete"
    }
    .into()],
  ))
  .await?;
  tx.commit().await?;
  Ok(Attempt {
    id,
    generation,
    sequence,
    verifier,
  })
}
pub(super) async fn failed(db: &Db, attempt: &Attempt) -> Result<(), Error> {
  let tx = transaction(db).await?;
  let connection = lock(&tx).await?;
  tx.execute(sql(
    "UPDATE google_auth_attempts SET outcome='failed' WHERE id=$1 AND outcome='consumed'",
    vec![attempt.id.into()],
  ))
  .await?;
  if connection.try_get::<i64>("", "auth_sequence")? == attempt.sequence
    && connection.try_get::<i64>("", "generation")? == attempt.generation
  {
    tx.execute(sql(
      "UPDATE google_connection SET notice='authorization_failed' WHERE id=1",
      vec![],
    ))
    .await?;
  }
  tx.commit().await?;
  Ok(())
}
pub(super) async fn install(
  db: &Db,
  clock: &dyn Clock,
  config: &GoogleConfig,
  session: &DoctorSession,
  attempt: &Attempt,
  tokens: &Tokens,
) -> Result<(), Error> {
  let tx = transaction(db).await?;
  live(&tx, &session.hash, clock.now_millis()).await?;
  let row = lock(&tx).await?;
  let evidence = tx
    .query_one(sql(
      "SELECT * FROM google_auth_attempts WHERE id=$1 FOR UPDATE",
      vec![attempt.id.into()],
    ))
    .await?
    .ok_or(Error::Invalid)?;
  // This session row is already locked; resample expiry after connection/attempt waits.
  live(&tx, &session.hash, clock.now_millis()).await?;
  if row.try_get::<i64>("", "generation")? != attempt.generation
    || row.try_get::<i64>("", "auth_sequence")? != attempt.sequence
    || evidence.try_get::<Vec<u8>>("", "config_fingerprint")? != config.fingerprint
    || evidence.try_get::<String>("", "outcome")? != "consumed"
    || evidence.try_get::<i64>("", "expires_at")? <= clock.now_millis()
  {
    return Err(Error::Invalid);
  }
  let generation = attempt.generation + 1;
  let encoded = serde_json::to_vec(tokens).map_err(|_| Error::Unavailable)?;
  let (bytes, nonce) = crypto::encrypt(&config.key, "credentials", "1", generation, &encoded)?;
  tx.execute(sql("UPDATE google_connection SET status='connected',generation=$1,token_version=token_version+1,credential_ciphertext=$2,credential_nonce=$3,credential_expires_at=$4,refresh_claim=NULL,refresh_deadline=NULL,refresh_dispatched=false,selection_version=selection_version+1,booking_calendar=NULL,booking_label=NULL,blocking_calendars='[]',current_probe_id=NULL,permission_checked_at=NULL,capability_checked_at=NULL,notice='connected_reselect' WHERE id=1",vec![generation.into(),bytes.into(),nonce.into(),tokens.expires_at.into()])).await?;
  tx.execute(sql(
    "UPDATE google_auth_attempts SET outcome='connected' WHERE id=$1",
    vec![attempt.id.into()],
  ))
  .await?;
  // The old target is retained. A new grant never automatically cleans another account.
  tx.execute(sql("UPDATE google_probes SET phase='attention',cleanup_outcome='unresolved',reason='connection_changed',claim=NULL,claim_deadline=NULL WHERE phase NOT IN ('complete','attention') AND create_committed_at IS NOT NULL",vec![])).await?;
  tx.execute(sql("UPDATE google_probes SET phase='complete',cleanup_outcome='not_created',reason='connection_changed',claim=NULL,claim_deadline=NULL WHERE phase='prepared' AND create_committed_at IS NULL",vec![])).await?;
  tx.commit().await?;
  Ok(())
}
pub(super) fn decrypt(row: &QueryResult, config: &GoogleConfig) -> Result<Tokens, Error> {
  let bytes = crypto::decrypt(
    &config.key,
    "credentials",
    "1",
    row.try_get("", "generation")?,
    &row.try_get::<Vec<u8>>("", "credential_ciphertext")?,
    &row.try_get::<Vec<u8>>("", "credential_nonce")?,
  )?;
  serde_json::from_slice(&bytes).map_err(|_| Error::Unavailable)
}
