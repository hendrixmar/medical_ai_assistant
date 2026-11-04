use reqwest::header::HeaderValue;
#[cfg(test)]
use reqwest::Url;

#[derive(Clone)]
pub struct StripeConfig {
  pub(super) authorization: HeaderValue,
  pub(super) public_origin: String,
  #[cfg(test)]
  pub(super) test_endpoint: Option<Url>,
}

impl StripeConfig {
  pub fn parse(
    secret_key: Option<&str>,
    public_origin: Option<&str>,
  ) -> Result<Option<Self>, &'static str> {
    const INVALID: &str =
      "Invalid Stripe configuration: require a valid secret key and canonical HTTPS PUBLIC_ORIGIN";
    let Some(secret_key) = secret_key else {
      return Ok(None);
    };
    if secret_key.len() > 255
      || secret_key.len() <= 3
      || !(secret_key.starts_with("sk_") || secret_key.starts_with("rk_"))
      || !secret_key.bytes().all(|byte| byte.is_ascii_graphic())
    {
      return Err(INVALID);
    }
    let public_origin = public_origin.ok_or(INVALID)?;
    let public_origin =
      crate::common::config::parse_public_origin(public_origin).map_err(|_| INVALID)?;
    let mut authorization =
      HeaderValue::from_str(&format!("Bearer {secret_key}")).map_err(|_| INVALID)?;
    authorization.set_sensitive(true);
    Ok(Some(Self {
      authorization,
      public_origin,
      #[cfg(test)]
      test_endpoint: None,
    }))
  }
}
