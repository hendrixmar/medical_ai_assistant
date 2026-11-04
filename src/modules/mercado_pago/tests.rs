use super::{
  client::{centavos, idempotency_key, pesos, Client, PreferenceRequest},
  config::MercadoPagoConfig,
  Error,
};
use axum::{
  body::{to_bytes, Body},
  extract::State,
  http::{HeaderValue, Method, Request, StatusCode},
  response::{IntoResponse, Response},
  Json, Router,
};
use serde_json::{json, Number, Value};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

const INVALID: &str = "Invalid Mercado Pago configuration: require a valid access token and canonical HTTPS PUBLIC_ORIGIN";

#[test]
fn access_token_is_optional_and_malformed_values_are_rejected_without_echoing_them() {
  assert!(MercadoPagoConfig::parse(None, Some("not an origin"))
    .unwrap()
    .is_none());
  let too_long = format!("APP_USR-{}", "x".repeat(248));
  for token in [
    "",
    "APP_USR-",
    "TEST-",
    "sk_test_wrong_provider",
    "APP_USR-has whitespace",
    "APP_USR-has\nnewline",
    "APP_USR-é",
    too_long.as_str(),
  ] {
    let error = MercadoPagoConfig::parse(Some(token), Some("https://practice.example"))
      .err()
      .unwrap();
    assert_eq!(error, INVALID);
    if !token.is_empty() {
      assert!(!error.contains(token));
    }
  }
  for origin in [
    None,
    Some("http://practice.example"),
    Some("https://practice.example/"),
  ] {
    assert!(MercadoPagoConfig::parse(Some("APP_USR-fixture"), origin).is_err());
  }
  for token in ["APP_USR-123-abc", "TEST-123-abc"] {
    let config = MercadoPagoConfig::parse(Some(token), Some("https://practice.example"))
      .unwrap()
      .unwrap();
    assert!(config.authorization.is_sensitive());
  }
}

#[test]
fn idempotency_keys_are_stable_and_distinct_per_purpose() {
  let key = idempotency_key("appointment-123", 9, "preference");
  assert_eq!(key, idempotency_key("appointment-123", 9, "preference"));
  assert_ne!(key, idempotency_key("appointment-123", 9, "refund"));
  assert_ne!(key, idempotency_key("appointment-123", 10, "preference"));
  assert!(key.starts_with("medical-assistant-mp-v1-"));
}

#[test]
fn peso_amounts_round_trip_exactly_through_json() {
  // Every price up to $10,000.00 and a sample of larger ones survive serialization exactly.
  for value in (0..=1_000_000).chain([12_345_678, 99_999_999, 123_456_789_012, 9_999_999_999_999]) {
    let Some(Value::Number(number)) = pesos(value) else {
      panic!("{value} must serialize");
    };
    let reparsed: Number = serde_json::from_str(&number.to_string()).unwrap();
    assert_eq!(centavos(&reparsed), Some(value), "{value}");
  }
  assert_eq!(pesos(125_500).unwrap().to_string(), "1255.0");
  assert_eq!(pesos(50_050).unwrap().to_string(), "500.5");
  assert!(pesos(-1).is_none());
  assert!(pesos(10_000_000_000_000).is_none());
  for (text, expected) in [
    ("500", Some(50_000)),
    ("500.0", Some(50_000)),
    ("500.05", Some(50_005)),
    ("500.001", None),
    ("-1", None),
    ("1e3", Some(100_000)),
    ("1e21", None),
  ] {
    let number: Number = serde_json::from_str(text).unwrap();
    assert_eq!(centavos(&number), expected, "{text}");
  }
}

