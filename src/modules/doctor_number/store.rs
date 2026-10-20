use super::{crypto, Error};
use crate::{
  database::Db,
  modules::{
    doctor::Clock,
    whatsapp::{bound_transaction, inbox::sql},
  },
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult, TransactionTrait};
use uuid::Uuid;

pub async fn lock(tx: &DatabaseTransaction) -> Result<QueryResult, Error> {
  tx.query_one(sql(
    "SELECT * FROM doctor_number_state WHERE id=1 FOR UPDATE",
    vec![],
  ))
  .await?
  .ok_or(Error::Unavailable)
}

/// Resolve the stable fingerprint subkey: NULL/NULL is the pre-first-rotation
/// derivation from the current master; a present envelope must unwrap with it.
pub async fn resolve_fingerprint_key(
  state: &QueryResult,
  master: &[u8; 32],
) -> Result<[u8; 32], Error> {
  let ciphertext: Option<Vec<u8>> = state.try_get("", "fingerprint_key_ciphertext")?;
  let nonce: Option<Vec<u8>> = state.try_get("", "fingerprint_key_nonce")?;
  match (ciphertext, nonce) {
    (None, None) => crypto::fingerprint_key(master),
    (Some(ciphertext), Some(nonce)) => crypto::unwrap_fingerprint_key(master, &ciphertext, &nonce),
    _ => Err(Error::Unavailable),
  }
}

pub async fn rotate_key(
  tx: &DatabaseTransaction,
  old: &[u8; 32],
  new: &[u8; 32],
) -> Result<u64, Error> {
  let state = lock(tx).await?;
  let subkey = resolve_fingerprint_key(&state, old).await?;
  let (ciphertext, nonce) = crypto::wrap_fingerprint_key(new, &subkey)?;
  tx.execute(sql(
    "UPDATE doctor_number_state SET fingerprint_key_ciphertext=$1,fingerprint_key_nonce=$2 WHERE id=1",
    vec![ciphertext.into(), nonce.into()],
  ))
  .await?;
  let retired = tx
    .execute(sql(
      "UPDATE doctor_number_challenges SET state='superseded',ciphertext=NULL,nonce=NULL WHERE state='active'",
      vec![],
    ))
    .await?;
  Ok(retired.rows_affected())
}
async fn challenge(
  tx: &DatabaseTransaction,
  state: &QueryResult,
) -> Result<Option<QueryResult>, Error> {
  let id: Option<Uuid> = state.try_get("", "challenge_id")?;
  match id {
    Some(id) => Ok(
      tx.query_one(sql(
        "SELECT * FROM doctor_number_challenges WHERE id=$1 FOR UPDATE",
        vec![id.into()],
      ))
      .await?,
    ),
    None => Ok(None),
  }
}
pub fn retry_at(rows: &[i64], now: i64) -> Option<i64> {
  let minute = rows.first().map(|t| t + 60_000).filter(|t| *t > now);
  let hour = rows.get(4).map(|t| t + 3_600_000).filter(|t| *t > now);
  minute
    .into_iter()
    .chain(hour)
    .max()
    .map(|t| (t - now + 999) / 1000)
}
pub async fn issue(
  db: &Db,
  clock: &dyn Clock,
  key: &[u8; 32],
  practice: &str,
) -> Result<(), Error> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let state = lock(&tx).await?;
  let previous = challenge(&tx, &state).await?;
  let now = clock.now_millis();
  let history = tx.query_all(sql("SELECT issued_at FROM doctor_number_challenges WHERE issued_at>$1 ORDER BY issued_at DESC LIMIT 5",vec![(now-3_600_000).into()])).await?;
  let history: Vec<i64> = history
    .iter()
    .map(|r| r.try_get("", "issued_at"))
    .collect::<Result<_, _>>()?;
  if let Some(seconds) = retry_at(&history, now) {
    return Err(Error::Limited(seconds));
  }
  let subkey = resolve_fingerprint_key(&state, key).await?;
  let mut allocated = None;
  for _ in 0..32 {
    let id = Uuid::now_v7();
    let code = crypto::generate()?;
    let fingerprint = crypto::fingerprint(&subkey, &code)?;
    let (ciphertext, nonce) = crypto::encrypt(key, id, &code)?;
    let inserted = tx.execute(sql("INSERT INTO doctor_number_challenges(id,fingerprint,ciphertext,nonce,practice_phone,issued_at,expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (fingerprint) DO NOTHING",vec![id.into(),fingerprint.into(),ciphertext.into(),nonce.into(),practice.into(),now.into(),(now+600_000).into()])).await?;
    if inserted.rows_affected() == 1 {
      allocated = Some(id);
      break;
    }
  }
  let id = allocated.ok_or(Error::Unavailable)?;
  if let Some(previous) = previous {
    retire(&tx, previous.try_get("", "id")?, "superseded").await?;
  }
  tx.execute(sql(
    "UPDATE doctor_number_state SET challenge_id=$1 WHERE id=1",
    vec![id.into()],
  ))
  .await?;
  tx.commit().await?;
  Ok(())
}
async fn retire(tx: &DatabaseTransaction, id: Uuid, state: &str) -> Result<(), Error> {
  tx.execute(sql("UPDATE doctor_number_challenges SET state=$2,ciphertext=NULL,nonce=NULL WHERE id=$1 AND state='active'",vec![id.into(),state.into()])).await?;
  Ok(())
}

