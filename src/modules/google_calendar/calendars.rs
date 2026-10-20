use super::{
  client::{classify, Client, Failure},
  credentials::{self, Access},
  store::{self, sql},
  Error,
};
use crate::{app::AppState, modules::doctor::DoctorSession};
use reqwest::Method;
use sea_orm::ConnectionTrait;
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Calendar {
  pub id: String,
  #[serde(default)]
  pub summary: String,
  #[serde(default)]
  pub access_role: String,
  #[serde(default)]
  pub deleted: bool,
}
impl Calendar {
  pub fn candidate(&self) -> bool {
    !self.deleted && self.access_role == "owner"
  }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
  #[serde(default)]
  items: Vec<Calendar>,
  next_page_token: Option<String>,
}
impl Client {
  pub(super) async fn list(&self, access: &str) -> Result<Vec<Calendar>, Failure> {
    let mut all = Vec::new();
    let mut seen = HashSet::new();
    let mut ids = HashSet::new();
    let mut token: Option<String> = None;
    let mut size = 0;
    for _ in 0..10 {
      let mut url = self.url(&["users", "me", "calendarList"])?;
      url.query_pairs_mut().extend_pairs([
        ("showHidden", "true"),
        ("showDeleted", "false"),
        ("maxResults", "250"),
      ]);
      if let Some(token) = &token {
        url.query_pairs_mut().append_pair("pageToken", token);
      }
      let reply = self
        .bounded(self.request(Method::GET, url, access), 512 * 1024)
        .await?;
      if !reply.status.is_success() {
        return Err(classify(&reply));
      }
      size += reply.bytes.len();
      if size > 2 * 1024 * 1024 {
        return Err(Failure::Unavailable);
      }
      let page: Page = serde_json::from_slice(&reply.bytes).map_err(|_| Failure::Unavailable)?;
      if page.items.len() > 250 || all.len() + page.items.len() > 2500 {
        return Err(Failure::Unavailable);
      }
      for item in page.items {
        if !valid_id(&item.id) || item.summary.len() > 4096 || !ids.insert(item.id.clone()) {
          return Err(Failure::Unavailable);
        }
        if !item.deleted {
          all.push(item);
        }
      }
      match page.next_page_token {
        None => return Ok(all),
        Some(next) if !next.is_empty() && next.len() <= 4096 && seen.insert(next.clone()) => {
          token = Some(next)
        }
        _ => return Err(Failure::Unavailable),
      }
    }
    Err(Failure::Unavailable)
  }
  pub(super) async fn candidate(&self, access: &str, id: &str) -> Result<(), Failure> {
    let reply = self
      .bounded(
        self.request(
          Method::GET,
          self.url(&["users", "me", "calendarList", id])?,
          access,
        ),
        512 * 1024,
      )
      .await?;
    if !reply.status.is_success() {
      return Err(classify(&reply));
    }
    let item: Calendar = serde_json::from_slice(&reply.bytes).map_err(|_| Failure::Unavailable)?;
    if item.id != id || !item.candidate() {
      return Err(Failure::Rejected);
    }
    Ok(())
  }
  pub(super) async fn freebusy(
    &self,
    access: &str,
    ids: &[String],
    now: i64,
  ) -> Result<(), Failure> {
    let start = chrono::DateTime::from_timestamp_millis(now).ok_or(Failure::Unavailable)?;
    let end = start + chrono::Duration::hours(24);
    for chunk in ids.chunks(50) {
      let payload = serde_json::json!({"timeMin":start.to_rfc3339(),"timeMax":end.to_rfc3339(),"calendarExpansionMax":50,"items":chunk.iter().map(|id|serde_json::json!({"id":id})).collect::<Vec<_>>()});
      let reply = self
        .bounded(
          self
            .request(Method::POST, self.url(&["freeBusy"])?, access)
            .json(&payload),
          256 * 1024,
        )
        .await?;
      if !reply.status.is_success() {
        return Err(classify(&reply));
      }
      let body: serde_json::Value =
        serde_json::from_slice(&reply.bytes).map_err(|_| Failure::Unavailable)?;
      let calendars = body
        .get("calendars")
        .and_then(|v| v.as_object())
        .ok_or(Failure::Unavailable)?;
      for id in chunk {
        let result = calendars.get(id).ok_or(Failure::Unavailable)?;
        if result
          .get("errors")
          .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
        {
          return Err(Failure::Unavailable);
        }
        let busy = result
          .get("busy")
          .and_then(|v| v.as_array())
          .ok_or(Failure::Unavailable)?;
        for interval in busy {
          let parse = |key: &str| {
            interval
              .get(key)
              .and_then(|v| v.as_str())
              .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
              .ok_or(Failure::Unavailable)
          };
          let a = parse("start")?;
          let b = parse("end")?;
          if a >= b || a < start || b > end {
            return Err(Failure::Unavailable);
          }
        }
      }
    }
    Ok(())
  }
  pub(super) async fn freebusy_intervals(
    &self,
    access: &str,
    ids: &[String],
    start: i64,
    end: i64,
  ) -> Result<Vec<(i64, i64)>, Failure> {
    const DAY: i64 = 24 * 60 * 60 * 1000;
    const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
    const MAX_BUSY_RANGES: usize = 10_000;
    if start >= end || end.saturating_sub(start) > 366 * DAY || ids.len() > 100 {
      return Err(Failure::Unavailable);
    }
    let mut seen = HashSet::new();
    if ids
      .iter()
      .any(|id| !valid_id(id) || !seen.insert(id.as_str()))
    {
      return Err(Failure::Unavailable);
    }
    if ids.is_empty() {
      return Ok(Vec::new());
    }
    let mut response_bytes = 0usize;
    let mut busy = Vec::new();
    let mut window_start = start;
    while window_start < end {
      let window_end = window_start.saturating_add(DAY).min(end);
      let time_min = chrono::DateTime::from_timestamp_millis(window_start)
        .ok_or(Failure::Unavailable)?
        .to_rfc3339();
      let time_max = chrono::DateTime::from_timestamp_millis(window_end)
        .ok_or(Failure::Unavailable)?
        .to_rfc3339();
      for chunk in ids.chunks(50) {
        let payload = serde_json::json!({
          "timeMin": time_min,
          "timeMax": time_max,
          "calendarExpansionMax": 50,
          "items": chunk.iter().map(|id| serde_json::json!({"id":id})).collect::<Vec<_>>()
        });
        let reply = self
          .bounded(
            self
              .request(Method::POST, self.url(&["freeBusy"])?, access)
              .json(&payload),
            256 * 1024,
          )
          .await?;
        response_bytes = response_bytes.saturating_add(reply.bytes.len());
        if response_bytes > MAX_RESPONSE_BYTES {
          return Err(Failure::Unavailable);
        }
        if !reply.status.is_success() {
          return Err(classify(&reply));
        }
        let body: serde_json::Value =
          serde_json::from_slice(&reply.bytes).map_err(|_| Failure::Unavailable)?;
        let calendars = body
          .get("calendars")
          .and_then(serde_json::Value::as_object)
          .ok_or(Failure::Unavailable)?;
        for id in chunk {
          let calendar = calendars.get(id).ok_or(Failure::Unavailable)?;
          if calendar
            .get("errors")
            .is_some_and(|errors| errors.as_array().is_none_or(|items| !items.is_empty()))
          {
            return Err(Failure::Unavailable);
          }
          let intervals = calendar
            .get("busy")
            .and_then(serde_json::Value::as_array)
            .ok_or(Failure::Unavailable)?;
          for interval in intervals {
            let parse = |key: &str| {
              interval
                .get(key)
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
                .map(|at| at.timestamp_millis())
                .ok_or(Failure::Unavailable)
            };
            let busy_start = parse("start")?;
            let busy_end = parse("end")?;
            if busy_start >= busy_end {
              return Err(Failure::Unavailable);
            }
            let clipped_start = busy_start.max(start);
            let clipped_end = busy_end.min(end);
            if clipped_start < clipped_end {
              if busy.len() >= MAX_BUSY_RANGES {
                return Err(Failure::Unavailable);
              }
              busy.push((clipped_start, clipped_end));
            }
          }
        }
      }
      window_start = window_end;
    }
    Ok(busy)
  }

