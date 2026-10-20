use super::{config::GoogleConfig, crypto, Error};
use reqwest::{redirect::Policy, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const SCOPES: [&str; 3] = [
  "https://www.googleapis.com/auth/calendar.events.owned",
  "https://www.googleapis.com/auth/calendar.freebusy",
  "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
];
#[derive(Clone)]
pub struct Client {
  http: reqwest::Client,
  token: Url,
  pub(super) calendar: Url,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
  Unauthorized,
  InvalidGrant,
  Rejected,
  Conflict,
  NotFound,
  Deleted,
  Precondition,
  Transient(u32),
  Unavailable,
}
#[derive(Serialize, Deserialize)]
pub(super) struct Tokens {
  pub access: String,
  pub refresh: String,
  pub expires_at: i64,
}
#[derive(Deserialize)]
struct Grant {
  access_token: String,
  #[serde(default, deserialize_with = "present_refresh_token")]
  refresh_token: Option<String>,
  expires_in: i64,
  token_type: String,
  scope: Option<String>,
}
// Only an absent field may reuse a refresh token; a supplied null is malformed.
fn present_refresh_token<'de, D: serde::Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<String>, D::Error> {
  String::deserialize(deserializer).map(Some)
}
pub(super) struct Reply {
  pub status: StatusCode,
  pub bytes: Vec<u8>,
  pub retry_after: u32,
}
impl Client {
  pub fn new(_config: &GoogleConfig) -> Result<Self, &'static str> {
    let http = reqwest::Client::builder()
      .no_proxy()
      .redirect(Policy::none())
      .retry(reqwest::retry::never())
      .connect_timeout(Duration::from_secs(2))
      .timeout(Duration::from_secs(5))
      .build()
      .map_err(|_| "Unable to initialize Google HTTP client")?;
    let token =
      Url::parse("https://oauth2.googleapis.com/token").map_err(|_| "Invalid Google endpoint")?;
    let calendar = Url::parse("https://www.googleapis.com/calendar/v3/")
      .map_err(|_| "Invalid Google endpoint")?;
    #[cfg(test)]
    let (token, calendar) = _config.test_endpoints.clone().unwrap_or((token, calendar));
    Ok(Self {
      http,
      token,
      calendar,
    })
  }
  pub(super) fn authorization(&self, config: &GoogleConfig, state: &str, verifier: &str) -> String {
    let mut url =
      Url::parse("https://accounts.google.com/o/oauth2/v2/auth").expect("fixed authorization URL");
    url.query_pairs_mut().extend_pairs([
      ("client_id", config.client_id.as_str()),
      ("redirect_uri", config.redirect.as_str()),
      ("response_type", "code"),
      ("scope", &SCOPES.join(" ")),
      ("access_type", "offline"),
      ("prompt", "consent select_account"),
      ("state", state),
      ("code_challenge_method", "S256"),
      ("code_challenge", &crypto::challenge(verifier)),
    ]);
    url.into()
  }
  pub(super) async fn bounded(
    &self,
    request: reqwest::RequestBuilder,
    limit: usize,
  ) -> Result<Reply, Failure> {
    let mut response = request.send().await.map_err(|_| Failure::Unavailable)?;
    let status = response.status();
    let retry_after = response
      .headers()
      .get("retry-after")
      .and_then(|v| v.to_str().ok())
      .and_then(|s| {
        s.parse::<u32>().ok().or_else(|| {
          chrono::DateTime::parse_from_rfc2822(s)
            .ok()
            .map(|at| (at.timestamp() - chrono::Utc::now().timestamp()).clamp(0, 301) as u32)
        })
      })
      .unwrap_or(0)
      .min(301);
    let limit = if status.is_success() {
      limit
    } else {
      64 * 1024
    };
    if response.content_length().is_some_and(|n| n > limit as u64) {
      return Err(Failure::Unavailable);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Unavailable)? {
      if chunk.len() > limit - bytes.len() {
        return Err(Failure::Unavailable);
      }
      bytes.extend_from_slice(&chunk);
    }
    Ok(Reply {
      status,
      bytes,
      retry_after,
    })
  }
  pub(super) async fn exchange(
    &self,
    config: &GoogleConfig,
    code: &str,
    verifier: &str,
    now: i64,
  ) -> Result<Tokens, Failure> {
    let reply = self
      .bounded(
        self.http.post(self.token.clone()).form(&[
          ("client_id", config.client_id.as_str()),
          ("client_secret", config.client_secret.as_str()),
          ("grant_type", "authorization_code"),
          ("code", code),
          ("code_verifier", verifier),
          ("redirect_uri", config.redirect.as_str()),
        ]),
        64 * 1024,
      )
      .await?;
    grant(reply, None, now)
  }
  pub(super) async fn refresh(
    &self,
    config: &GoogleConfig,
    refresh: &str,
    now: i64,
  ) -> Result<Tokens, Failure> {
    let reply = self
      .bounded(
        self.http.post(self.token.clone()).form(&[
          ("client_id", config.client_id.as_str()),
          ("client_secret", config.client_secret.as_str()),
          ("grant_type", "refresh_token"),
          ("refresh_token", refresh),
        ]),
        64 * 1024,
      )
      .await?;
    grant(reply, Some(refresh), now)
  }
  pub(super) fn request(
    &self,
    method: reqwest::Method,
    url: Url,
    access: &str,
  ) -> reqwest::RequestBuilder {
    self
      .http
      .request(method, url)
      .bearer_auth(access)
      .header("accept", "application/json")
  }
  pub(super) fn url(&self, segments: &[&str]) -> Result<Url, Failure> {
    let mut url = self.calendar.clone();
    url
      .path_segments_mut()
      .map_err(|_| Failure::Unavailable)?
      .pop_if_empty()
      .extend(segments);
    Ok(url)
  }
  pub(super) async fn list_events(
    &self,
    access: &str,
    calendar: &str,
    start: &str,
    end: &str,
    page_token: Option<&str>,
  ) -> Result<Reply, Failure> {
    let mut url = self.url(&["calendars", calendar, "events"])?;
    url.query_pairs_mut().extend_pairs([
      ("timeMin", start),
      ("timeMax", end),
      ("singleEvents", "true"),
      ("orderBy", "startTime"),
      ("maxResults", "250"),
      (
        "fields",
        "nextPageToken,items(id,status,transparency,start(date,dateTime),end(date,dateTime))",
      ),
    ]);
    if let Some(token) = page_token {
      url.query_pairs_mut().append_pair("pageToken", token);
    }
    self
      .bounded(self.request(reqwest::Method::GET, url, access), 256 * 1024)
      .await
  }

  pub(super) async fn get_event(
    &self,
    access: &str,
    calendar: &str,
    event: &str,
  ) -> Result<Reply, Failure> {
    let mut url = self.url(&["calendars", calendar, "events", event])?;
    url
      .query_pairs_mut()
      .append_pair("fields", "id,etag,status,summary,description,start(date,dateTime),end(date,dateTime),conferenceData(createRequest(requestId,status(statusCode)),conferenceSolution(key(type)),entryPoints(entryPointType,uri))");
    self
      .bounded(self.request(reqwest::Method::GET, url, access), 256 * 1024)
      .await
  }

  pub(super) async fn insert_event(
    &self,
    access: &str,
    calendar: &str,
    body: &serde_json::Value,
    conference: bool,
  ) -> Result<Reply, Failure> {
    let mut url = self.url(&["calendars", calendar, "events"])?;
    url.query_pairs_mut().append_pair("sendUpdates", "none");
    if conference {
      url
        .query_pairs_mut()
        .append_pair("conferenceDataVersion", "1");
    }
    self
      .bounded(
        self.request(reqwest::Method::POST, url, access).json(body),
        256 * 1024,
      )
      .await
  }

  pub(super) async fn patch_event(
    &self,
    access: &str,
    calendar: &str,
    event: &str,
    body: &serde_json::Value,
    conference: bool,
    etag: &str,
  ) -> Result<Reply, Failure> {
    let mut url = self.url(&["calendars", calendar, "events", event])?;
    url.query_pairs_mut().append_pair("sendUpdates", "none");
    if conference {
      url
        .query_pairs_mut()
        .append_pair("conferenceDataVersion", "1");
    }
    self
      .bounded(
        self
          .request(reqwest::Method::PATCH, url, access)
          .header(reqwest::header::IF_MATCH, etag)
          .json(body),
        256 * 1024,
      )
      .await
  }

  pub(super) async fn delete_event(
    &self,
    access: &str,
    calendar: &str,
    event: &str,
    etag: &str,
  ) -> Result<Reply, Failure> {
    let mut url = self.url(&["calendars", calendar, "events", event])?;
    url.query_pairs_mut().append_pair("sendUpdates", "none");
    let mut request = self.request(reqwest::Method::DELETE, url, access);
    request = request.header(reqwest::header::IF_MATCH, etag);
    self.bounded(request, 256 * 1024).await
  }
}
fn grant(reply: Reply, previous: Option<&str>, now: i64) -> Result<Tokens, Failure> {
  if !reply.status.is_success() {
    return Err(classify(&reply));
  }
  let grant: Grant = serde_json::from_slice(&reply.bytes).map_err(|_| Failure::Unavailable)?;
  if !valid_token(&grant.access_token)
    || grant.token_type != "Bearer"
    || !(31..=604800).contains(&grant.expires_in)
  {
    return Err(Failure::Unavailable);
  }
  let scope = grant.scope.ok_or(Failure::Unavailable)?;
  let scopes: Vec<_> = scope.split(' ').collect();
  if scopes.len() != 3 || !SCOPES.iter().all(|s| scopes.contains(s)) {
    return Err(Failure::Unavailable);
  }
  let refresh = grant
    .refresh_token
    .or_else(|| previous.map(String::from))
    .filter(|s| valid_token(s))
    .ok_or(Failure::Unavailable)?;
  Ok(Tokens {
    access: grant.access_token,
    refresh,
    expires_at: now
      .checked_add(grant.expires_in * 1000)
      .ok_or(Failure::Unavailable)?,
  })
}
fn valid_token(s: &str) -> bool {
  !s.is_empty() && s.len() <= 8192 && !s.chars().any(|c| c.is_control() || c.is_whitespace())
}
pub(super) fn classify(reply: &Reply) -> Failure {
  let body: serde_json::Value =
    serde_json::from_slice(&reply.bytes).unwrap_or(serde_json::Value::Null);
  if body.get("error").and_then(|v| v.as_str()) == Some("invalid_grant") {
    return Failure::InvalidGrant;
  }
  match reply.status.as_u16() {
    401 => Failure::Unauthorized,
    409 => Failure::Conflict,
    404 => Failure::NotFound,
    412 => Failure::Precondition,
    410
      if body
        .pointer("/error/errors")
        .and_then(|v| v.as_array())
        .is_some_and(|v| v.iter().any(|e| e["reason"] == "deleted")) =>
    {
      Failure::Deleted
    }
    429 | 500..=599 => Failure::Transient(reply.retry_after),
    403
      if body
        .pointer("/error/errors")
        .and_then(|v| v.as_array())
        .is_some_and(|errors| {
          errors.iter().any(|e| {
            matches!(
              e["reason"].as_str(),
              Some("rateLimitExceeded" | "userRateLimitExceeded")
            )
          })
        }) =>
    {
      Failure::Transient(reply.retry_after)
    }
    400 | 403 => Failure::Rejected,
    _ => Failure::Unavailable,
  }
}
impl From<Failure> for Error {
  fn from(failure: Failure) -> Self {
    match failure {
      Failure::Unauthorized | Failure::InvalidGrant => Self::Reauthorize,
      _ => Self::Unavailable,
    }
  }
}