/// Exact phone identity, with only Mexico's precisely sized historical mobile form equivalent.
pub fn same_phone(left: &str, right: &str) -> bool {
  fn mexican(phone: &str) -> Option<&str> {
    let digits = phone.strip_suffix("@c.us")?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
      return None;
    }
    if digits.len() == 13 && digits.starts_with("521") {
      Some(&digits[3..])
    } else if digits.len() == 12 && digits.starts_with("52") {
      Some(&digits[2..])
    } else {
      None
    }
  }
  left == right
    || mexican(left)
      .zip(mexican(right))
      .is_some_and(|(a, b)| a == b)
}

pub struct Incoming<'a> {
  pub contact: Uuid,
  pub inbox: &'a str,
  pub original: &'a str,
  pub phone: Option<&'a str>,
  pub envelope: &'a serde_json::Value,
  pub text: Option<&'a str>,
}
/// Caller owns job, sorted aliases and Contacts. Never acquire Contacts after this call.
pub async fn recognize(
  tx: &DatabaseTransaction,
  clock: &dyn Clock,
  key: Option<&[u8; 32]>,
  input: Incoming<'_>,
) -> Result<Option<&'static str>, Error> {
  use crate::modules::{conversation::dispatch, whatsapp::inbox::phone_identity};
  use subtle::ConstantTimeEq;
  let state = lock(tx).await?;
  let active = challenge(tx, &state).await?;
  let now = clock.now_millis();
  let conflict = tx
    .query_one(sql(
      "SELECT identity_conflict FROM contacts WHERE id=$1",
      vec![input.contact.into()],
    ))
    .await?
    .ok_or(Error::Unavailable)?
    .try_get::<bool>("", "identity_conflict")?;
  let trusted =
    !conflict && dispatch::context(tx, input.contact, input.inbox, input.original).await?;
  if let Some(active) = active {
    let id: Uuid = active.try_get("", "id")?;
    let status: String = active.try_get("", "state")?;
    let expiry: i64 = active.try_get("", "expires_at")?;
    if status == "active" && now >= expiry {
      retire(tx, id, "expired").await?;
    }
    if status == "active" && now < expiry && trusted {
      let candidate = input
        .text
        .filter(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_digit()));
      let me = input
        .envelope
        .pointer("/me/id")
        .and_then(serde_json::Value::as_str)
        .and_then(phone_identity);
      if let (Some(code), Some(phone), Some(me), Some(key)) = (candidate, input.phone, me, key) {
        let practice: String = active.try_get("", "practice_phone")?;
        let issued: i64 = active.try_get("", "issued_at")?;
        let evidence = tx
          .query_one(sql(
            "SELECT arrived_at,server_timestamp FROM inbox_messages WHERE message_id=$1",
            vec![input.inbox.into()],
          ))
          .await?
          .ok_or(Error::Unavailable)?;
        let arrival = evidence
          .try_get::<chrono::DateTime<chrono::Utc>>("", "arrived_at")?
          .timestamp_millis();
        let server: i64 = evidence.try_get("", "server_timestamp")?;
        if same_phone(&practice, &me)
          && !same_phone(phone, &me)
          && arrival > issued
          && server >= issued.div_euclid(1000)
        {
          let expected: Vec<u8> = active.try_get("", "fingerprint")?;
          let subkey = resolve_fingerprint_key(&state, key).await?;
          let actual = crypto::fingerprint(&subkey, code)?;
          // Authentication time is independent of timely arrival and resampled at consumption.
          let consume_at = clock.now_millis();
          if consume_at >= expiry {
            retire(tx, id, "expired").await?;
          } else if bool::from(expected.ct_eq(&actual)) {
            tx.execute(sql("UPDATE doctor_number_state SET verified_phone=$1,original_target=$2,contact_id=$3,inbox_message_id=$4,verified_at=$5,practice_phone=$6 WHERE id=1",vec![phone.into(),input.original.into(),input.contact.into(),input.inbox.into(),consume_at.into(),me.into()])).await?;
            retire(tx, id, "used").await?;
            return Ok(Some("doctor_verified"));
          } else {
            tx.execute(sql(
              "UPDATE doctor_number_challenges SET failures=failures+1 WHERE id=$1",
              vec![id.into()],
            ))
            .await?;
            if active.try_get::<i32>("", "failures")? == 4 {
              retire(tx, id, "exhausted").await?;
            }
          }
        }
      }
    }
  }
  let binding: Option<String> = state.try_get("", "verified_phone")?;
  Ok(
    if trusted
      && input
        .phone
        .zip(binding.as_deref())
        .is_some_and(|(a, b)| same_phone(a, b))
    {
      Some("doctor_read_only")
    } else {
      None
    },
  )
}

