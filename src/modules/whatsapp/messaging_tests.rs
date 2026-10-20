use super::*;
use crate::modules::whatsapp::config::WahaConfig;
use axum::{
  body::{to_bytes, Body},
  extract::{Request, State},
  response::Response,
  Router,
};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const API_KEY: &str = "synthetic-messaging-api-key";
const HMAC_KEY: &str = "synthetic-messaging-hmac-key-32-characters";
const CHAT: &str = "15550100200@c.us";
const LID: &str = "998877665544@lid";
const RAW_ID: &str = "opaque:ID-with-dashes";

#[derive(Clone)]
struct Captured {
  method: String,
  path: String,
  body: Value,
}

struct Fixture {
  status: u16,
  body: Vec<u8>,
  streamed: bool,
  delay: Duration,
  requests: Vec<Captured>,
}

struct Provider {
  state: Arc<Mutex<Fixture>>,
  client: Client,
  task: tokio::task::JoinHandle<()>,
}

impl Drop for Provider {
  fn drop(&mut self) {
    self.task.abort();
  }
}

fn client(origin: &str) -> Client {
  let config = WahaConfig::parse(Some(origin), Some(API_KEY), Some(HMAC_KEY))
    .unwrap()
    .unwrap();
  Client::new(&config).unwrap()
}

impl Provider {
  async fn new(status: u16, body: Vec<u8>) -> Self {
    let state = Arc::new(Mutex::new(Fixture {
      status,
      body,
      streamed: false,
      delay: Duration::ZERO,
      requests: Vec::new(),
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&format!("http://{}", listener.local_addr().unwrap()));
    let app = Router::new().fallback(reply).with_state(state.clone());
    let task = tokio::spawn(async move {
      axum::serve(listener, app).await.unwrap();
    });
    Self {
      state,
      client,
      task,
    }
  }

  fn respond(&self, status: u16, body: Value) {
    let mut state = self.state.lock().unwrap();
    state.status = status;
    state.body = body.to_string().into_bytes();
  }

  fn requests(&self) -> Vec<Captured> {
    self.state.lock().unwrap().requests.clone()
  }
}

async fn reply(State(state): State<Arc<Mutex<Fixture>>>, request: Request) -> Response {
  assert_eq!(request.headers()["x-api-key"], API_KEY);
  assert_eq!(request.headers()["accept"], "application/json");
  let method = request.method().to_string();
  let path = request.uri().to_string();
  if method == "POST" {
    assert_eq!(request.headers()["content-type"], "application/json");
  }
  let bytes = to_bytes(request.into_body(), 2 * MAX_TEXT).await.unwrap();
  let body = if bytes.is_empty() {
    Value::Null
  } else {
    serde_json::from_slice(&bytes).unwrap()
  };
  let (status, response, streamed, delay) = {
    let mut state = state.lock().unwrap();
    state.requests.push(Captured { method, path, body });
    (
      state.status,
      state.body.clone(),
      state.streamed,
      state.delay,
    )
  };
  tokio::time::sleep(delay).await;
  Response::builder()
    .status(status)
    .header("content-type", "application/json")
    .header("location", "/must-not-follow")
    .body(if streamed {
      Body::new(Chunks {
        bytes: response,
        offset: 0,
      })
    } else {
      Body::from(response)
    })
    .unwrap()
}

// Unknown size forces the incremental limit, rather than Content-Length rejection.
struct Chunks {
  bytes: Vec<u8>,
  offset: usize,
}
impl hyper::body::Body for Chunks {
  type Data = axum::body::Bytes;
  type Error = std::convert::Infallible;

  fn poll_frame(
    mut self: std::pin::Pin<&mut Self>,
    _: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
    if self.offset == self.bytes.len() {
      return std::task::Poll::Ready(None);
    }
    let end = (self.offset + 16384).min(self.bytes.len());
    let chunk = axum::body::Bytes::copy_from_slice(&self.bytes[self.offset..end]);
    self.offset = end;
    std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(chunk))))
  }
}

