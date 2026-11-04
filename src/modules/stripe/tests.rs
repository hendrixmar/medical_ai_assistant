use super::{
  client::{idempotency_key, Client, PaymentLinkRequest},
  config::StripeConfig,
};
use axum::{
  body::{to_bytes, Body},
  extract::State,
  http::{HeaderValue, Method, Request, StatusCode},
  response::{IntoResponse, Response},
  Json, Router,
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

#[test]
fn stripe_key_configuration_is_optional_and_rejects_malformed_values_without_echoing_them() {
  assert!(StripeConfig::parse(None, Some("not an origin"))
    .unwrap()
    .is_none());
  let too_long = format!("sk_{}", "x".repeat(253));
  for secret in [
    "",
    "sk_",
    "pk_live_not_secret",
    "sk_test_has whitespace",
    "sk_test_has\nnewline",
    "sk_test_é",
    too_long.as_str(),
  ] {
    let error = StripeConfig::parse(Some(secret), Some("https://practice.example"))
      .err()
      .unwrap();
    assert_eq!(
      error,
      "Invalid Stripe configuration: require a valid secret key and canonical HTTPS PUBLIC_ORIGIN"
    );
    if !secret.is_empty() {
      assert!(!error.contains(secret));
    }
  }
  for origin in [
    None,
    Some("http://practice.example"),
    Some("https://practice.example/"),
  ] {
    assert!(StripeConfig::parse(Some("rk_test_fixture"), origin).is_err());
  }
  let config = StripeConfig::parse(Some("sk_test_fixture"), Some("https://practice.example"))
    .unwrap()
    .unwrap();
  assert!(config.authorization.is_sensitive());
}

#[test]
fn appointment_purpose_idempotency_keys_are_stable_and_distinct() {
  let price = idempotency_key("appointment-123", 9, "price");
  assert_eq!(price, idempotency_key("appointment-123", 9, "price"));
  assert_ne!(price, idempotency_key("appointment-123", 9, "payment_link"));
  assert_ne!(price, idempotency_key("appointment-123", 10, "price"));
}

#[tokio::test]
async fn requests_use_exact_connect_and_card_payment_parameters() {
  let captured = Capture::default();
  let provider = Router::new()
    .fallback(capture_request)
    .with_state(captured.clone());
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let base = format!("http://{}/v1/", listener.local_addr().unwrap());
  let server = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });

  let mut config = StripeConfig::parse(Some("sk_test_fixture"), Some("https://practice.example"))
    .unwrap()
    .unwrap();
  config.test_endpoint = Some(base.parse().unwrap());
  let client = Client::new(&config).unwrap();

  let account = client.create_account().await.unwrap();
  assert_eq!(account.id, "acct_fixture");
  let onboarding = client.create_account_link(&account.id).await.unwrap();
  assert!(onboarding.starts_with("https://connect.stripe.com/setup/e/"));
  let account_status = client.retrieve_account(&account.id).await.unwrap();
  assert!(account_status.charges_enabled);
  assert!(account_status.details_submitted);
  let appointment_id = uuid::Uuid::from_u128(0x12345678123456781234567812345678);
  let payment_link = client
    .create_payment_link(PaymentLinkRequest {
      account_id: &account.id,
      appointment_id,
      revision: 9,
      amount_centavos: 12500,
      local_start_time: "2026-11-02 09:30",
      payment_reference: "AB12CD",
    })
    .await
    .unwrap();
  assert_eq!(payment_link.price_id, "price_fixture");
  assert_eq!(payment_link.link_id, "plink_fixture");
  assert_eq!(payment_link.url, "https://buy.stripe.com/test_fixture");
  let sessions = client
    .list_checkout_sessions(&account.id, &payment_link.link_id)
    .await
    .unwrap();
  assert_eq!(sessions.len(), 1);
  assert_eq!(sessions[0].id, "cs_fixture");
  assert_eq!(sessions[0].currency.as_deref(), Some("mxn"));
  assert_eq!(sessions[0].amount_total, Some(12500));
  client
    .deactivate_payment_link(&account.id, &payment_link.link_id)
    .await
    .unwrap();

  let requests = captured.0.lock().await;
  assert_eq!(requests.len(), 7);
  for request in requests.iter() {
    let authorization = request.authorization.as_ref().unwrap();
    assert_eq!(authorization.as_bytes(), b"Bearer sk_test_fixture");
  }

  assert_eq!(requests[0].method, Method::POST);
  assert_eq!(requests[0].uri, "/v1/accounts");
  let expected_account_key = idempotency_key("https://practice.example", 0, "connected_account");
  assert_eq!(
    requests[0].idempotency_key.as_deref(),
    Some(expected_account_key.as_str())
  );
  let account_form = form_values(&requests[0].body);
  assert_eq!(account_form.len(), 5);
  assert_eq!(account_form["country"], "MX");
  assert_eq!(account_form["controller[stripe_dashboard][type]"], "full");
  assert_eq!(account_form["controller[fees][payer]"], "account");
  assert_eq!(account_form["controller[losses][payments]"], "stripe");
  assert_eq!(account_form["controller[requirement_collection]"], "stripe");

  assert_eq!(requests[1].uri, "/v1/account_links");
  let account_link_form = form_values(&requests[1].body);
  assert_eq!(account_link_form.len(), 4);
  assert_eq!(account_link_form["account"], "acct_fixture");
  assert_eq!(account_link_form["type"], "account_onboarding");
  assert_eq!(
    account_link_form["refresh_url"],
    "https://practice.example/settings/stripe/refresh"
  );
  assert_eq!(
    account_link_form["return_url"],
    "https://practice.example/settings/stripe/return"
  );

  assert_eq!(requests[2].method, Method::GET);
  assert_eq!(requests[2].uri, "/v1/accounts/acct_fixture");
  assert!(requests[2].stripe_account.is_none());

  assert_eq!(requests[3].uri, "/v1/prices");
  assert_eq!(requests[3].stripe_account.as_deref(), Some("acct_fixture"));
  let price_form = form_values(&requests[3].body);
  assert_eq!(price_form.len(), 3);
  assert_eq!(price_form["currency"], "mxn");
  assert_eq!(price_form["unit_amount"], "12500");
  assert_eq!(
    price_form["product_data[name]"],
    "Consulta por video - 2026-11-02 09:30"
  );
  assert!(requests[3].idempotency_key.is_some());

  assert_eq!(requests[4].uri, "/v1/payment_links");
  assert_eq!(requests[4].stripe_account.as_deref(), Some("acct_fixture"));
  let link_form = form_values(&requests[4].body);
  assert_eq!(link_form.len(), 9);
  assert_eq!(link_form["line_items[0][price]"], "price_fixture");
  assert_eq!(link_form["line_items[0][quantity]"], "1");
  assert_eq!(link_form["payment_method_types[0]"], "card");
  assert_eq!(link_form["restrictions[completed_sessions][limit]"], "1");
  assert_eq!(link_form["after_completion[type]"], "hosted_confirmation");
  assert_eq!(
    link_form["after_completion[hosted_confirmation][custom_message]"],
    "Pago recibido. Recibirás la confirmación por WhatsApp."
  );
  assert_eq!(
    link_form["metadata[appointment_id]"],
    "12345678-1234-5678-1234-567812345678"
  );
  assert_eq!(link_form["metadata[revision]"], "9");
  assert_eq!(link_form["metadata[payment_reference]"], "AB12CD");
  assert_eq!(requests[4].stripe_account, requests[3].stripe_account);
  assert!(requests[4].idempotency_key.is_some());
  assert_ne!(requests[3].idempotency_key, requests[4].idempotency_key);

  let sessions_uri = UrlParts::parse(&requests[5].uri);
  assert_eq!(requests[5].method, Method::GET);
  assert_eq!(sessions_uri.path, "/v1/checkout/sessions");
  assert_eq!(sessions_uri.query["payment_link"], "plink_fixture");
  assert_eq!(sessions_uri.query["limit"], "3");
  assert_eq!(requests[5].stripe_account.as_deref(), Some("acct_fixture"));

  assert_eq!(requests[6].method, Method::POST);
  assert_eq!(requests[6].uri, "/v1/payment_links/plink_fixture");
  assert_eq!(requests[6].stripe_account.as_deref(), Some("acct_fixture"));
  assert_eq!(form_values(&requests[6].body)["active"], "false");
  server.abort();
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<CapturedRequest>>>);

