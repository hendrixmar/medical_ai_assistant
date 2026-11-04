use super::{config::StripeConfig, Error};
use reqwest::{
  header::{HeaderValue, ACCEPT, AUTHORIZATION},
  redirect::Policy,
  Method, Url,
};
use serde::{de::DeserializeOwned, Deserialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CHECKOUT_SESSIONS: usize = 3;
const HOSTED_CONFIRMATION: &str = "Pago recibido. Recibirás la confirmación por WhatsApp.";

#[derive(Clone)]
pub struct Client {
  http: reqwest::Client,
  base: Url,
  authorization: HeaderValue,
  public_origin: String,
}

#[derive(PartialEq, Eq)]
pub struct ConnectedAccount {
  pub id: String,
}

#[derive(PartialEq, Eq)]
pub struct AccountStatus {
  pub charges_enabled: bool,
  pub details_submitted: bool,
}

pub struct PaymentLinkRequest<'a> {
  pub account_id: &'a str,
  pub appointment_id: uuid::Uuid,
  pub revision: i64,
  pub amount_centavos: i64,
  pub local_start_time: &'a str,
  pub payment_reference: &'a str,
}

#[derive(PartialEq, Eq)]
pub struct CreatedPaymentLink {
  pub price_id: String,
  pub link_id: String,
  pub url: String,
}

#[derive(Deserialize, PartialEq, Eq)]
pub struct CheckoutSession {
  pub id: String,
  pub payment_status: String,
  pub status: String,
  pub currency: Option<String>,
  pub amount_total: Option<i64>,
}

#[derive(Deserialize)]
struct IdResponse {
  id: String,
}

#[derive(Deserialize)]
struct AccountLinkResponse {
  url: String,
}

#[derive(Deserialize)]
struct AccountStatusResponse {
  charges_enabled: bool,
  details_submitted: bool,
}

#[derive(Deserialize)]
struct PaymentLinkResponse {
  id: String,
  url: String,
}

#[derive(Deserialize)]
struct CheckoutSessionList {
  data: Vec<CheckoutSession>,
}

