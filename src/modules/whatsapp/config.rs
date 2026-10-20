use reqwest::{header::HeaderValue, Url};

/// Secrets deliberately have no Debug/Serialize implementation.
pub struct WahaConfig {
  pub(super) base: Url,
  pub(super) api_key: HeaderValue,
  pub(super) hmac_key: String,
}

impl WahaConfig {
  pub fn parse(
    base: Option<&str>,
    api_key: Option<&str>,
    hmac_key: Option<&str>,
  ) -> Result<Option<Self>, &'static str> {
    const INVALID: &str = "Invalid WAHA configuration: require canonical origin, API key and HMAC key (at least 32 characters)";
    let (base, api_key, hmac_key) = match (base, api_key, hmac_key) {
      (None, None, None) => return Ok(None),
      (Some(base), Some(api_key), Some(hmac_key)) => (base, api_key, hmac_key),
      _ => return Err(INVALID),
    };
    let url = Url::parse(base).map_err(|_| INVALID)?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && local))
      || url.host_str().is_none()
      || !url.username().is_empty()
      || url.password().is_some()
      || url.query().is_some()
      || url.fragment().is_some()
      || url.path() != "/"
      || url.origin().ascii_serialization() != base
      || api_key.trim().is_empty()
      || hmac_key.chars().count() < 32
      || hmac_key.trim().is_empty()
    {
      return Err(INVALID);
    }
    let mut api_key = HeaderValue::from_str(api_key).map_err(|_| INVALID)?;
    api_key.set_sensitive(true);
    Ok(Some(Self {
      base: url,
      api_key,
      hmac_key: hmac_key.to_owned(),
    }))
  }
}
