use base64::Engine;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct GoogleConfig {
  pub(super) client_id: String,
  pub(super) client_secret: String,
  pub(super) key: [u8; 32],
  pub(super) redirect: String,
  pub(super) fingerprint: Vec<u8>,
  #[cfg(test)]
  pub(super) test_endpoints: Option<(reqwest::Url, reqwest::Url)>,
}
impl GoogleConfig {
  pub(crate) fn same_key(&self, key: &[u8; 32]) -> bool {
    self.key == *key
  }
  pub fn parse(
    id: Option<&str>,
    secret: Option<&str>,
    key: Option<&str>,
    origin: Option<&str>,
  ) -> Result<Option<Self>, &'static str> {
    if id.is_none() && secret.is_none() && key.is_none() {
      return Ok(None);
    }
    const INVALID: &str = "Invalid Google configuration: require client ID, client secret, separate base64 32-byte token key and canonical HTTPS PUBLIC_ORIGIN";
    let (id, secret, key, origin) = match (id, secret, key, origin) {
      (Some(id), Some(secret), Some(key), Some(origin)) => (id, secret, key, origin),
      _ => return Err(INVALID),
    };
    if [id, secret]
      .iter()
      .any(|s| s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control) || s.trim() != *s)
    {
      return Err(INVALID);
    }
    crate::common::config::parse_public_origin(origin).map_err(|_| INVALID)?;
    let key: [u8; 32] = base64::engine::general_purpose::STANDARD
      .decode(key)
      .map_err(|_| INVALID)?
      .try_into()
      .map_err(|_| INVALID)?;
    let redirect = format!("{origin}/settings/google/callback");
    let fingerprint =
      Sha256::digest(format!("google-client:v1\0{id}\0{redirect}").as_bytes()).to_vec();
    Ok(Some(Self {
      client_id: id.into(),
      client_secret: secret.into(),
      key,
      redirect,
      fingerprint,
      #[cfg(test)]
      test_endpoints: None,
    }))
  }
}
