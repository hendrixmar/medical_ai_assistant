use super::{crypto, Error};
use crate::database::Db;
use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement, TransactionTrait, Value};

pub const SESSION_SECONDS: i64 = 30 * 24 * 60 * 60;
pub const CHALLENGE_SECONDS: i64 = 600;

pub fn sql(query: &str, values: Vec<Value>) -> Statement {
  Statement::from_sql_and_values(DbBackend::Postgres, query, values)
}

pub async fn doctor(db: &Db) -> Result<Option<QueryResult>, Error> {
  Ok(
    db.conn
      .query_one(sql(
        "SELECT password_hash, totp_ciphertext, totp_nonce FROM doctor WHERE id = 1",
        vec![],
      ))
      .await?,
  )
}

pub async fn insert_doctor(
  db: &Db,
  password: String,
  ciphertext: Vec<u8>,
  nonce: Vec<u8>,
  step: i64,
) -> Result<(), Error> {
  let result = db.conn.execute(sql("INSERT INTO doctor (id, password_hash, totp_ciphertext, totp_nonce, last_used_totp_step) VALUES (1, $1, $2, $3, $4) ON CONFLICT (id) DO NOTHING", vec![password.into(), ciphertext.into(), nonce.into(), step.into()])).await?;
  if result.rows_affected() != 1 {
    return Err(Error::Exists);
  }
  Ok(())
}

pub async fn challenge(db: &Db, now: i64) -> Result<(String, String), Error> {
  let tx = db.conn.begin().await?;
  tx.execute(sql(
    "DELETE FROM doctor_login_challenges WHERE expires_at <= $1",
    vec![now.into()],
  ))
  .await?;
  tx.execute(sql(
    "DELETE FROM doctor_sessions WHERE expires_at <= $1",
    vec![now.into()],
  ))
  .await?;
  let row = tx
    .query_one(sql(
      "UPDATE doctor_login_limit SET
    issued = CASE WHEN issue_start + 300 <= $1 THEN 1 ELSE issued + 1 END,
    issue_start = CASE WHEN issue_start + 300 <= $1 THEN $1 ELSE issue_start END
    WHERE id = 1 AND (issue_start + 300 <= $1 OR issued < 20) RETURNING id",
      vec![now.into()],
    ))
    .await?;
  if row.is_none() {
    return Err(Error::Limited(300));
  }
  let token = crypto::token()?;
  let csrf = crypto::token()?;
  tx.execute(sql(
    "INSERT INTO doctor_login_challenges (token_hash, csrf_token, expires_at) VALUES ($1, $2, $3)",
    vec![
      crypto::hash(&token).into(),
      csrf.clone().into(),
      (now + CHALLENGE_SECONDS).into(),
    ],
  ))
  .await?;
  tx.commit().await?;
  Ok((token, csrf))
}

pub async fn valid_challenge(db: &Db, token: &str, csrf: &str, now: i64) -> Result<bool, Error> {
  let row = db
    .conn
    .query_one(sql(
      "SELECT csrf_token FROM doctor_login_challenges WHERE token_hash = $1 AND expires_at > $2",
      vec![crypto::hash(token).into(), now.into()],
    ))
    .await?;
  match row {
    Some(row) => Ok(crypto::equal(
      &row.try_get::<String>("", "csrf_token")?,
      csrf,
    )),
    None => Ok(false),
  }
}

// The singleton row serializes reservations across processes, before any bcrypt work.
pub async fn reserve_attempt(db: &Db, now: i64) -> Result<(), Error> {
  let tx = db.conn.begin().await?;
  let row = tx
    .query_one(sql(
      "SELECT window_start, attempts FROM doctor_login_limit WHERE id = 1 FOR UPDATE",
      vec![],
    ))
    .await?
    .ok_or(Error::Unavailable)?;
  let start: i64 = row.try_get("", "window_start")?;
  let attempts: i32 = row.try_get("", "attempts")?;
  if start + 300 > now && attempts >= 5 {
    return Err(Error::Limited((start + 300 - now).max(1)));
  }
  tx.execute(sql(
    "UPDATE doctor_login_limit SET window_start = $1, attempts = $2 WHERE id = 1",
    vec![
      if start + 300 <= now { now } else { start }.into(),
      if start + 300 <= now { 1 } else { attempts + 1 }.into(),
    ],
  ))
  .await?;
  tx.commit().await?;
  Ok(())
}

pub async fn create_session(
  db: &Db,
  step: i64,
  challenge_token: &str,
  now: i64,
) -> Result<String, Error> {
  let tx = db.conn.begin().await?;
  let result = tx
    .execute(sql(
      "UPDATE doctor SET last_used_totp_step = $1 WHERE id = 1 AND last_used_totp_step < $1",
      vec![step.into()],
    ))
    .await?;
  if result.rows_affected() != 1 {
    return Err(Error::Invalid);
  }
  let result = tx
    .execute(sql(
      "DELETE FROM doctor_login_challenges WHERE token_hash = $1 AND expires_at > $2",
      vec![crypto::hash(challenge_token).into(), now.into()],
    ))
    .await?;
  if result.rows_affected() != 1 {
    return Err(Error::Invalid);
  }
  let token = crypto::token()?;
  let csrf = crypto::token()?;
  tx.execute(sql("INSERT INTO doctor_sessions (token_hash, csrf_token, created_at, expires_at) VALUES ($1, $2, $3, $4)", vec![crypto::hash(&token).into(), csrf.into(), now.into(), (now + SESSION_SECONDS).into()])).await?;
  tx.commit().await?;
  Ok(token)
}

#[derive(Clone)]
pub struct Session {
  pub hash: Vec<u8>,
  pub csrf: String,
}

pub async fn session(db: &Db, token: &str, now: i64) -> Result<Option<Session>, Error> {
  let hash = crypto::hash(token);
  let row = db
    .conn
    .query_one(sql(
      "SELECT csrf_token FROM doctor_sessions WHERE token_hash = $1 AND expires_at > $2",
      vec![hash.clone().into(), now.into()],
    ))
    .await?;
  row
    .map(|row| {
      Ok(Session {
        hash,
        csrf: row.try_get("", "csrf_token")?,
      })
    })
    .transpose()
}

pub async fn logout(db: &Db, session: &Session) -> Result<(), Error> {
  db.conn
    .execute(sql(
      "DELETE FROM doctor_sessions WHERE token_hash = $1",
      vec![session.hash.clone().into()],
    ))
    .await?;
  Ok(())
}
