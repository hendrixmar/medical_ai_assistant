use super::Error;
use aes_gcm::{
  aead::{Aead, KeyInit, Payload},
  Aes256Gcm, Nonce,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

fn key(master: &[u8; 32], purpose: &[u8]) -> Result<[u8; 32], Error> {
  let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(master).map_err(|_| Error::Unavailable)?;
  mac.update(purpose);
  Ok(mac.finalize().into_bytes().into())
}
pub fn fingerprint_key(master: &[u8; 32]) -> Result<[u8; 32], Error> {
  key(master, b"medical-assistant:doctor-number:fingerprint:v1")
}
pub fn fingerprint(subkey: &[u8; 32], code: &str) -> Result<Vec<u8>, Error> {
  let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(subkey).map_err(|_| Error::Unavailable)?;
  mac.update(code.as_bytes());
  Ok(mac.finalize().into_bytes().to_vec())
}
const FINGERPRINT_WRAP_AAD: &[u8] = b"medical-assistant:doctor-number:1:fingerprint:v1";
pub fn wrap_fingerprint_key(
  master: &[u8; 32],
  subkey: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>), Error> {
  let wrap_key = key(
    master,
    b"medical-assistant:doctor-number:fingerprint-wrap:v1",
  )?;
  let cipher = Aes256Gcm::new_from_slice(&wrap_key).map_err(|_| Error::Unavailable)?;
  let mut nonce = [0; 12];
  getrandom::fill(&mut nonce).map_err(|_| Error::Unavailable)?;
  let ciphertext = cipher
    .encrypt(
      Nonce::from_slice(&nonce),
      Payload {
        msg: subkey.as_slice(),
        aad: FINGERPRINT_WRAP_AAD,
      },
    )
    .map_err(|_| Error::Unavailable)?;
  Ok((ciphertext, nonce.to_vec()))
}
pub fn unwrap_fingerprint_key(
  master: &[u8; 32],
  ciphertext: &[u8],
  nonce: &[u8],
) -> Result<[u8; 32], Error> {
  if nonce.len() != 12 {
    return Err(Error::Unavailable);
  }
  let wrap_key = key(
    master,
    b"medical-assistant:doctor-number:fingerprint-wrap:v1",
  )?;
  let cipher = Aes256Gcm::new_from_slice(&wrap_key).map_err(|_| Error::Unavailable)?;
  let plain = cipher
    .decrypt(
      Nonce::from_slice(nonce),
      Payload {
        msg: ciphertext,
        aad: FINGERPRINT_WRAP_AAD,
      },
    )
    .map_err(|_| Error::Unavailable)?;
  plain.try_into().map_err(|_| Error::Unavailable)
}
pub fn generate() -> Result<String, Error> {
  loop {
    let mut bytes = [0; 4];
    getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
    let sample = u32::from_le_bytes(bytes);
    if sample < u32::MAX - u32::MAX % 1_000_000 {
      return Ok(format!("{:06}", sample % 1_000_000));
    }
  }
}
pub fn encrypt(master: &[u8; 32], id: Uuid, code: &str) -> Result<(Vec<u8>, Vec<u8>), Error> {
  let key = key(master, b"medical-assistant:doctor-number:encryption:v1")?;
  let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| Error::Unavailable)?;
  let mut nonce = [0; 12];
  getrandom::fill(&mut nonce).map_err(|_| Error::Unavailable)?;
  let aad = format!("medical-assistant:doctor-number:challenge:{id}:v1");
  let ciphertext = cipher
    .encrypt(
      Nonce::from_slice(&nonce),
      Payload {
        msg: code.as_bytes(),
        aad: aad.as_bytes(),
      },
    )
    .map_err(|_| Error::Unavailable)?;
  Ok((ciphertext, nonce.to_vec()))
}
pub fn decrypt(
  master: &[u8; 32],
  id: Uuid,
  ciphertext: &[u8],
  nonce: &[u8],
) -> Result<String, Error> {
  if nonce.len() != 12 {
    return Err(Error::Unavailable);
  }
  let key = key(master, b"medical-assistant:doctor-number:encryption:v1")?;
  let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| Error::Unavailable)?;
  let aad = format!("medical-assistant:doctor-number:challenge:{id}:v1");
  let plain = cipher
    .decrypt(
      Nonce::from_slice(nonce),
      Payload {
        msg: ciphertext,
        aad: aad.as_bytes(),
      },
    )
    .map_err(|_| Error::Unavailable)?;
  let code = String::from_utf8(plain).map_err(|_| Error::Unavailable)?;
  if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
    return Err(Error::Unavailable);
  }
  Ok(code)
}