#[tokio::test]
async fn requests_use_exact_checkout_pro_payment_and_refund_parameters() {
  let (client, captured, server) = provider(json!(
    "https://www.mercadopago.com.mx/checkout/v1/redirect?pref_id=123-pref"
  ))
  .await;
  assert_eq!(client.user_id().await.unwrap(), 4242);
  let appointment_id = uuid::Uuid::from_u128(0x12345678123456781234567812345678);
  let created_at = 1_793_800_000_000;
  let preference = client
    .create_preference(PreferenceRequest {
      appointment_id,
      revision: 9,
      external_reference: "ma1:12345678-1234-5678-1234-567812345678:9",
      amount_centavos: 125_550,
      local_start_time: "02/11/2026 a las 09:30 CST",
      created_at,
      expires_at: created_at + 7_200_000,
    })
    .await
    .unwrap();
  assert_eq!(preference.id, "123-pref");
  assert_eq!(
    preference.url,
    "https://www.mercadopago.com.mx/checkout/v1/redirect?pref_id=123-pref"
  );
  client
    .expire_preference(&preference.id, created_at + 60_000)
    .await
    .unwrap();
  let found = client
    .search_payments("ma1:12345678-1234-5678-1234-567812345678:9")
    .await
    .unwrap();
  assert_eq!(found.len(), 1);
  assert_eq!((found[0].id, found[0].status.as_str()), (77, "approved"));
  let payment = client.payment(77).await.unwrap();
  assert_eq!(payment.amount_centavos, Some(125_550));
  assert_eq!(payment.currency_id.as_deref(), Some("MXN"));
  assert_eq!(payment.collector_id, Some(4242));
  assert_eq!(
    payment.external_reference.as_deref(),
    Some("ma1:12345678-1234-5678-1234-567812345678:9")
  );
  let refund = client.refund(77, 125_550).await.unwrap();
  assert_eq!((refund.id, refund.status.as_str()), (901, "approved"));
  assert_eq!(client.refund(0, 1).await, Err(Error::Invalid));

  let requests = captured.requests.lock().await;
  assert_eq!(requests.len(), 6);
  for request in requests.iter() {
    assert_eq!(
      request.authorization.as_ref().unwrap().as_bytes(),
      b"Bearer APP_USR-synthetic-fixture"
    );
  }
  assert_eq!(
    (&requests[0].method, requests[0].uri.as_str()),
    (&Method::GET, "/users/me")
  );

  assert_eq!(requests[1].method, Method::POST);
  assert_eq!(requests[1].uri, "/checkout/preferences");
  assert_eq!(
    requests[1].idempotency_key.as_deref(),
    Some(idempotency_key("12345678-1234-5678-1234-567812345678", 9, "preference").as_str())
  );
  let body: Value = serde_json::from_str(&requests[1].body).unwrap();
  assert_eq!(
    body,
    json!({
      "items": [{
        "id": "consulta-video",
        "title": "Consulta por video - 02/11/2026 a las 09:30 CST",
        "quantity": 1,
        "currency_id": "MXN",
        "unit_price": 1255.5,
      }],
      "external_reference": "ma1:12345678-1234-5678-1234-567812345678:9",
      "expires": true,
      "expiration_date_from": "2026-11-04T13:46:40.000+00:00",
      "expiration_date_to": "2026-11-04T15:46:40.000+00:00",
      "payment_methods": {
        "excluded_payment_types": [{"id": "ticket"}, {"id": "atm"}, {"id": "bank_transfer"}],
        "installments": 1,
      },
      "binary_mode": true,
    })
  );
  assert!(requests[1].body.contains("\"unit_price\":1255.5"));

  assert_eq!(requests[2].method, Method::PUT);
  assert_eq!(requests[2].uri, "/checkout/preferences/123-pref");
  assert_eq!(
    serde_json::from_str::<Value>(&requests[2].body).unwrap(),
    json!({"expires": true, "expiration_date_to": "2026-11-04T13:47:40.000+00:00"})
  );

  let search = reqwest::Url::parse(&format!("http://fixture.invalid{}", requests[3].uri)).unwrap();
  assert_eq!(search.path(), "/v1/payments/search");
  let query: HashMap<_, _> = search.query_pairs().into_owned().collect();
  assert_eq!(
    query["external_reference"],
    "ma1:12345678-1234-5678-1234-567812345678:9"
  );
  assert_eq!(query["sort"], "date_created");
  assert_eq!(query["criteria"], "asc");

  assert_eq!(
    (&requests[4].method, requests[4].uri.as_str()),
    (&Method::GET, "/v1/payments/77")
  );

  assert_eq!(requests[5].method, Method::POST);
  assert_eq!(requests[5].uri, "/v1/payments/77/refunds");
  assert_eq!(
    requests[5].idempotency_key.as_deref(),
    Some(idempotency_key("77", 0, "refund").as_str())
  );
  assert_eq!(
    serde_json::from_str::<Value>(&requests[5].body).unwrap(),
    json!({"amount": 1255.5})
  );
  server.abort();
}

