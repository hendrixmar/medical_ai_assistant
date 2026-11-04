use reqwest::header::HeaderValue;
#[cfg(test)]
use reqwest::Url;

#[derive(Clone)]
pub struct MercadoPagoConfig {
  pub(super) authorization: HeaderValue,
  #[cfg(test)]
  pub(super) test_endpoint: Option<Url>,
}

impl MercadoPagoConfig {
  pub fn parse(
    access_token: Option<&str>,
    public_origin: Option<&str>,
  ) -> Result<Option<Self>, &'static str> {
    const INVALID: &str = "Invalid Mercado Pago configuration: require a valid access token and canonical HTTPS PUBLIC_ORIGIN";
    let Some(access_token) = access_token else {
      return Ok(None);
    };
    let prefix_len = if access_token.starts_with("APP_USR-") {
      8
    } else if access_token.starts_with("TEST-") {
      5
    } else {
      return Err(INVALID);
    };
    if access_token.len() > 255
      || access_token.len() <= prefix_len
      || !access_token.bytes().all(|byte| byte.is_ascii_graphic())
    {
      return Err(INVALID);
    }
    // The outbox needs the canonical origin to send the payment URL.
    crate::common::config::parse_public_origin(public_origin.ok_or(INVALID)?)
      .map_err(|_| INVALID)?;
    let mut authorization =
      HeaderValue::from_str(&format!("Bearer {access_token}")).map_err(|_| INVALID)?;
    authorization.set_sensitive(true);
    Ok(Some(Self {
      authorization,
      #[cfg(test)]
      test_endpoint: None,
    }))
  }
}
