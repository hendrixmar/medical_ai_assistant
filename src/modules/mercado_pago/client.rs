use super::{config::MercadoPagoConfig, Error};
use reqwest::{
  header::{HeaderValue, ACCEPT, AUTHORIZATION},
  redirect::Policy,
  Method, Url,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Number, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_SEARCH_RESULTS: usize = 50;

#[derive(Clone)]
pub struct Client {
  http: reqwest::Client,
  base: Url,
  authorization: HeaderValue,
}

pub struct PreferenceRequest<'a> {
  pub appointment_id: uuid::Uuid,
  pub revision: i64,
  pub external_reference: &'a str,
  pub amount_centavos: i64,
  pub local_start_time: &'a str,
  pub created_at: i64,
  pub expires_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CreatedPreference {
  pub id: String,
  pub url: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct PaymentSummary {
  pub id: i64,
  pub status: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Payment {
  pub id: i64,
  pub status: String,
  pub currency_id: Option<String>,
  /// `None` when the provider amount is not an exact non-negative peso value with at most two decimals.
  pub amount_centavos: Option<i64>,
  pub external_reference: Option<String>,
  pub collector_id: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Refund {
  pub id: i64,
  pub status: String,
}

#[derive(Deserialize)]
struct UserResponse {
  id: i64,
}

#[derive(Deserialize)]
struct PreferenceResponse {
  id: String,
  init_point: String,
}

#[derive(Deserialize)]
struct SearchResponse {
  results: Vec<PaymentSummary>,
}

#[derive(Deserialize)]
struct PaymentResponse {
  id: i64,
  status: String,
  currency_id: Option<String>,
  transaction_amount: Option<Number>,
  external_reference: Option<String>,
  collector_id: Option<i64>,
}

#[derive(Deserialize)]
struct RefundResponse {
  id: i64,
  status: String,
}

impl Client {
  pub fn new(config: &MercadoPagoConfig) -> Result<Self, &'static str> {
    let http = reqwest::Client::builder()
      .no_proxy()
      .redirect(Policy::none())
      .retry(reqwest::retry::never())
      .connect_timeout(Duration::from_secs(2))
      .timeout(Duration::from_secs(5))
      .build()
      .map_err(|_| "Unable to initialize Mercado Pago HTTP client")?;
    let base =
      Url::parse("https://api.mercadopago.com/").map_err(|_| "Invalid Mercado Pago endpoint")?;
    #[cfg(test)]
    let base = config.test_endpoint.clone().unwrap_or(base);
    Ok(Self {
      http,
      base,
      authorization: config.authorization.clone(),
    })
  }

  /// The Mercado Pago user that owns the access token; every payment must name it as collector.
  pub async fn user_id(&self) -> Result<i64, Error> {
    let bytes = self
      .send(self.request(Method::GET, self.url(&["users", "me"])?))
      .await?;
    let user: UserResponse = parse_json(&bytes)?;
    (user.id > 0).then_some(user.id).ok_or(Error::Unavailable)
  }

  pub async fn create_preference(
    &self,
    request: PreferenceRequest<'_>,
  ) -> Result<CreatedPreference, Error> {
    let (Some(price), Some(from), Some(to)) = (
      pesos(request.amount_centavos),
      iso_time(request.created_at),
      iso_time(request.expires_at),
    ) else {
      return Err(Error::Invalid);
    };
    if request.revision <= 0
      || request.amount_centavos <= 0
      || request.expires_at <= request.created_at
      || !valid_text(request.external_reference, 256)
      || !valid_text(request.local_start_time, 200)
    {
      return Err(Error::Invalid);
    }
    let body = json!({
      "items": [{
        "id": "consulta-video",
        "title": format!("Consulta por video - {}", request.local_start_time),
        "quantity": 1,
        "currency_id": "MXN",
        "unit_price": price,
      }],
      "external_reference": request.external_reference,
      "expires": true,
      "expiration_date_from": from,
      "expiration_date_to": to,
      // Immediate methods only (ADR 0006): cash and bank transfer outlive the Receipt window.
      "payment_methods": {
        "excluded_payment_types": [{"id": "ticket"}, {"id": "atm"}, {"id": "bank_transfer"}],
        "installments": 1,
      },
      "binary_mode": true,
    });
    let key = idempotency_key(
      &request.appointment_id.to_string(),
      request.revision,
      "preference",
    );
    let bytes = self
      .send(
        self
          .request(Method::POST, self.url(&["checkout", "preferences"])?)
          .header("X-Idempotency-Key", key)
          .json(&body),
      )
      .await?;
    let response: PreferenceResponse = parse_json(&bytes)?;
    if !valid_preference_id(&response.id) {
      return Err(Error::Unavailable);
    }
    let url = checkout_url(&response.init_point).ok_or(Error::Unavailable)?;
    Ok(CreatedPreference {
      id: response.id,
      url,
    })
  }

  pub async fn expire_preference(&self, preference_id: &str, at: i64) -> Result<(), Error> {
    let Some(at) = iso_time(at) else {
      return Err(Error::Invalid);
    };
    if !valid_preference_id(preference_id) {
      return Err(Error::Invalid);
    }
    self
      .send(
        self
          .request(
            Method::PUT,
            self.url(&["checkout", "preferences", preference_id])?,
          )
          .json(&json!({"expires": true, "expiration_date_to": at})),
      )
      .await?;
    Ok(())
  }

  pub async fn search_payments(
    &self,
    external_reference: &str,
  ) -> Result<Vec<PaymentSummary>, Error> {
    if !valid_text(external_reference, 256) {
      return Err(Error::Invalid);
    }
    let mut url = self.url(&["v1", "payments", "search"])?;
    url
      .query_pairs_mut()
      .append_pair("external_reference", external_reference)
      .append_pair("sort", "date_created")
      .append_pair("criteria", "asc")
      .append_pair("range", "date_created")
      .append_pair("begin_date", "NOW-30DAYS")
      .append_pair("end_date", "NOW");
    let bytes = self.send(self.request(Method::GET, url)).await?;
    let response: SearchResponse = parse_json(&bytes)?;
    if response.results.len() > MAX_SEARCH_RESULTS
      || response.results.iter().any(|payment| payment.id <= 0)
    {
      return Err(Error::Unavailable);
    }
    Ok(response.results)
  }

  pub async fn payment(&self, payment_id: i64) -> Result<Payment, Error> {
    if payment_id <= 0 {
      return Err(Error::Invalid);
    }
    let bytes = self
      .send(self.request(
        Method::GET,
        self.url(&["v1", "payments", &payment_id.to_string()])?,
      ))
      .await?;
    let response: PaymentResponse = parse_json(&bytes)?;
    if response.id != payment_id {
      return Err(Error::Unavailable);
    }
    Ok(Payment {
      id: response.id,
      status: response.status,
      currency_id: response.currency_id,
      amount_centavos: response.transaction_amount.as_ref().and_then(centavos),
      external_reference: response.external_reference,
      collector_id: response.collector_id,
    })
  }

  /// Full refund. The idempotency key is fixed per payment, so a retried request cannot refund twice.
  pub async fn refund(&self, payment_id: i64, amount_centavos: i64) -> Result<Refund, Error> {
    let Some(amount) = pesos(amount_centavos) else {
      return Err(Error::Invalid);
    };
    if payment_id <= 0 || amount_centavos <= 0 {
      return Err(Error::Invalid);
    }
    let payment = payment_id.to_string();
    let bytes = self
      .send(
        self
          .request(
            Method::POST,
            self.url(&["v1", "payments", &payment, "refunds"])?,
          )
          .header("X-Idempotency-Key", idempotency_key(&payment, 0, "refund"))
          .json(&json!({"amount": amount})),
      )
      .await?;
    let response: RefundResponse = parse_json(&bytes)?;
    if response.id <= 0 {
      return Err(Error::Unavailable);
    }
    Ok(Refund {
      id: response.id,
      status: response.status,
    })
  }

  fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
    self
      .http
      .request(method, url)
      .header(AUTHORIZATION, self.authorization.clone())
      .header(ACCEPT, "application/json")
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
    let status = response.status();
    if !status.is_success() {
      return Err(
        if status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
          Error::Rejected
        } else {
          Error::Unavailable
        },
      );
    }
    if response
      .content_length()
      .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
      return Err(Error::Unavailable);
    }
    let mut bytes = Vec::new();
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

/// Centavos as a JSON peso amount. The shortest round-trip form of the nearest double is the exact
/// two-decimal value for any realistic price (below 10^13 centavos), checked in tests.
pub(super) fn pesos(amount_centavos: i64) -> Option<Value> {
  if !(0..10_000_000_000_000).contains(&amount_centavos) {
    return None;
  }
  Number::from_f64(amount_centavos as f64 / 100.0).map(Value::Number)
}

/// Exact peso amount to centavos; rejects negatives, exponents and more than two decimals.
pub(super) fn centavos(amount: &Number) -> Option<i64> {
  let text = amount.to_string();
  let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
  if whole.is_empty()
    || whole.len() > 13
    || fraction.len() > 2
    || !whole.bytes().all(|byte| byte.is_ascii_digit())
    || !fraction.bytes().all(|byte| byte.is_ascii_digit())
  {
    return None;
  }
  let fraction = format!("{fraction:0<2}");
  whole
    .parse::<i64>()
    .ok()?
    .checked_mul(100)?
    .checked_add(fraction.parse::<i64>().ok()?)
}

fn iso_time(millis: i64) -> Option<String> {
  chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
    .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, false))
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
  !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn valid_preference_id(value: &str) -> bool {
  !value.is_empty()
    && value.len() <= 255
    && value
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn checkout_url(value: &str) -> Option<String> {
  if value.len() > 2048 || value.chars().any(char::is_control) {
    return None;
  }
  let url = Url::parse(value).ok()?;
  let host = url.host_str()?;
  if url.scheme() != "https"
    || !(host == "mercadopago.com.mx" || host.ends_with(".mercadopago.com.mx"))
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
  hash.update(b"medical-assistant:mercado-pago:v1\0");
  hash.update((identity.len() as u64).to_be_bytes());
  hash.update(identity.as_bytes());
  hash.update(revision.to_be_bytes());
  hash.update(purpose.as_bytes());
  let digest = hash.finalize();
  let mut key = String::with_capacity(28 + digest.len() * 2);
  key.push_str("medical-assistant-mp-v1-");
  const HEX: &[u8; 16] = b"0123456789abcdef";
  for byte in digest {
    key.push(HEX[(byte >> 4) as usize] as char);
    key.push(HEX[(byte & 0x0f) as usize] as char);
  }
  key
}