  pub(super) async fn booking_busy(
    &self,
    access: &str,
    calendar: &str,
    start: i64,
    end: i64,
    own_events: &HashSet<String>,
  ) -> Result<Vec<(i64, i64)>, Failure> {
    const DAY: i64 = 24 * 60 * 60 * 1000;
    if !valid_id(calendar) || start >= end || end.saturating_sub(start) > 366 * DAY {
      return Err(Failure::Unavailable);
    }
    let time_min = chrono::DateTime::from_timestamp_millis(start)
      .ok_or(Failure::Unavailable)?
      .to_rfc3339();
    let time_max = chrono::DateTime::from_timestamp_millis(end)
      .ok_or(Failure::Unavailable)?
      .to_rfc3339();
    let query_start = start;
    let query_end = end;
    let mut token = None;
    let mut page_tokens = HashSet::new();
    let mut event_ids = HashSet::new();
    let mut busy = Vec::new();
    let mut bytes_read = 0usize;
    let mut count = 0usize;
    for _ in 0..10 {
      let reply = self
        .list_events(access, calendar, &time_min, &time_max, token.as_deref())
        .await?;
      if !reply.status.is_success() {
        return Err(classify(&reply));
      }
      bytes_read = bytes_read.saturating_add(reply.bytes.len());
      if bytes_read > 2 * 1024 * 1024 {
        return Err(Failure::Unavailable);
      }
      let page: serde_json::Value =
        serde_json::from_slice(&reply.bytes).map_err(|_| Failure::Unavailable)?;
      if !page.is_object() {
        return Err(Failure::Unavailable);
      }
      let items = page
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or(Failure::Unavailable)?;
      if items.len() > 250 || count.saturating_add(items.len()) > 2500 {
        return Err(Failure::Unavailable);
      }
      count += items.len();
      for event in items {
        let id = event
          .get("id")
          .and_then(serde_json::Value::as_str)
          .filter(|id| !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control))
          .ok_or(Failure::Unavailable)?;
        if !event_ids.insert(id.to_owned()) {
          return Err(Failure::Unavailable);
        }
        if own_events.contains(id) {
          continue;
        }
        let status = match event.get("status") {
          None => "confirmed",
          Some(serde_json::Value::String(status)) => status.as_str(),
          _ => return Err(Failure::Unavailable),
        };
        if !matches!(status, "confirmed" | "tentative" | "cancelled") {
          return Err(Failure::Unavailable);
        }
        let transparency = match event.get("transparency") {
          None => "opaque",
          Some(serde_json::Value::String(transparency)) => transparency.as_str(),
          _ => return Err(Failure::Unavailable),
        };
        if !matches!(transparency, "opaque" | "transparent") {
          return Err(Failure::Unavailable);
        }
        if status == "cancelled" || transparency == "transparent" {
          continue;
        }
        let (start, start_all_day) = parse_event_time(event.get("start"))?;
        let (end, end_all_day) = parse_event_time(event.get("end"))?;
        let start = if start_all_day {
          start.saturating_sub(DAY)
        } else {
          start
        };
        let end = if end_all_day {
          end.saturating_add(DAY)
        } else {
          end
        };
        if start >= end {
          return Err(Failure::Unavailable);
        }
        let clipped_start = start.max(query_start);
        let clipped_end = end.min(query_end);
        if clipped_start < clipped_end {
          busy.push((clipped_start, clipped_end));
        }
      }
      match page.get("nextPageToken") {
        None | Some(serde_json::Value::Null) => {
          busy.sort_unstable();
          return Ok(busy);
        }
        Some(serde_json::Value::String(next))
          if !next.is_empty() && next.len() <= 4096 && page_tokens.insert(next.clone()) =>
        {
          token = Some(next.clone());
        }
        _ => return Err(Failure::Unavailable),
      }
    }
    Err(Failure::Unavailable)
  }
}