fn success(peer: &str, raw: &str) -> Value {
  json!({"id": format!("true_{peer}_{raw}"), "_data": null})
}

#[test]
fn serialized_direct_ids_preserve_opaque_raw_and_original_peer() {
  for peer in [CHAT, LID] {
    for (direction, from_me) in [("false", false), ("true", true)] {
      let serialized = format!("{direction}_{peer}_{RAW_ID}");
      let id = parse_message_id(&serialized).unwrap();
      assert_eq!(id.serialized, serialized);
      assert_eq!(id.peer, peer);
      assert_eq!(id.raw, RAW_ID);
      assert_eq!(id.from_me, from_me);
    }
  }
  for value in [
    "",
    "raw-only",
    "TRUE_123@c.us_id",
    "false_@c.us_id",
    "false_123@c.us_",
    "false_abc@lid_id",
    "false_１２３@lid_id",
    "false_123@g.us_id",
    "false_123@s.whatsapp.net_id",
    "false_status@broadcast_id",
    "false_123@newsletter_id",
    "false_123:4@lid_id",
    "false_123@c.us_a\0b",
    "false_123@c.us_a\nb",
    "false_123@c.us_a\u{0085}b",
    "false_123@c.us_opaque:ID_with_underscores",
    "false_123@c.us_id_participant",
    "false_123@c.us_id_456@c.us",
    "true_123@lid_id_456@lid",
    "false_123@c.us_id_",
  ] {
    assert!(parse_message_id(value).is_none());
  }
  for (suffix, max) in [("@c.us", 20), ("@lid", 32)] {
    assert!(parse_message_id(&format!("false_{}{suffix}_id", "1".repeat(max))).is_some());
    assert!(parse_message_id(&format!("false_{}{suffix}_id", "1".repeat(max + 1))).is_none());
  }
  let prefix = format!("false_{CHAT}_");
  assert!(parse_message_id(&format!("{prefix}{}", "x".repeat(MAX_ID - prefix.len()))).is_some());
  assert!(parse_message_id(&format!(
    "{prefix}{}",
    "x".repeat(MAX_ID - prefix.len() + 1)
  ))
  .is_none());
}

#[tokio::test]
async fn exact_methods_headers_and_dtos_include_original_seen_id_and_direct_lid() {
  let provider = Provider::new(200, json!({"id":RAW_ID}).to_string().into_bytes()).await;
  assert_eq!(provider.client.new_message_id().await.unwrap(), RAW_ID);
  let incoming = format!("false_{LID}_{RAW_ID}");
  {
    let mut state = provider.state.lock().unwrap();
    state.status = 201;
    state.body.clear();
  }
  provider.client.seen(LID, &incoming).await.unwrap();
  provider.respond(201, json!({"result":true}));
  provider.client.start_typing(LID).await.unwrap();
  provider.client.stop_typing(LID).await.unwrap();
  provider.respond(201, success(LID, RAW_ID));
  let sent = provider
    .client
    .clone()
    .send_text(LID, "Texto sintético\nsegunda línea", RAW_ID)
    .await
    .unwrap();
  assert_eq!(sent.serialized, format!("true_{LID}_{RAW_ID}"));
  let requests = provider.requests();
  assert_eq!(requests.len(), 5);
  for (request, method, path, body) in [
    (
      &requests[0],
      "GET",
      "/api/default/new-message-id",
      Value::Null,
    ),
    (
      &requests[1],
      "POST",
      "/api/sendSeen",
      json!({"session":"default","chatId":LID,"messageIds":[incoming]}),
    ),
    (
      &requests[2],
      "POST",
      "/api/startTyping",
      json!({"session":"default","chatId":LID}),
    ),
    (
      &requests[3],
      "POST",
      "/api/stopTyping",
      json!({"session":"default","chatId":LID}),
    ),
    (
      &requests[4],
      "POST",
      "/api/sendText",
      json!({"session":"default","chatId":LID,"text":"Texto sintético\nsegunda línea","id":RAW_ID,"linkPreview":false}),
    ),
  ] {
    assert_eq!(request.method, method);
    assert_eq!(request.path, path);
    assert_eq!(request.body, body);
  }
}

