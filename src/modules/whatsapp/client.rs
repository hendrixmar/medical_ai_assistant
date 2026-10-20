use super::{config::WahaConfig, inbox::phone_identity, views::ConnectionState};
use base64::Engine;
use reqwest::{header, redirect::Policy, Method};
use serde::Deserialize;
use std::time::Duration;

pub(crate) const MAX_MEDIA_BYTES: usize = 10 * 1024 * 1024;
const MAX_HISTORY_BYTES: usize = 12 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MediaDownloadError {
  Unavailable,
  TooLarge,
  UnsafeUrl,
}

#[derive(Clone, Copy)]
enum ReadError {
  Unavailable,
  TooLarge,
}

#[derive(Clone)]
pub struct Client {
  pub(super) http: reqwest::Client,
  pub(super) base: reqwest::Url,
}
#[derive(Debug)]
pub(crate) struct Unavailable;
pub(crate) struct Status {
  pub state: ConnectionState,
  pub phone: Option<String>,
}
#[derive(Deserialize)]
struct SessionResponse {
  name: String,
  status: String,
  me: Option<Me>,
  engine: Option<EngineInfo>,
}
#[derive(Deserialize)]
struct Me {
  id: String,
}
#[derive(Deserialize)]
struct EngineInfo {
  engine: Option<String>,
}
#[derive(Deserialize)]
struct QrResponse {
  mimetype: String,
  data: String,
}
#[derive(Deserialize)]
struct CodeResponse {
  code: String,
}

pub(crate) struct HistoryMessage {
  pub id: String,
  pub timestamp: i64,
  pub from: String,
  pub from_me: bool,
  pub to: Option<String>,
  pub body: Option<String>,
  pub has_media: bool,
  pub media: Option<serde_json::Value>,
  pub media_url: Option<String>,
  pub resolved_phone: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryMessageRow {
  id: String,
  timestamp: i64,
  from: String,
  from_me: bool,
  to: Option<String>,
  body: Option<String>,
  #[serde(default)]
  has_media: bool,
  media: Option<serde_json::Value>,
  media_url: Option<String>,
  #[serde(rename = "_data")]
  data: Option<HistoryData>,
}

#[derive(Deserialize)]
struct HistoryData {
  #[serde(rename = "Info")]
  info: Option<HistoryInfo>,
}

#[derive(Deserialize)]
struct HistoryInfo {
  #[serde(rename = "SenderAlt")]
  sender_alt: Option<String>,
}
impl Client {
  pub fn new(config: &WahaConfig) -> Result<Self, &'static str> {
    let mut headers = header::HeaderMap::new();
    headers.insert("x-api-key", config.api_key.clone());
    headers.insert(
      header::ACCEPT,
      header::HeaderValue::from_static("application/json"),
    );
    let http = reqwest::Client::builder()
      .default_headers(headers)
      .no_proxy()
      .redirect(Policy::none())
      .retry(reqwest::retry::never())
      .connect_timeout(Duration::from_secs(2))
      .timeout(Duration::from_secs(5))
      .build()
      .map_err(|_| "Unable to initialize WAHA HTTP client")?;
    Ok(Self {
      http,
      base: config.base.clone(),
    })
  }
  async fn request(
    &self,
    method: Method,
    path: &str,
    limit: usize,
    body: Option<&serde_json::Value>,
  ) -> Result<Vec<u8>, Unavailable> {
    let url = self.base.join(path).map_err(|_| Unavailable)?;
    self.request_url(method, url, limit, body).await
  }

  async fn request_url(
    &self,
    method: Method,
    url: reqwest::Url,
    limit: usize,
    body: Option<&serde_json::Value>,
  ) -> Result<Vec<u8>, Unavailable> {
    let mut request = self.http.request(method, url);
    if let Some(body) = body {
      request = request.json(body);
    }
    let response = request.send().await.map_err(|_| Unavailable)?;
    Self::read_limited(response, limit)
      .await
      .map_err(|_| Unavailable)
  }