fn parse_event_time(value: Option<&serde_json::Value>) -> Result<(i64, bool), Failure> {
  let value = value.ok_or(Failure::Unavailable)?;
  let date_time = value.get("dateTime").and_then(serde_json::Value::as_str);
  let date = value.get("date").and_then(serde_json::Value::as_str);
  match (date_time, date) {
    (Some(raw), None) => chrono::DateTime::parse_from_rfc3339(raw)
      .map(|at| (at.timestamp_millis(), false))
      .map_err(|_| Failure::Unavailable),
    (None, Some(raw)) => chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
      .map(|date| {
        (
          date
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid")
            .and_utc()
            .timestamp_millis(),
          true,
        )
      })
      .map_err(|_| Failure::Unavailable),
    _ => Err(Failure::Unavailable),
  }
}
pub(super) fn valid_id(id: &str) -> bool {
  !id.is_empty()
    && id.len() <= 1024
    && !id.chars().any(char::is_control)
    && !matches!(id, "primary" | "." | "..")
}
pub(super) async fn list(state: &AppState) -> Result<(Access, Vec<Calendar>), Error> {
  let config = state.cfg.google.as_ref().ok_or(Error::Unavailable)?;
  let client = state.google.as_ref().ok_or(Error::Unavailable)?;
  let mut access =
    credentials::access(&state.db, state.clock.as_ref(), config, client, None, None).await?;
  let mut result = client.list(&access.token).await;
  if result.as_ref().is_err_and(|e| *e == Failure::Unauthorized) {
    access = credentials::access(
      &state.db,
      state.clock.as_ref(),
      config,
      client,
      Some(access.version),
      None,
    )
    .await?;
    result = client.list(&access.token).await;
    if result.as_ref().is_err_and(|e| *e == Failure::Unauthorized) {
      credentials::unauthorized(&state.db, &access).await?;
    }
  }
  Ok((access, result?))
}
pub(super) async fn save(
  state: &AppState,
  session: &DoctorSession,
  generation: i64,
  version: i64,
  booking: &str,
  blocking: Vec<String>,
) -> Result<(), Error> {
  if !valid_id(booking)
    || blocking.len() > 100
    || blocking.iter().any(|id| !valid_id(id))
    || blocking.iter().collect::<HashSet<_>>().len() != blocking.len()
  {
    return Err(Error::Invalid);
  }
  // Cheap stale check before remote work; commit rechecks under the lock.
  let row = store::load(&state.db).await?;
  if row.try_get::<i64>("", "generation")? != generation
    || row.try_get::<i64>("", "selection_version")? != version
  {
    return Err(Error::Invalid);
  }
  let (mut access, calendars) = list(state).await?;
  if access.generation != generation {
    return Err(Error::Invalid);
  }
  let chosen = calendars
    .iter()
    .find(|c| c.id == booking && c.candidate())
    .ok_or(Error::Invalid)?;
  if blocking
    .iter()
    .any(|id| !calendars.iter().any(|c| c.id == *id))
  {
    return Err(Error::Invalid);
  }
  let client = state.google.as_ref().ok_or(Error::Unavailable)?;
  let mut result = client
    .freebusy(&access.token, &blocking, state.clock.now_millis())
    .await;
  if result.as_ref().is_err_and(|e| *e == Failure::Unauthorized) {
    access = credentials::access(
      &state.db,
      state.clock.as_ref(),
      state.cfg.google.as_ref().ok_or(Error::Unavailable)?,
      client,
      Some(access.version),
      None,
    )
    .await?;
    if access.generation != generation {
      return Err(Error::Invalid);
    }
    result = client
      .freebusy(&access.token, &blocking, state.clock.now_millis())
      .await;
    if result.as_ref().is_err_and(|e| *e == Failure::Unauthorized) {
      credentials::unauthorized(&state.db, &access).await?;
    }
  }
  result?;
  let tx = store::transaction(&state.db).await?;
  store::live(&tx, &session.hash, state.clock.now_millis()).await?;
  let row = store::lock(&tx).await?;
  if row.try_get::<i64>("", "generation")? != generation
    || row.try_get::<i64>("", "selection_version")? != version
    || row.try_get::<String>("", "status")? != "connected"
  {
    return Err(Error::Invalid);
  }
  tx.execute(sql("UPDATE google_connection SET selection_version=selection_version+1,booking_calendar=$1,booking_label=$2,blocking_calendars=$3,current_probe_id=NULL,permission_checked_at=NULL,capability_checked_at=NULL,notice='selection_saved' WHERE id=1",vec![booking.into(),chosen.summary.clone().into(),serde_json::json!(blocking).into()])).await?;
  tx.commit().await?;
  Ok(())
}