#[tokio::test]
async fn explicit_seen_accepts_empty_success_bodies() {
  let provider = Provider::new(200, Vec::new()).await;
  for status in [200, 201, 204] {
    provider.state.lock().unwrap().status = status;
    provider
      .client
      .seen(CHAT, &format!("false_{CHAT}_{RAW_ID}"))
      .await
      .unwrap();
  }
  assert_eq!(provider.requests().len(), 3);
}

#[tokio::test]
async fn invalid_preflight_input_never_reaches_provider() {
  let provider = Provider::new(201, Vec::new()).await;
  for incoming in [
    RAW_ID.to_owned(),
    format!("true_{CHAT}_{RAW_ID}"),
    format!("false_{LID}_{RAW_ID}"),
    format!("false_{CHAT}_opaque:ID_with_underscores"),
    format!("false_{CHAT}_{RAW_ID}_456@c.us"),
    format!("false_{CHAT}_{RAW_ID}_"),
  ] {
    assert_eq!(
      provider.client.seen(CHAT, &incoming).await,
      Err(PreflightError::Unavailable)
    );
  }
  for chat in ["123@g.us", "123@s.whatsapp.net", "not-a-chat"] {
    assert_eq!(
      provider.client.start_typing(chat).await,
      Err(PreflightError::Unavailable)
    );
    assert_eq!(
      provider.client.stop_typing(chat).await,
      Err(PreflightError::Unavailable)
    );
  }
  assert!(provider.requests().is_empty());
}

#[tokio::test]
async fn generator_and_typing_reject_malformed_or_unsuccessful_responses() {
  let provider = Provider::new(200, Vec::new()).await;
  for body in [
    json!({}),
    json!({"id":null}),
    json!({"id":42}),
    json!({"id":""}),
    json!({"id":"a\nb"}),
    json!({"id":"opaque:ID_with_underscores"}),
    json!({"id":"id_participant"}),
    json!({"id":"id_"}),
    json!({"id":"x".repeat(MAX_ID + 1)}),
  ] {
    provider.respond(200, body);
    let before = provider.requests().len();
    assert_eq!(
      provider.client.new_message_id().await,
      Err(PreflightError::Unavailable)
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), before + 1);
    assert_eq!(requests[before].method, "GET");
    assert_eq!(requests[before].path, "/api/default/new-message-id");
  }
  provider.state.lock().unwrap().body = b"not json".to_vec();
  assert_eq!(
    provider.client.new_message_id().await,
    Err(PreflightError::Unavailable)
  );
  for body in [json!({}), json!({"result":false}), json!({"result":"true"})] {
    provider.respond(201, body);
    assert_eq!(
      provider.client.start_typing(CHAT).await,
      Err(PreflightError::Unavailable)
    );
  }
  for status in [302, 401, 422, 500] {
    provider.respond(status, json!({"id":RAW_ID,"result":true}));
    let before = provider.requests().len();
    assert_eq!(
      provider.client.new_message_id().await,
      Err(PreflightError::Unavailable)
    );
    assert_eq!(
      provider
        .client
        .seen(CHAT, &format!("false_{CHAT}_{RAW_ID}"))
        .await,
      Err(PreflightError::Unavailable)
    );
    assert_eq!(
      provider.client.stop_typing(CHAT).await,
      Err(PreflightError::Unavailable)
    );
    assert_eq!(provider.requests().len(), before + 3);
  }
}

