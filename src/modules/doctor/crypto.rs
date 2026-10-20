use super::Error;
use aes_gcm::{
  aead::{Aead, KeyInit, Payload},
  Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use totp_rs::{Builder, Secret, Totp};

const AAD: &[u8] = b"medical-assistant:doctor:1:totp:v1";
static BCRYPT_SLOTS: Semaphore = Semaphore::const_new(2);

pub fn random<const N: usize>() -> Result<[u8; N], Error> {
  let mut value = [0; N];
  getrandom::fill(&mut value).map_err(|_| Error::Unavailable)?;
  Ok(value)
}

pub fn token() -> Result<String, Error> {
  Ok(URL_SAFE_NO_PAD.encode(random::<32>()?))
}
pub fn hash(token: &str) -> Vec<u8> {
  Sha256::digest(token.as_bytes()).to_vec()
}
pub fn equal(left: &str, right: &str) -> bool {
  bool::from(left.as_bytes().ct_eq(right.as_bytes()))
}

pub fn totp(secret: [u8; 20]) -> Result<Totp, Error> {
  Builder::new()
    .with_secret(Secret::from(secret))
    .with_step_duration(30)
    .with_skew(1)
    .with_issuer(Some("Medical Assistant"))
    .with_account_name("Doctor")
    .build()
    .map_err(|_| Error::Unavailable)
}

pub fn accepted_step(secret: [u8; 20], code: &str, now: i64) -> Result<i64, Error> {
  let now = u64::try_from(now).map_err(|_| Error::Unavailable)?;
  totp(secret)?
    .check(code, now)
    .and_then(|step| i64::try_from(step).ok())
    .ok_or(Error::Invalid)
}

pub fn encrypt(key: &[u8; 32], secret: &[u8; 20]) -> Result<(Vec<u8>, Vec<u8>), Error> {
  let nonce = random::<12>()?;
  let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| Error::Unavailable)?;
  let encrypted = cipher
    .encrypt(
      Nonce::from_slice(&nonce),
      Payload {
        msg: secret,
        aad: AAD,
      },
    )
    .map_err(|_| Error::Unavailable)?;
  Ok((encrypted, nonce.to_vec()))
}

pub fn decrypt(key: &[u8; 32], encrypted: &[u8], nonce: &[u8]) -> Result<[u8; 20], Error> {
  if nonce.len() != 12 {
    return Err(Error::Unavailable);
  }
  let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| Error::Unavailable)?;
  cipher
    .decrypt(
      Nonce::from_slice(nonce),
      Payload {
        msg: encrypted,
        aad: AAD,
      },
    )
    .map_err(|_| Error::Unavailable)?
    .try_into()
    .map_err(|_| Error::Unavailable)
}

pub fn valid_password(password: &str) -> bool {
  password.chars().count() >= 12 && password.len() <= 72
}

pub async fn password_hash(password: String, cost: u32) -> Result<String, Error> {
  if !valid_password(&password) || !(4..=31).contains(&cost) {
    return Err(Error::Invalid);
  }
  let permit = BCRYPT_SLOTS
    .acquire()
    .await
    .map_err(|_| Error::Unavailable)?;
  tokio::task::spawn_blocking(move || {
    let _permit = permit;
    bcrypt::hash(password, cost).map_err(|_| Error::Unavailable)
  })
  .await
  .map_err(|_| Error::Unavailable)?
}

pub async fn password_matches(password: String, hash: String) -> Result<bool, Error> {
  if !valid_password(&password) {
    return Ok(false);
  }
  let permit = BCRYPT_SLOTS
    .acquire()
    .await
    .map_err(|_| Error::Unavailable)?;
  tokio::task::spawn_blocking(move || {
    let _permit = permit;
    bcrypt::verify(password, &hash).map_err(|_| Error::Unavailable)
  })
  .await
  .map_err(|_| Error::Unavailable)?
}