#[tokio::test]
async fn foreign_checkout_urls_and_error_statuses_are_not_trusted() {
  for url in [
    "https://evil.example/checkout?pref_id=1",
    "http://www.mercadopago.com.mx/checkout",
    "https://www.mercadopago.com.mx.evil.example/checkout",
    "https://www.mercadopago.com.mx/",
  ] {
    let (client, _, server) = provider(json!(url)).await;
    let result = client
      .create_preference(PreferenceRequest {
        appointment_id: uuid::Uuid::nil(),
        revision: 1,
        external_reference: "ma1:x:1",
        amount_centavos: 100,
        local_start_time: "hoy",
        created_at: 0,
        expires_at: 1,
      })
      .await;
    assert_eq!(result, Err(Error::Unavailable), "{url}");
    server.abort();
  }
  let (client, _, server) = provider(json!(null)).await;
  // The fixture answers 401 for this payment, 429 for its refund and 500 for its search.
  assert_eq!(client.payment(401).await, Err(Error::Rejected));
  assert_eq!(
    client.refund(429, 100).await.map(|_| ()),
    Err(Error::Unavailable)
  );
  assert_eq!(
    client.search_payments("error").await.map(|_| ()),
    Err(Error::Unavailable)
  );
  server.abort();
}

async fn provider(init_point: Value) -> (Client, Capture, tokio::task::JoinHandle<()>) {
  let captured = Capture {
    requests: Arc::default(),
    init_point,
  };
  let router = Router::new()
    .fallback(capture_request)
    .with_state(captured.clone());
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let base = format!("http://{}/", listener.local_addr().unwrap());
  let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
  let mut config = MercadoPagoConfig::parse(
    Some("APP_USR-synthetic-fixture"),
    Some("https://practice.example"),
  )
  .unwrap()
  .unwrap();
  config.test_endpoint = Some(base.parse().unwrap());
  (Client::new(&config).unwrap(), captured, server)
}

#[derive(Clone)]
struct Capture {
  requests: Arc<Mutex<Vec<CapturedRequest>>>,
  init_point: Value,
}

struct CapturedRequest {
  method: Method,
  uri: String,
  authorization: Option<HeaderValue>,
  idempotency_key: Option<String>,
  body: String,
}

async fn capture_request(State(capture): State<Capture>, request: Request<Body>) -> Response {
  let (parts, body) = request.into_parts();
  let bytes = to_bytes(body, 64 * 1024).await.unwrap();
  capture.requests.lock().await.push(CapturedRequest {
    method: parts.method.clone(),
    uri: parts.uri.to_string(),
    authorization: parts.headers.get("authorization").cloned(),
    idempotency_key: parts
      .headers
      .get("X-Idempotency-Key")
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned),
    body: String::from_utf8(bytes.to_vec()).unwrap(),
  });
  let query = parts.uri.query().unwrap_or_default();
  let response = match (parts.method, parts.uri.path()) {
    (Method::GET, "/users/me") => json!({"id": 4242, "nickname": "fixture"}),
    (Method::POST, "/checkout/preferences") => {
      json!({"id": "123-pref", "init_point": capture.init_point})
    }
    (Method::PUT, "/checkout/preferences/123-pref") => json!({"id": "123-pref"}),
    (Method::GET, "/v1/payments/search") if query.contains("external_reference=error") => {
      return StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
    (Method::GET, "/v1/payments/search") => json!({
      "paging": {"total": 1},
      "results": [{"id": 77, "status": "approved", "transaction_amount": 1255.5}]
    }),
    (Method::GET, "/v1/payments/401") => return StatusCode::UNAUTHORIZED.into_response(),
    (Method::GET, "/v1/payments/77") => json!({
      "id": 77,
      "status": "approved",
      "status_detail": "accredited",
      "currency_id": "MXN",
      "transaction_amount": 1255.5,
      "external_reference": "ma1:12345678-1234-5678-1234-567812345678:9",
      "collector_id": 4242
    }),
    (Method::POST, "/v1/payments/429/refunds") => {
      return StatusCode::TOO_MANY_REQUESTS.into_response()
    }
    (Method::POST, "/v1/payments/77/refunds") => json!({"id": 901, "status": "approved"}),
    _ => return (StatusCode::NOT_FOUND, "unexpected request").into_response(),
  };
  Json(response).into_response()
}