struct CapturedRequest {
  method: Method,
  uri: String,
  authorization: Option<HeaderValue>,
  stripe_account: Option<String>,
  idempotency_key: Option<String>,
  body: String,
}

async fn capture_request(State(capture): State<Capture>, request: Request<Body>) -> Response {
  let (parts, body) = request.into_parts();
  let bytes = to_bytes(body, 64 * 1024).await.unwrap();
  let record = CapturedRequest {
    method: parts.method.clone(),
    uri: parts.uri.to_string(),
    authorization: parts.headers.get("authorization").cloned(),
    stripe_account: parts
      .headers
      .get("Stripe-Account")
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned),
    idempotency_key: parts
      .headers
      .get("Idempotency-Key")
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned),
    body: String::from_utf8(bytes.to_vec()).unwrap(),
  };
  let response: Value = match (parts.method, parts.uri.path()) {
    (Method::POST, "/v1/accounts") => json!({"id":"acct_fixture"}),
    (Method::POST, "/v1/account_links") => {
      json!({"url":"https://connect.stripe.com/setup/e/fixture"})
    }
    (Method::GET, "/v1/accounts/acct_fixture") => {
      json!({"charges_enabled":true,"details_submitted":true})
    }
    (Method::POST, "/v1/prices") => json!({"id":"price_fixture"}),
    (Method::POST, "/v1/payment_links") => {
      json!({"id":"plink_fixture","url":"https://buy.stripe.com/test_fixture"})
    }
    (Method::GET, "/v1/checkout/sessions") => json!({
      "data":[{"id":"cs_fixture","payment_status":"paid","status":"complete","currency":"mxn","amount_total":12500}]
    }),
    (Method::POST, "/v1/payment_links/plink_fixture") => json!({"active":false}),
    _ => {
      return (StatusCode::NOT_FOUND, "unexpected request").into_response();
    }
  };
  capture.0.lock().await.push(record);
  Json(response).into_response()
}

fn form_values(body: &str) -> HashMap<String, String> {
  reqwest::Url::parse(&format!("https://fixture.invalid/?{body}"))
    .unwrap()
    .query_pairs()
    .map(|(key, value)| (key.into_owned(), value.into_owned()))
    .collect()
}

struct UrlParts {
  path: String,
  query: HashMap<String, String>,
}

impl UrlParts {
  fn parse(value: &str) -> Self {
    let url = reqwest::Url::parse(&format!("http://fixture.invalid{value}")).unwrap();
    Self {
      path: url.path().to_owned(),
      query: url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect(),
    }
  }
}