#[tokio::test]
async fn send_validates_direction_and_raw_id_but_leaves_alias_trust_to_caller() {
  let provider = Provider::new(201, success(LID, RAW_ID).to_string().into_bytes()).await;
  let result = provider
    .client
    .send_text(CHAT, "Sintético", RAW_ID)
    .await
    .unwrap();
  assert_eq!(result.peer, LID);
  assert_eq!(result.raw, RAW_ID);
  for body in [
    json!({"id":format!("false_{CHAT}_{RAW_ID}")}),
    success(CHAT, "different"),
    success(CHAT, &format!("{RAW_ID}_456@c.us")),
    success(CHAT, "opaque:ID_with_underscores"),
    success("123@g.us", RAW_ID),
    json!({"id":RAW_ID}),
    json!({"id":null}),
    json!({}),
    json!({"id":format!("true_{CHAT}_{}", "x".repeat(MAX_ID))}),
  ] {
    provider.respond(201, body);
    let before = provider.requests().len();
    assert!(matches!(
      provider.client.send_text(CHAT, "Sintético", RAW_ID).await,
      Err(SendError::Uncertain)
    ));
    assert_eq!(provider.requests().len(), before + 1);
  }
  provider.state.lock().unwrap().body = b"{\"id\":".to_vec();
  assert!(matches!(
    provider.client.send_text(CHAT, "Sintético", RAW_ID).await,
    Err(SendError::Uncertain)
  ));
}

fn session_rejection(status: &str) -> Value {
  json!({"error":"Session status is not as expected. Try again later or restart the session",
    "session":"default", "status":status, "expected":["WORKING"]})
}

#[tokio::test]
async fn only_exact_known_422_session_shapes_are_conclusive_rejections() {
  let provider = Provider::new(422, Vec::new()).await;
  for body in [
    json!({"error":"Session \"default\" does not exist","session":"default"}),
    session_rejection("STOPPED"),
    session_rejection("STARTING"),
    session_rejection("SCAN_QR_CODE"),
    session_rejection("FAILED"),
    session_rejection("PASSKEY_REQUIRED"),
    session_rejection("PASSKEY_CONFIRMATION_REQUIRED"),
  ] {
    provider.respond(422, body);
    let before = provider.requests().len();
    assert!(matches!(
      provider.client.send_text(CHAT, "Sintético", RAW_ID).await,
      Err(SendError::Rejected)
    ));
    assert_eq!(provider.requests().len(), before + 1);
  }
  let mut wrong_session = session_rejection("STOPPED");
  wrong_session["session"] = json!("other");
  let mut wrong_expected = session_rejection("STOPPED");
  wrong_expected["expected"] = json!(["STOPPED"]);
  let mut extra_field = session_rejection("STOPPED");
  extra_field["unverified"] = json!(true);
  for body in [
    session_rejection("WORKING"),
    session_rejection("UNKNOWN"),
    wrong_session,
    wrong_expected,
    extra_field,
    json!({"error":"Session name is required","session":""}),
    json!({"error":"Session \"default\" does not exist"}),
    json!({"exception":{"message":"server returned error 475"}}),
  ] {
    provider.respond(422, body);
    assert!(matches!(
      provider.client.send_text(CHAT, "Sintético", RAW_ID).await,
      Err(SendError::Uncertain)
    ));
  }
}

#[tokio::test]
async fn unknown_errors_and_redirects_remain_uncertain_without_second_post() {
  let provider = Provider::new(500, Vec::new()).await;
  for status in [
    301, 302, 303, 307, 308, 400, 401, 403, 404, 429, 463, 475, 500, 502, 503, 504,
  ] {
    for body in [
      session_rejection("STOPPED"),
      json!({"exception":{"message":"server returned error 463; server returned error 475; websocket not connected"}}),
    ] {
      provider.respond(status, body);
      let before = provider.requests().len();
      assert!(matches!(
        provider.client.send_text(CHAT, "Sintético", RAW_ID).await,
        Err(SendError::Uncertain)
      ));
      assert_eq!(provider.requests().len(), before + 1);
    }
  }
  assert!(provider
    .requests()
    .iter()
    .all(|r| r.path == "/api/sendText" && r.method == "POST"));
}

