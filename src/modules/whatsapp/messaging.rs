use super::client::Client;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_RESPONSE: usize = 256 * 1024;
const MAX_ID: usize = 512;
const MAX_TEXT: usize = 1024 * 1024;
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);
const SEND_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreflightError {
  Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
  Rejected,
  Uncertain,
}

// Deliberately no Debug: message identifiers contain Contact identities.
#[derive(Clone, PartialEq, Eq)]
pub struct MessageId {
  pub serialized: String,
  pub peer: String,
  pub raw: String,
  pub from_me: bool,
}

pub fn parse_message_id(value: &str) -> Option<MessageId> {
  if value.len() > MAX_ID {
    return None;
  }
  let mut parts = value.splitn(3, '_');
  let from_me = match parts.next()? {
    "true" => true,
    "false" => false,
    _ => return None,
  };
  let peer = parts.next()?;
  let raw = parts.next()?;
  if !direct_chat(peer) || !valid_raw_id(raw) {
    return None;
  }
  Some(MessageId {
    serialized: value.to_owned(),
    peer: peer.to_owned(),
    raw: raw.to_owned(),
    from_me,
  })
}

fn direct_chat(chat: &str) -> bool {
  let (digits, max) = if let Some(digits) = chat.strip_suffix("@c.us") {
    (digits, 20)
  } else if let Some(digits) = chat.strip_suffix("@lid") {
    (digits, 32)
  } else {
    return false;
  };
  !digits.is_empty() && digits.len() <= max && digits.bytes().all(|b| b.is_ascii_digit())
}

fn valid_raw_id(id: &str) -> bool {
  // WAHA treats a fourth underscore-separated part as a participant, not raw ID.
  !id.is_empty() && id.len() <= MAX_ID && !id.contains('_') && !id.chars().any(char::is_control)
}

#[derive(Deserialize)]
struct IdResponse {
  id: String,
}

#[derive(Deserialize)]
struct TypingResponse {
  result: bool,
}

async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, ()> {
  if response
    .content_length()
    .is_some_and(|size| size > MAX_RESPONSE as u64)
  {
    return Err(());
  }
  let mut bytes = Vec::new();
  while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
    if chunk.len() > MAX_RESPONSE - bytes.len() {
      return Err(());
    }
    bytes.extend_from_slice(&chunk);
  }
  Ok(bytes)
}

impl Client {
  async fn messaging_preflight(
    &self,
    method: Method,
    path: &str,
    body: Option<Value>,
  ) -> Result<Vec<u8>, PreflightError> {
    let url = self
      .base
      .join(path)
      .map_err(|_| PreflightError::Unavailable)?;
    let mut request = self.http.request(method, url).timeout(PREFLIGHT_TIMEOUT);
    if let Some(body) = body {
      request = request.json(&body);
    }
    let response = request
      .send()
      .await
      .map_err(|_| PreflightError::Unavailable)?;
    if !response.status().is_success() {
      return Err(PreflightError::Unavailable);
    }
    bounded_body(response)
      .await
      .map_err(|_| PreflightError::Unavailable)
  }

  pub async fn new_message_id(&self) -> Result<String, PreflightError> {
    let bytes = self
      .messaging_preflight(Method::GET, "/api/default/new-message-id", None)
      .await?;
    let response: IdResponse =
      serde_json::from_slice(&bytes).map_err(|_| PreflightError::Unavailable)?;
    if !valid_raw_id(&response.id) {
      return Err(PreflightError::Unavailable);
    }
    Ok(response.id)
  }

  pub async fn seen(&self, chat: &str, incoming_id: &str) -> Result<(), PreflightError> {
    let id = parse_message_id(incoming_id).ok_or(PreflightError::Unavailable)?;
    if id.from_me || id.peer != chat {
      return Err(PreflightError::Unavailable);
    }
    self
      .messaging_preflight(
        Method::POST,
        "/api/sendSeen",
        Some(json!({
          "session": "default", "chatId": chat, "messageIds": [incoming_id]
        })),
      )
      .await?;
    Ok(())
  }

  async fn typing(&self, chat: &str, path: &str) -> Result<(), PreflightError> {
    if !direct_chat(chat) {
      return Err(PreflightError::Unavailable);
    }
    let bytes = self
      .messaging_preflight(
        Method::POST,
        path,
        Some(json!({
          "session": "default", "chatId": chat
        })),
      )
      .await?;
    let response: TypingResponse =
      serde_json::from_slice(&bytes).map_err(|_| PreflightError::Unavailable)?;
    if !response.result {
      return Err(PreflightError::Unavailable);
    }
    Ok(())
  }

  pub async fn start_typing(&self, chat: &str) -> Result<(), PreflightError> {
    self.typing(chat, "/api/startTyping").await
  }

  pub async fn stop_typing(&self, chat: &str) -> Result<(), PreflightError> {
    self.typing(chat, "/api/stopTyping").await
  }

  pub async fn send_text(
    &self,
    chat: &str,
    text: &str,
    raw_id: &str,
  ) -> Result<MessageId, SendError> {
    if !direct_chat(chat)
      || !valid_raw_id(raw_id)
      || "true__".len() + chat.len() + raw_id.len() > MAX_ID
      || text.is_empty()
      || text.len() > MAX_TEXT
      || text.contains('\0')
    {
      return Err(SendError::Rejected);
    }
    let url = self
      .base
      .join("/api/sendText")
      .map_err(|_| SendError::Rejected)?;
    // One invocation only. A lost response never authorizes another POST.
    let response = self
      .http
      .post(url)
      .timeout(SEND_TIMEOUT)
      .json(&json!({
        "session": "default", "chatId": chat, "text": text,
        "id": raw_id, "linkPreview": false
      }))
      .send()
      .await
      .map_err(|_| SendError::Uncertain)?;
    let status = response.status();
    let bytes = bounded_body(response)
      .await
      .map_err(|_| SendError::Uncertain)?;
    if !status.is_success() {
      return Err(
        if status == StatusCode::UNPROCESSABLE_ENTITY && working_session_rejection(&bytes) {
          SendError::Rejected
        } else {
          SendError::Uncertain
        },
      );
    }
    let response: IdResponse = serde_json::from_slice(&bytes).map_err(|_| SendError::Uncertain)?;
    let id = parse_message_id(&response.id).ok_or(SendError::Uncertain)?;
    if !id.from_me || id.raw != raw_id {
      return Err(SendError::Uncertain);
    }
    // The database-backed caller must validate this peer against trusted aliases.
    Ok(id)
  }
}

fn working_session_rejection(bytes: &[u8]) -> bool {
  let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
    return false;
  };
  if value == json!({"error": "Session \"default\" does not exist", "session": "default"}) {
    return true;
  }
  let Some(status) = value.get("status").and_then(Value::as_str) else {
    return false;
  };
  matches!(
    status,
    "STOPPED"
      | "STARTING"
      | "SCAN_QR_CODE"
      | "FAILED"
      | "PASSKEY_REQUIRED"
      | "PASSKEY_CONFIRMATION_REQUIRED"
  ) && value
    == json!({
      "error": "Session status is not as expected. Try again later or restart the session",
      "session": "default", "status": status, "expected": ["WORKING"]
    })
}

#[cfg(test)]
#[path = "messaging_tests.rs"]
mod tests;