pub struct Snapshot {
  pub verified_phone: Option<String>,
  pub verified_at: Option<String>,
  pub code: Option<String>,
  pub practice_phone: Option<String>,
  pub expires_at: Option<String>,
  pub state: String,
  pub retry: Option<i64>,
}
pub fn display_phone(jid: &str) -> String {
  format!("+{}", jid.trim_end_matches("@c.us"))
}
pub async fn load(db: &Db, clock: &dyn Clock, key: &[u8; 32]) -> Result<Snapshot, Error> {
  // One read snapshot for binding, challenge, timezone and persisted issuance history.
  let row=db.conn.query_one(sql("SELECT s.verified_phone,s.verified_at,c.id,c.state,c.ciphertext,c.nonce,c.practice_phone,c.expires_at,(SELECT timezone FROM practice WHERE id=1) AS timezone,ARRAY(SELECT issued_at FROM doctor_number_challenges ORDER BY issued_at DESC LIMIT 5) AS history FROM doctor_number_state s LEFT JOIN doctor_number_challenges c ON c.id=s.challenge_id WHERE s.id=1",vec![])).await?.ok_or(Error::Unavailable)?;
  let now = clock.now_millis();
  let zone = row
    .try_get::<Option<String>>("", "timezone")?
    .and_then(|s| s.parse::<chrono_tz::Tz>().ok())
    .unwrap_or(chrono_tz::UTC);
  let date = |ms: i64| -> Result<String, Error> {
    Ok(
      chrono::DateTime::from_timestamp_millis(ms)
        .ok_or(Error::Unavailable)?
        .with_timezone(&zone)
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string(),
    )
  };
  let mut state = row
    .try_get::<Option<String>>("", "state")?
    .unwrap_or_else(|| "none".into());
  let expires: Option<i64> = row.try_get("", "expires_at")?;
  if state == "active" && expires.is_none_or(|t| t <= now) {
    state = "expired".into();
  }
  let code = if state == "active" {
    Some(crypto::decrypt(
      key,
      row.try_get("", "id")?,
      &row.try_get::<Vec<u8>>("", "ciphertext")?,
      &row.try_get::<Vec<u8>>("", "nonce")?,
    )?)
  } else {
    None
  };
  Ok(Snapshot {
    verified_phone: row
      .try_get::<Option<String>>("", "verified_phone")?
      .map(|s| display_phone(&s)),
    verified_at: row
      .try_get::<Option<i64>>("", "verified_at")?
      .map(date)
      .transpose()?,
    code,
    practice_phone: row
      .try_get::<Option<String>>("", "practice_phone")?
      .map(|s| display_phone(&s)),
    expires_at: expires.map(date).transpose()?,
    state,
    retry: retry_at(&row.try_get::<Vec<i64>>("", "history")?, now),
  })
}