#[tokio::test]
async fn all_responses_enforce_header_and_incremental_limits_including_error_bodies() {
  let oversized = json!({
    "id": format!("true_{CHAT}_{RAW_ID}"), "result": true,
    "_data": "x".repeat(MAX_RESPONSE)
  })
  .to_string()
  .into_bytes();
  let provider = Provider::new(201, oversized.clone()).await;
  for streamed in [false, true] {
    {
      let mut state = provider.state.lock().unwrap();
      state.streamed = streamed;
      state.body = oversized.clone();
    }
    provider.respond(201, json!({"id":RAW_ID,"_data":"x".repeat(MAX_RESPONSE)}));
    assert_eq!(
      provider.client.new_message_id().await,
      Err(PreflightError::Unavailable)
    );
    provider.state.lock().unwrap().body = oversized.clone();
    assert_eq!(
      provider
        .client
        .seen(CHAT, &format!("false_{CHAT}_{RAW_ID}"))
        .await,
      Err(PreflightError::Unavailable)
    );
    assert_eq!(
      provider.client.start_typing(CHAT).await,
      Err(PreflightError::Unavailable)
    );
    assert_eq!(
      provider.client.stop_typing(CHAT).await,
      Err(PreflightError::Unavailable)
    );
    for status in [201, 422, 500] {
      {
        let mut state = provider.state.lock().unwrap();
        state.status = status;
        state.body = if status == 422 {
          let mut bytes = session_rejection("STOPPED").to_string().into_bytes();
          bytes.resize(MAX_RESPONSE + 1, b' ');
          bytes
        } else {
          oversized.clone()
        };
      }
      let before = provider.requests().len();
      assert!(matches!(
        provider.client.send_text(CHAT, "Sintético", RAW_ID).await,
        Err(SendError::Uncertain)
      ));
      assert_eq!(provider.requests().len(), before + 1);
    }
    provider.state.lock().unwrap().status = 201;
  }
  // The exact bound is accepted; only the next byte exceeds the contract.
  for streamed in [false, true] {
    let mut bytes = success(CHAT, RAW_ID).to_string().into_bytes();
    bytes.resize(MAX_RESPONSE, b' ');
    {
      let mut state = provider.state.lock().unwrap();
      state.body = bytes;
      state.streamed = streamed;
    }
    assert!(provider
      .client
      .send_text(CHAT, "Sintético", RAW_ID)
      .await
      .is_ok());
  }
}

#[tokio::test]
async fn invalid_send_arguments_are_bounded_and_make_no_request() {
  let provider = Provider::new(201, Vec::new()).await;
  for (chat, text, raw) in [
    ("123@g.us", "text", RAW_ID),
    (CHAT, "", RAW_ID),
    (CHAT, "nul\0text", RAW_ID),
    (CHAT, "text", ""),
    (CHAT, "text", "id\n"),
    (CHAT, "text", "opaque:ID_with_underscores"),
    (CHAT, "text", "id_participant"),
    (CHAT, "text", "id_"),
    (CHAT, "text", &"x".repeat(MAX_ID)),
    (CHAT, "text", &"x".repeat(MAX_ID + 1)),
    (CHAT, &"x".repeat(MAX_TEXT + 1), RAW_ID),
  ] {
    assert!(matches!(
      provider.client.send_text(chat, text, raw).await,
      Err(SendError::Rejected)
    ));
  }
  assert!(provider.requests().is_empty());
}

#[tokio::test]
async fn outgoing_serialized_limit_is_checked_before_http_for_each_target() {
  let provider = Provider::new(201, Vec::new()).await;
  for chat in [CHAT, LID, "12345678901234567890123456789012@lid"] {
    let prefix = format!("true_{chat}_");
    let raw = "x".repeat(MAX_ID - prefix.len());
    let before = provider.requests().len();
    assert!(matches!(
      provider
        .client
        .send_text(chat, "Sintético", &format!("{raw}x"))
        .await,
      Err(SendError::Rejected)
    ));
    assert_eq!(provider.requests().len(), before);
    provider.respond(201, success(chat, &raw));
    let sent = provider
      .client
      .send_text(chat, "Sintético", &raw)
      .await
      .unwrap();
    assert_eq!(sent.serialized.len(), MAX_ID);
    assert_eq!(sent.raw, raw);
    assert_eq!(provider.requests().len(), before + 1);
  }
}