impl Client {
  pub fn new(config: &StripeConfig) -> Result<Self, &'static str> {
    let http = reqwest::Client::builder()
      .no_proxy()
      .redirect(Policy::none())
      .retry(reqwest::retry::never())
      .connect_timeout(Duration::from_secs(2))
      .timeout(Duration::from_secs(5))
      .build()
      .map_err(|_| "Unable to initialize Stripe HTTP client")?;
    let base = Url::parse("https://api.stripe.com/v1/").map_err(|_| "Invalid Stripe endpoint")?;
    #[cfg(test)]
    let base = config.test_endpoint.clone().unwrap_or(base);
    Ok(Self {
      http,
      base,
      authorization: config.authorization.clone(),
      public_origin: config.public_origin.clone(),
    })
  }

  pub async fn create_account(&self) -> Result<ConnectedAccount, Error> {
    let bytes = self
      .send(
        self
          .request(Method::POST, self.url(&["accounts"])?, None)
          .header(
            "Idempotency-Key",
            idempotency_key(&self.public_origin, 0, "connected_account"),
          )
          .form(&[
            ("country", "MX"),
            ("controller[stripe_dashboard][type]", "full"),
            ("controller[fees][payer]", "account"),
            ("controller[losses][payments]", "stripe"),
            ("controller[requirement_collection]", "stripe"),
          ]),
      )
      .await?;
    let response: IdResponse = parse_json(&bytes)?;
    if !valid_stripe_id(&response.id, "acct_") {
      return Err(Error::Unavailable);
    }
    Ok(ConnectedAccount { id: response.id })
  }

  pub async fn create_account_link(&self, account_id: &str) -> Result<String, Error> {
    if !valid_stripe_id(account_id, "acct_") {
      return Err(Error::Invalid);
    }
    let refresh_url = format!("{}/settings/stripe/refresh", self.public_origin);
    let return_url = format!("{}/settings/stripe/return", self.public_origin);
    let bytes = self
      .send(
        self
          .request(Method::POST, self.url(&["account_links"])?, None)
          .form(&[
            ("account", account_id),
            ("refresh_url", refresh_url.as_str()),
            ("return_url", return_url.as_str()),
            ("type", "account_onboarding"),
          ]),
      )
      .await?;
    let response: AccountLinkResponse = parse_json(&bytes)?;
    hosted_url(&response.url, Some("connect.stripe.com")).ok_or(Error::Unavailable)
  }

  pub async fn retrieve_account(&self, account_id: &str) -> Result<AccountStatus, Error> {
    if !valid_stripe_id(account_id, "acct_") {
      return Err(Error::Invalid);
    }
    let bytes = self
      .send(self.request(Method::GET, self.url(&["accounts", account_id])?, None))
      .await?;
    let response: AccountStatusResponse = parse_json(&bytes)?;
    Ok(AccountStatus {
      charges_enabled: response.charges_enabled,
      details_submitted: response.details_submitted,
    })
  }

  pub async fn create_payment_link(
    &self,
    request: PaymentLinkRequest<'_>,
  ) -> Result<CreatedPaymentLink, Error> {
    if !valid_stripe_id(request.account_id, "acct_")
      || request.revision <= 0
      || request.amount_centavos <= 0
      || !valid_metadata_value(request.payment_reference, 500)
      || !valid_metadata_value(request.local_start_time, 220)
    {
      return Err(Error::Invalid);
    }

    let product_name = format!("Consulta por video - {}", request.local_start_time);
    let appointment_id = request.appointment_id.to_string();
    let amount_centavos = request.amount_centavos.to_string();
    let price_idempotency = idempotency_key(&appointment_id, request.revision, "price");
    let price_bytes = self
      .send(
        self
          .request(
            Method::POST,
            self.url(&["prices"])?,
            Some(request.account_id),
          )
          .header("Idempotency-Key", price_idempotency)
          .form(&[
            ("currency", "mxn"),
            ("unit_amount", amount_centavos.as_str()),
            ("product_data[name]", product_name.as_str()),
          ]),
      )
      .await?;
    let price: IdResponse = parse_json(&price_bytes)?;
    if !valid_stripe_id(&price.id, "price_") {
      return Err(Error::Unavailable);
    }

    let revision = request.revision.to_string();
    let link_idempotency = idempotency_key(&appointment_id, request.revision, "payment_link");
    let link_bytes = self
      .send(
        self
          .request(
            Method::POST,
            self.url(&["payment_links"])?,
            Some(request.account_id),
          )
          .header("Idempotency-Key", link_idempotency)
          .form(&[
            ("line_items[0][price]", price.id.as_str()),
            ("line_items[0][quantity]", "1"),
            ("payment_method_types[0]", "card"),
            ("restrictions[completed_sessions][limit]", "1"),
            ("after_completion[type]", "hosted_confirmation"),
            (
              "after_completion[hosted_confirmation][custom_message]",
              HOSTED_CONFIRMATION,
            ),
            ("metadata[appointment_id]", appointment_id.as_str()),
            ("metadata[revision]", revision.as_str()),
            ("metadata[payment_reference]", request.payment_reference),
          ]),
      )
      .await?;
    let link: PaymentLinkResponse = parse_json(&link_bytes)?;
    if !valid_stripe_id(&link.id, "plink_") {
      return Err(Error::Unavailable);
    }
    let url = hosted_url(&link.url, Some("buy.stripe.com")).ok_or(Error::Unavailable)?;
    Ok(CreatedPaymentLink {
      price_id: price.id,
      link_id: link.id,
      url,
    })
  }

  pub async fn list_checkout_sessions(
    &self,
    account_id: &str,
    payment_link_id: &str,
  ) -> Result<Vec<CheckoutSession>, Error> {
    if !valid_stripe_id(account_id, "acct_") || !valid_stripe_id(payment_link_id, "plink_") {
      return Err(Error::Invalid);
    }
    let mut url = self.url(&["checkout", "sessions"])?;
    url
      .query_pairs_mut()
      .append_pair("payment_link", payment_link_id)
      .append_pair("limit", "3");
    let bytes = self
      .send(self.request(Method::GET, url, Some(account_id)))
      .await?;
    let response: CheckoutSessionList = parse_json(&bytes)?;
    if response.data.len() > MAX_CHECKOUT_SESSIONS
      || response
        .data
        .iter()
        .any(|session| !valid_stripe_id(&session.id, "cs_"))
    {
      return Err(Error::Unavailable);
    }
    Ok(response.data)
  }

  pub async fn deactivate_payment_link(
    &self,
    account_id: &str,
    payment_link_id: &str,
  ) -> Result<(), Error> {
    if !valid_stripe_id(account_id, "acct_") || !valid_stripe_id(payment_link_id, "plink_") {
      return Err(Error::Invalid);
    }
    self
      .send(
        self
          .request(
            Method::POST,
            self.url(&["payment_links", payment_link_id])?,
            Some(account_id),
          )
          .form(&[("active", "false")]),
      )
      .await?;
    Ok(())
  }

  fn request(&self, method: Method, url: Url, account_id: Option<&str>) -> reqwest::RequestBuilder {
    let request = self
      .http
      .request(method, url)
      .header(AUTHORIZATION, self.authorization.clone())
      .header(ACCEPT, "application/json");
    match account_id {
      Some(account_id) => request.header("Stripe-Account", account_id),
      None => request,
    }
  }

  fn url(&self, segments: &[&str]) -> Result<Url, Error> {
    let mut url = self.base.clone();
    url
      .path_segments_mut()
      .map_err(|_| Error::Unavailable)?
      .pop_if_empty()
      .extend(segments);
    Ok(url)
  }

  async fn send(&self, request: reqwest::RequestBuilder) -> Result<Vec<u8>, Error> {
    let mut response = request.send().await.map_err(|_| Error::Unavailable)?;
    if !response.status().is_success() {
      return Err(if response.status().is_client_error() {
        Error::Rejected
      } else {
        Error::Unavailable
      });
    }
    if response
      .content_length()
      .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
      return Err(Error::Unavailable);
    }
    let mut bytes = Vec::with_capacity(
      response
        .content_length()
        .unwrap_or(0)
        .min(MAX_RESPONSE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
      if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
        return Err(Error::Unavailable);
      }
      bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
  }
}

fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
  serde_json::from_slice(bytes).map_err(|_| Error::Unavailable)
}

fn valid_stripe_id(value: &str, prefix: &str) -> bool {
  value.starts_with(prefix)
    && value.len() > prefix.len()
    && value.len() <= 255
    && value
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

fn valid_metadata_value(value: &str, max_bytes: usize) -> bool {
  !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn hosted_url(value: &str, expected_host: Option<&str>) -> Option<String> {
  if value.len() > 2048 || value.chars().any(char::is_control) {
    return None;
  }
  let url = Url::parse(value).ok()?;
  if url.scheme() != "https"
    || url.host_str().is_none()
    || expected_host.is_some_and(|host| url.host_str() != Some(host))
    || !url.username().is_empty()
    || url.password().is_some()
    || url.port().is_some()
    || url.fragment().is_some()
    || url.path() == "/"
  {
    return None;
  }
  Some(url.into())
}

pub(super) fn idempotency_key(identity: &str, revision: i64, purpose: &str) -> String {
  let mut hash = Sha256::new();
  hash.update(b"medical-assistant:stripe-connect:v1\0");
  hash.update((identity.len() as u64).to_be_bytes());
  hash.update(identity.as_bytes());
  hash.update(revision.to_be_bytes());
  hash.update(purpose.as_bytes());
  let digest = hash.finalize();
  let mut key = String::with_capacity(30 + digest.len() * 2);
  key.push_str("medical-assistant-stripe-v1-");
  const HEX: &[u8; 16] = b"0123456789abcdef";
  for byte in digest {
    key.push(HEX[(byte >> 4) as usize] as char);
    key.push(HEX[(byte & 0x0f) as usize] as char);
  }
  key
}
