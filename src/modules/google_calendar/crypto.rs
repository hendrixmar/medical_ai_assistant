use super::Error;
use aes_gcm::{
  aead::{Aead, KeyInit, Payload},
  Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};

pub fn random<const N: usize>() -> Result<[u8; N], Error> {
  let mut value = [0; N];
  getrandom::fill(&mut value).map_err(|_| Error::Unavailable)?;
  Ok(value)
}
pub fn token() -> Result<String, Error> {
  Ok(URL_SAFE_NO_PAD.encode(random::<32>()?))
}
pub fn hash(value: &str) -> Vec<u8> {
  Sha256::digest(value.as_bytes()).to_vec()
}
pub fn challenge(verifier: &str) -> String {
  URL_SAFE_NO_PAD.encode(hash(verifier))
}
pub fn encrypt(
  key: &[u8; 32],
  purpose: &str,
  record: &str,
  generation: i64,
  value: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), Error> {
  let nonce = random::<12>()?;
  let aad = format!("medical-assistant:google:v1:{purpose}:{record}:{generation}");
  let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| Error::Unavailable)?;
  let bytes = cipher
    .encrypt(
      Nonce::from_slice(&nonce),
      Payload {
        msg: value,
        aad: aad.as_bytes(),
      },
    )
    .map_err(|_| Error::Unavailable)?;
  Ok((bytes, nonce.to_vec()))
}
pub fn decrypt(
  key: &[u8; 32],
  purpose: &str,
  record: &str,
  generation: i64,
  bytes: &[u8],
  nonce: &[u8],
) -> Result<Vec<u8>, Error> {
  if nonce.len() != 12 {
    return Err(Error::Unavailable);
  }
  let aad = format!("medical-assistant:google:v1:{purpose}:{record}:{generation}");
  Aes256Gcm::new_from_slice(key)
    .map_err(|_| Error::Unavailable)?
    .decrypt(
      Nonce::from_slice(nonce),
      Payload {
        msg: bytes,
        aad: aad.as_bytes(),
      },
    )
    .map_err(|_| Error::Unavailable)
}