#[tokio::test]
async fn lost_and_truncated_http_responses_are_uncertain_without_replay() {
  for truncated in [false, true] {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&format!("http://{}", listener.local_addr().unwrap()));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recorded = calls.clone();
    let task = tokio::spawn(async move {
      loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        // Read the complete request before losing the response: submission is ambiguous.
        loop {
          let mut chunk = [0u8; 4096];
          let n = socket.read(&mut chunk).await.unwrap();
          if n == 0 {
            break;
          }
          received.extend_from_slice(&chunk[..n]);
          if let Some(end) = received.windows(4).position(|v| v == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&received[..end]);
            let length: usize = headers
              .lines()
              .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name
                  .eq_ignore_ascii_case("content-length")
                  .then(|| value.trim().parse::<usize>().unwrap())
              })
              .unwrap();
            if received.len() >= end + 4 + length {
              break;
            }
          }
        }
        recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert!(received.starts_with(b"POST /api/sendText HTTP/1.1\r\n"));
        if truncated {
          socket
            .write_all(
              b"HTTP/1.1 201 Created\r\nContent-Length: 200\r\nConnection: close\r\n\r\n{\"id\":",
            )
            .await
            .unwrap();
        }
        socket.shutdown().await.unwrap();
      }
    });
    let outcome = tokio::time::timeout(
      Duration::from_secs(10),
      client.send_text(CHAT, "Sintético", RAW_ID),
    )
    .await;
    task.abort();
    assert!(matches!(outcome.unwrap(), Err(SendError::Uncertain)));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
  }
}

#[tokio::test]
async fn reset_with_unread_request_body_is_uncertain_without_replay() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let client = client(&format!("http://{}", listener.local_addr().unwrap()));
  let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
  let recorded = calls.clone();
  let task = tokio::spawn(async move {
    loop {
      let (mut socket, _) = listener.accept().await.unwrap();
      let mut headers = Vec::new();
      while !headers.ends_with(b"\r\n\r\n") {
        headers.push(socket.read_u8().await.unwrap());
        assert!(headers.len() < 8192);
      }
      assert!(headers.starts_with(b"POST /api/sendText HTTP/1.1\r\n"));
      recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      // Closing with unread inbound body resets the connection on this Linux host.
      drop(socket);
    }
  });
  let outcome = tokio::time::timeout(
    Duration::from_secs(10),
    client.send_text(CHAT, "Sintético", RAW_ID),
  )
  .await;
  task.abort();
  assert!(matches!(outcome.unwrap(), Err(SendError::Uncertain)));
  assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore = "Exercises the actual 120-second send deadline against a stalled loopback provider"]
async fn send_deadline_leaves_one_uncertain_post() {
  let provider = Provider::new(201, success(CHAT, RAW_ID).to_string().into_bytes()).await;
  provider.state.lock().unwrap().delay = Duration::from_secs(125);
  let outcome = tokio::time::timeout(
    Duration::from_secs(130),
    provider.client.send_text(CHAT, "Sintético", RAW_ID),
  )
  .await
  .unwrap();
  assert!(matches!(outcome, Err(SendError::Uncertain)));
  assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
async fn preflight_deadline_remains_five_seconds_but_send_overrides_it() {
  let provider = Provider::new(201, success(CHAT, RAW_ID).to_string().into_bytes()).await;
  let generator = Provider::new(200, json!({"id":RAW_ID}).to_string().into_bytes()).await;
  provider.state.lock().unwrap().delay = Duration::from_secs(6);
  generator.state.lock().unwrap().delay = Duration::from_secs(6);
  let (preflight, sent) = tokio::join!(
    generator.client.new_message_id(),
    provider.client.send_text(CHAT, "Sintético", RAW_ID),
  );
  assert_eq!(preflight, Err(PreflightError::Unavailable));
  assert!(sent.is_ok());
  assert_eq!(provider.requests().len(), 1);
  assert_eq!(generator.requests().len(), 1);
}