  async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
  ) -> Result<Vec<u8>, ReadError> {
    if !response.status().is_success() {
      return Err(ReadError::Unavailable);
    }
    if response
      .content_length()
      .is_some_and(|size| size > limit as u64)
    {
      return Err(ReadError::TooLarge);
    }
    let capacity = response
      .content_length()
      .and_then(|size| usize::try_from(size).ok())
      .unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.map_err(|_| ReadError::Unavailable)? {
      if chunk.len() > limit - bytes.len() {
        return Err(ReadError::TooLarge);
      }
      bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
  }

  pub(crate) async fn download_media(&self, raw_url: &str) -> Result<Vec<u8>, MediaDownloadError> {
    let url = reqwest::Url::parse(raw_url).map_err(|_| MediaDownloadError::UnsafeUrl)?;
    if url.origin() != self.base.origin()
      || !url.username().is_empty()
      || url.password().is_some()
      || url.fragment().is_some()
    {
      return Err(MediaDownloadError::UnsafeUrl);
    }
    let response = self
      .http
      .get(url)
      .header(header::ACCEPT, "*/*")
      .send()
      .await
      .map_err(|_| MediaDownloadError::Unavailable)?;
    Self::read_limited(response, MAX_MEDIA_BYTES)
      .await
      .map_err(|error| match error {
        ReadError::Unavailable => MediaDownloadError::Unavailable,
        ReadError::TooLarge => MediaDownloadError::TooLarge,
      })
  }

  pub(crate) async fn messages(
    &self,
    chat_id: &str,
    timestamp_gte: i64,
    timestamp_lte: i64,
    limit: u16,
    offset: u32,
  ) -> Result<Vec<HistoryMessage>, Unavailable> {
    let Some((digits, suffix)) = chat_id.rsplit_once('@') else {
      return Err(Unavailable);
    };
    if !digits.bytes().all(|byte| byte.is_ascii_digit())
      || (suffix == "c.us" && !(1..=20).contains(&digits.len()))
      || (suffix == "lid" && !(1..=32).contains(&digits.len()))
      || !matches!(suffix, "c.us" | "lid")
      || timestamp_gte < 0
      || timestamp_lte < timestamp_gte
      || timestamp_lte > 253_402_300_799
      || !(1..=100).contains(&limit)
      || offset > 100_000
    {
      return Err(Unavailable);
    }
    let mut url = self.base.join("/api/messages").map_err(|_| Unavailable)?;
    {
      let mut query = url.query_pairs_mut();
      query
        .append_pair("session", "default")
        .append_pair("chatId", chat_id)
        .append_pair("limit", &limit.to_string())
        .append_pair("offset", &offset.to_string())
        .append_pair("sortBy", "timestamp")
        .append_pair("sortOrder", "asc")
        .append_pair("merge", "false")
        .append_pair("downloadMedia", "true")
        .append_pair("downloadMediaMimetypes", "image/,application/pdf")
        .append_pair("filter.fromMe", "false")
        .append_pair("filter.timestamp.gte", &timestamp_gte.to_string())
        .append_pair("filter.timestamp.lte", &timestamp_lte.to_string());
    }
    let bytes = self
      .request_url(Method::GET, url, MAX_HISTORY_BYTES, None)
      .await?;
    let rows: Vec<HistoryMessageRow> = serde_json::from_slice(&bytes).map_err(|_| Unavailable)?;
    if rows.len() > limit as usize
      || rows.iter().any(|message| {
        message.id.is_empty()
          || message.id.len() > 512
          || message.id.chars().any(char::is_control)
          || message.from != chat_id
          || message.from_me
          || message.timestamp < timestamp_gte
          || message.timestamp > timestamp_lte
          || message
            .body
            .as_ref()
            .is_some_and(|body| body.len() > 1024 * 1024 || body.contains('\0'))
          || message
            .media_url
            .as_ref()
            .is_some_and(|url| url.len() > 8192 || url.contains('\0'))
      })
    {
      return Err(Unavailable);
    }
    Ok(
      rows
        .into_iter()
        .map(|row| HistoryMessage {
          id: row.id,
          timestamp: row.timestamp,
          from: row.from,
          from_me: row.from_me,
          to: row.to,
          body: row.body,
          has_media: row.has_media,
          media: row.media,
          media_url: row.media_url,
          resolved_phone: row
            .data
            .and_then(|data| data.info)
            .and_then(|info| info.sender_alt)
            .and_then(|jid| phone_identity(&jid)),
        })
        .collect(),
    )
  }

  pub(crate) async fn status(&self) -> Result<Status, Unavailable> {
    let bytes = self
      .request(Method::GET, "/api/sessions/default", 256 * 1024, None)
      .await?;
    let session: SessionResponse = serde_json::from_slice(&bytes).map_err(|_| Unavailable)?;
    if session.name != "default"
      || session
        .engine
        .and_then(|e| e.engine)
        .is_some_and(|e| e != "GOWS")
    {
      return Err(Unavailable);
    }
    let state = match session.status.as_str() {
      "STOPPED" => ConnectionState::Stopped,
      "STARTING" => ConnectionState::Starting,
      "SCAN_QR_CODE" => ConnectionState::Qr,
      "PASSKEY_REQUIRED" => ConnectionState::PasskeyRequired,
      "PASSKEY_CONFIRMATION_REQUIRED" => ConnectionState::PasskeyConfirmationRequired,
      "WORKING" => ConnectionState::Working,
      "FAILED" => ConnectionState::Failed,
      _ => ConnectionState::Unknown,
    };
    let phone = session
      .me
      .and_then(|me| phone_identity(&me.id))
      .and_then(|jid| jid.strip_suffix("@c.us").map(|digits| format!("+{digits}")));
    Ok(Status { state, phone })
  }
  pub(super) async fn start(&self) -> Result<(), Unavailable> {
    self
      .request(
        Method::POST,
        "/api/sessions/default/start",
        256 * 1024,
        None,
      )
      .await?;
    Ok(())
  }
  pub(super) async fn request_code(&self, phone: &str) -> Result<String, Unavailable> {
    let body = serde_json::json!({"phoneNumber": phone});
    let bytes = self
      .request(
        Method::POST,
        "/api/default/auth/request-code",
        256 * 1024,
        Some(&body),
      )
      .await?;
    let response: CodeResponse = serde_json::from_slice(&bytes).map_err(|_| Unavailable)?;
    let code = response.code.trim();
    if !(4..=32).contains(&code.len())
      || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
      return Err(Unavailable);
    }
    Ok(code.to_owned())
  }
  pub(super) async fn qr(&self) -> Result<Vec<u8>, Unavailable> {
    let bytes = self
      .request(
        Method::GET,
        "/api/default/auth/qr?format=image",
        1536 * 1024,
        None,
      )
      .await?;
    let qr: QrResponse = serde_json::from_slice(&bytes).map_err(|_| Unavailable)?;
    if qr.mimetype != "image/png" {
      return Err(Unavailable);
    }
    let png = base64::engine::general_purpose::STANDARD
      .decode(qr.data)
      .map_err(|_| Unavailable)?;
    if png.len() > 1024 * 1024 || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
      return Err(Unavailable);
    }
    Ok(png)
  }
}
#[cfg(test)]
mod media_tests {
  use super::*;
  use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  };
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  fn test_client(origin: &str) -> Client {
    Client {
      http: reqwest::Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .build()
        .unwrap(),
      base: reqwest::Url::parse(origin).unwrap(),
    }
  }

  #[tokio::test]
  async fn media_download_rejects_other_origins_credentials_and_fragments() {
    let client = test_client("https://waha.example");
    for url in [
      "http://127.0.0.1:8080/media",
      "https://169.254.169.254/latest/meta-data/",
      "https://user:secret@waha.example/media",
      "https://waha.example/media#fragment",
    ] {
      assert_eq!(
        client.download_media(url).await,
        Err(MediaDownloadError::UnsafeUrl)
      );
    }
  }

  #[tokio::test]
  async fn media_download_rejects_redirects_and_oversized_responses() {
    let requests = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let seen = requests.clone();
    let server = tokio::spawn(async move {
      for _ in 0..2 {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut header = [0u8; 4096];
        let mut used = 0;
        loop {
          let n = stream.read(&mut header[used..]).await.unwrap();
          assert!(n > 0, "The media request must include complete headers");
          used += n;
          if header[..used].windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            break;
          }
          assert!(
            used < header.len(),
            "Synthetic request exceeds fixture bounds"
          );
        }
        seen.fetch_add(1, Ordering::Relaxed);
        let response: &[u8] = if header[..used].starts_with(b"GET /large ") {
          b"HTTP/1.1 200 OK\r\nContent-Length: 10485761\r\nConnection: close\r\n\r\n"
        } else {
          b"HTTP/1.1 302 Found\r\nLocation: /followed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        };
        stream.write_all(response).await.unwrap();
        stream.shutdown().await.unwrap();
      }
    });
    let client = test_client(&origin);
    assert_eq!(
      client.download_media(&format!("{origin}/redirect")).await,
      Err(MediaDownloadError::Unavailable)
    );
    assert_eq!(requests.load(Ordering::Relaxed), 1);
    assert_eq!(
      client.download_media(&format!("{origin}/large")).await,
      Err(MediaDownloadError::TooLarge)
    );
    assert_eq!(requests.load(Ordering::Relaxed), 2);
    server.await.unwrap();
  }
}
