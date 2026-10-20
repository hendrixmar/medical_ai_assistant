use super::{
  client::{classify, Client, Failure},
  credentials,
  probes::{self, Evidence, Probe},
  Error,
};
use crate::{common::config::Config, database::Db, modules::doctor::Clock};
use reqwest::Method;
use serde_json::{json, Value};

/// One durable phase per tick. Constructors never spawn tasks; main owns the loop.
pub async fn run_once(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
) -> Result<bool, Error> {
  let (Some(_), Some(client)) = (&cfg.google, client) else {
    return Ok(false);
  };
  let Some(probe) = probes::claim(db, clock).await? else {
    return Ok(false);
  };
  if probe.phase == "cleanup"
    && (probe.row.try_get::<i32>("", "cleanup_calls")? >= 12
      || probe
        .row
        .try_get::<Option<i64>>("", "cleanup_deadline")?
        .is_none_or(|t| clock.now_millis() + 8_000 >= t))
  {
    record(
      db,
      clock,
      &probe,
      "attention",
      "cleanup_exhausted",
      "pending",
      "unresolved",
      false,
      false,
      0,
    )
    .await?;
    return Ok(true);
  }
  if matches!(probe.phase.as_str(), "create_committed" | "observing")
    && (probe.row.try_get::<i32>("", "observations")? >= 12
      || probe
        .row
        .try_get::<Option<i64>>("", "observation_deadline")?
        .is_none_or(|t| clock.now_millis() + 8_000 >= t))
  {
    record(
      db,
      clock,
      &probe,
      "cleanup",
      "observation_exhausted",
      "failed",
      "pending",
      false,
      false,
      0,
    )
    .await?;
    return Ok(true);
  }
  let access = match authorize(db, clock, cfg, &probe, client, None).await {
    Ok(access) if access.generation == probe.authorization => access,
    _ => {
      let committed = probe
        .row
        .try_get::<Option<i64>>("", "create_committed_at")?
        .is_some();
      record(
        db,
        clock,
        &probe,
        if committed { "attention" } else { "complete" },
        "credentials_unavailable",
        "failed",
        if committed {
          "unresolved"
        } else {
          "not_created"
        },
        false,
        false,
        0,
      )
      .await?;
      return Ok(true);
    }
  };
  match probe.phase.as_str() {
    "prepared" => create(db, clock, cfg, &probe, client, access).await?,
    "create_committed" | "observing" => observe(db, clock, cfg, &probe, client, access).await?,
    "cleanup" => cleanup(db, clock, cfg, &probe, client, access).await?,
    _ => return Err(Error::Unavailable),
  }
  Ok(true)
}
#[allow(clippy::too_many_arguments)]
async fn record(
  db: &Db,
  clock: &dyn Clock,
  p: &Probe,
  phase: &str,
  reason: &str,
  conference: &str,
  cleanup: &str,
  created: bool,
  own: bool,
  delay: i64,
) -> Result<(), Error> {
  record_evidence(
    db,
    clock,
    p,
    Evidence {
      phase,
      reason,
      conference,
      cleanup,
      created,
      own,
      read_and_deleted: false,
      delay,
    },
  )
  .await
}
async fn record_evidence(
  db: &Db,
  clock: &dyn Clock,
  p: &Probe,
  evidence: Evidence<'_>,
) -> Result<(), Error> {
  tokio::time::timeout(
    std::time::Duration::from_secs(3),
    probes::record(db, clock, p, evidence),
  )
  .await
  .map_err(|_| Error::Unavailable)?
}
async fn authorize(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  p: &Probe,
  client: &Client,
  force: Option<i64>,
) -> Result<credentials::Access, Error> {
  // Reserve even when another process already refreshed. Conservative budgeting
  // bounds ALL cleanup HTTP, including any OAuth rotation, after a crash.
  if p.phase == "cleanup" && !probes::dispatch(db, clock, p, "cleanup").await? {
    return Err(Error::Unavailable);
  }
  credentials::access(
    db,
    clock,
    cfg.google.as_ref().ok_or(Error::Unavailable)?,
    client,
    force,
    Some(p),
  )
  .await
}
async fn metadata(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  p: &Probe,
  client: &Client,
  access: &mut credentials::Access,
) -> Result<bool, Error> {
  for retry in 0..2 {
    if !probes::dispatch(
      db,
      clock,
      p,
      if p.cleanup_only {
        "cleanup"
      } else {
        "metadata"
      },
    )
    .await?
    {
      return Ok(false);
    }
    match client.candidate(&access.token, &p.calendar).await {
      Ok(()) => return Ok(true),
      Err(Failure::Unauthorized) if retry == 0 => {
        *access = authorize(db, clock, cfg, p, client, Some(access.version)).await?;
        if access.generation != p.authorization {
          return Ok(false);
        }
      }
      Err(Failure::Unauthorized) => {
        credentials::unauthorized(db, access).await?;
        return Ok(false);
      }
      Err(_) => return Ok(false),
    }
  }
  Ok(false)
}
async fn create(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  p: &Probe,
  client: &Client,
  mut access: credentials::Access,
) -> Result<(), Error> {
  if !metadata(db, clock, cfg, p, client, &mut access).await? {
    return record(
      db,
      clock,
      p,
      "complete",
      "permission_unverified",
      "failed",
      "not_created",
      false,
      false,
      0,
    )
    .await;
  }
  let start = chrono::DateTime::from_timestamp_millis(p.row.try_get("", "event_start")?)
    .ok_or(Error::Unavailable)?;
  let end = start + chrono::Duration::minutes(5);
  let body = json!({"id":p.event,"summary":"Prueba de conexión de video — eliminar","start":{"dateTime":start.to_rfc3339()},"end":{"dateTime":end.to_rfc3339()},"transparency":"transparent","visibility":"private","reminders":{"useDefault":false},"extendedProperties":{"private":{"maPurpose":"video-capability-probe","maInstance":p.deployment,"maProbe":p.marker}},"conferenceData":{"createRequest":{"requestId":p.conference,"conferenceSolutionKey":{"type":"hangoutsMeet"}}}});
  let mut url = client.url(&["calendars", &p.calendar, "events"])?;
  url
    .query_pairs_mut()
    .extend_pairs([("conferenceDataVersion", "1"), ("sendUpdates", "none")]);
  let request = client.request(Method::POST, url, &access.token).json(&body);
  if !probes::dispatch(db, clock, p, "insert").await? {
    return Ok(());
  }
  // There is intentionally no retry around this sole insert invocation.
  let result = client.bounded(request, 256 * 1024).await;
  match result {
    Ok(reply) if reply.status.is_success() => {
      let event = serde_json::from_slice::<Value>(&reply.bytes).ok();
      let own = event.as_ref().is_some_and(|event| ours(p, event));
      record(
        db,
        clock,
        p,
        "observing",
        "observing",
        "pending",
        "pending",
        own,
        own,
        0,
      )
      .await
    }
    Ok(reply) if matches!(classify(&reply), Failure::Rejected) => {
      record(
        db,
        clock,
        p,
        "complete",
        "insert_rejected",
        "failed",
        "not_created",
        false,
        false,
        0,
      )
      .await
    }
    Ok(reply) if classify(&reply) == Failure::Unauthorized => {
      let _ = authorize(db, clock, cfg, p, client, Some(access.version)).await;
      record(
        db,
        clock,
        p,
        "observing",
        "create_uncertain",
        "pending",
        "pending",
        false,
        false,
        0,
      )
      .await
    }
    _ => {
      record(
        db,
        clock,
        p,
        "observing",
        "create_uncertain",
        "pending",
        "pending",
        false,
        false,
        0,
      )
      .await
    }
  }
}
fn ours(p: &Probe, event: &Value) -> bool {
  event.get("id").and_then(Value::as_str) == Some(&p.event)
    && event
      .pointer("/extendedProperties/private/maPurpose")
      .and_then(Value::as_str)
      == Some("video-capability-probe")
    && event
      .pointer("/extendedProperties/private/maInstance")
      .and_then(Value::as_str)
      == Some(&p.deployment)
    && event
      .pointer("/extendedProperties/private/maProbe")
      .and_then(Value::as_str)
      == Some(&p.marker)
}
fn conference(p: &Probe, event: &Value) -> &'static str {
  let data = &event["conferenceData"];
  if data["createRequest"]["requestId"].as_str() != Some(&p.conference) {
    return "failed";
  }
  match data["createRequest"]["status"]["statusCode"].as_str() {
    Some("failure") => "failed",
    Some("success") => {
      if data["conferenceSolution"]["key"]["type"] != "hangoutsMeet" {
        return "failed";
      }
      let Some(entries) = data["entryPoints"].as_array() else {
        return "failed";
      };
      let videos: Vec<_> = entries
        .iter()
        .filter(|e| e["entryPointType"] == "video")
        .collect();
      if videos.len() == 1 && videos[0]["uri"].as_str().is_some_and(valid_meet) {
        "success"
      } else {
        "failed"
      }
    }
    _ => "pending",
  }
}
pub(crate) fn valid_meet(raw: &str) -> bool {
  if raw.is_empty()
    || raw.len() > 1300
    || raw
      .chars()
      .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
  {
    return false;
  }
  let Ok(url) = reqwest::Url::parse(raw) else {
    return false;
  };
  if url.scheme() != "https"
    || url.host_str() != Some("meet.google.com")
    || !url.username().is_empty()
    || url.password().is_some()
    || url.port().is_some_and(|p| p != 443)
    || url.query().is_some()
    || url.fragment().is_some()
  {
    return false;
  }
  let Some((_, authority_path)) = raw.split_once("://") else {
    return false;
  };
  let Some(path_start) = authority_path.find('/') else {
    return false;
  };
  if &authority_path[path_start..] != url.path() {
    return false;
  }
  let Some(path) = url.path().strip_prefix('/') else {
    return false;
  };
  let parts: Vec<_> = path.split('-').collect();
  parts.len() == 3
    && parts
      .iter()
      .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()))
}
fn delay(attempt: i32) -> Result<i64, Error> {
  let seconds = match attempt {
    0 => 1,
    1 => 2,
    2 => 4,
    3 => 8,
    _ => 10,
  };
  Ok(seconds * 1000 + i64::from(super::crypto::random::<1>()?[0]))
}
async fn read_event(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  p: &Probe,
  client: &Client,
  access: &mut credentials::Access,
  kind: &str,
) -> Result<Result<Value, Failure>, Error> {
  for retry in 0..2 {
    let url = client.url(&["calendars", &p.calendar, "events", &p.event])?;
    if !probes::dispatch(db, clock, p, kind).await? {
      return Ok(Err(Failure::Unavailable));
    }
    let reply = client
      .bounded(client.request(Method::GET, url, &access.token), 256 * 1024)
      .await;
    let result = match reply {
      Ok(reply) if reply.status.is_success() => {
        serde_json::from_slice(&reply.bytes).map_err(|_| Failure::Unavailable)
      }
      Ok(reply) => Err(classify(&reply)),
      Err(error) => Err(error),
    };
    if result.as_ref().is_err_and(|e| *e == Failure::Unauthorized) {
      if retry == 0 {
        *access = authorize(db, clock, cfg, p, client, Some(access.version)).await?;
        if access.generation != p.authorization {
          return Err(Error::Invalid);
        }
        continue;
      }
      credentials::unauthorized(db, access).await?;
    }
    return Ok(result);
  }
  Ok(Err(Failure::Unavailable))
}
async fn observe(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  p: &Probe,
  client: &Client,
  mut access: credentials::Access,
) -> Result<(), Error> {
  let attempts: i32 = p.row.try_get("", "observations")?;
  let deadline = p
    .row
    .try_get::<Option<i64>>("", "observation_deadline")?
    .ok_or(Error::Unavailable)?;
  if attempts >= 12 || clock.now_millis() >= deadline {
    return record(
      db,
      clock,
      p,
      "cleanup",
      "observation_exhausted",
      "failed",
      "pending",
      false,
      false,
      0,
    )
    .await;
  }
  match read_event(db, clock, cfg, p, client, &mut access, "observe").await? {
    Ok(event) if ours(p, &event) => {
      let result = conference(p, &event);
      let next = if result == "pending" {
        "observing"
      } else {
        "cleanup"
      };
      record(
        db,
        clock,
        p,
        next,
        if result == "failed" {
          "conference_failed"
        } else {
          "observing"
        },
        result,
        "pending",
        false,
        true,
        if result == "pending" {
          delay(attempts)?
        } else {
          0
        },
      )
      .await
    }
    Ok(event)
      if event["id"] == p.event
        && event["status"] == "cancelled"
        && p.row.try_get::<bool>("", "own_verified")? =>
    {
      record(
        db, clock, p, "complete", "deleted", "failed", "deleted", false, true, 0,
      )
      .await
    }
    Ok(_) => {
      record(
        db,
        clock,
        p,
        "attention",
        "markers_mismatch",
        "failed",
        "unresolved",
        false,
        false,
        0,
      )
      .await
    }
    Err(Failure::Deleted) if p.row.try_get::<bool>("", "own_verified")? => {
      record(
        db, clock, p, "complete", "deleted", "failed", "deleted", false, true, 0,
      )
      .await
    }
    Err(Failure::Rejected | Failure::Unauthorized | Failure::InvalidGrant) => {
      record(
        db,
        clock,
        p,
        "attention",
        "cleanup_permission",
        "failed",
        "unresolved",
        false,
        false,
        0,
      )
      .await
    }
    Err(Failure::Transient(seconds)) => {
      let wait = delay(attempts)?.max(i64::from(seconds) * 1000);
      if clock.now_millis() + wait >= deadline {
        record(
          db,
          clock,
          p,
          if wait >= 300_000 {
            "attention"
          } else {
            "cleanup"
          },
          "observation_exhausted",
          "failed",
          if wait >= 300_000 {
            "unresolved"
          } else {
            "pending"
          },
          false,
          false,
          wait,
        )
        .await
      } else {
        record(
          db,
          clock,
          p,
          "observing",
          "provider_unavailable",
          "pending",
          "pending",
          false,
          false,
          wait,
        )
        .await
      }
    }
    Err(_) => {
      record(
        db,
        clock,
        p,
        "observing",
        "create_uncertain",
        "pending",
        "pending",
        false,
        false,
        delay(attempts)?,
      )
      .await
    }
  }
}
async fn cleanup(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  p: &Probe,
  client: &Client,
  mut access: credentials::Access,
) -> Result<(), Error> {
  if p.row.try_get::<i32>("", "cleanup_calls")? >= 12
    || p
      .row
      .try_get::<Option<i64>>("", "cleanup_deadline")?
      .is_none_or(|t| clock.now_millis() >= t)
  {
    return record(
      db,
      clock,
      p,
      "attention",
      "cleanup_exhausted",
      "pending",
      "unresolved",
      false,
      false,
      0,
    )
    .await;
  }
  if p.cleanup_only && !metadata(db, clock, cfg, p, client, &mut access).await? {
    return record(
      db,
      clock,
      p,
      "attention",
      "cleanup_permission",
      "pending",
      "unresolved",
      false,
      false,
      0,
    )
    .await;
  }
  let event = match read_event(db, clock, cfg, p, client, &mut access, "cleanup").await? {
    Ok(event) => event,
    Err(Failure::Deleted) if p.row.try_get::<bool>("", "own_verified")? => {
      return record(
        db, clock, p, "complete", "deleted", "pending", "deleted", false, true, 0,
      )
      .await
    }
    Err(Failure::Rejected | Failure::Unauthorized | Failure::InvalidGrant) => {
      return record(
        db,
        clock,
        p,
        "attention",
        "cleanup_permission",
        "pending",
        "unresolved",
        false,
        false,
        0,
      )
      .await
    }
    Err(Failure::Transient(seconds)) => {
      return cleanup_delay(db, clock, p, i64::from(seconds) * 1000, false).await
    }
    Err(_) => {
      return record(
        db,
        clock,
        p,
        "cleanup",
        "cleanup_uncertain",
        "pending",
        "unresolved",
        false,
        false,
        10_000,
      )
      .await
    }
  };
  if event["id"] == p.event
    && event["status"] == "cancelled"
    && p.row.try_get::<bool>("", "own_verified")?
  {
    return record(
      db, clock, p, "complete", "deleted", "pending", "deleted", false, true, 0,
    )
    .await;
  }
  if !ours(p, &event) {
    return record(
      db,
      clock,
      p,
      "attention",
      "markers_mismatch",
      "pending",
      "unresolved",
      false,
      false,
      0,
    )
    .await;
  }
  let Some(etag) = event.get("etag").and_then(Value::as_str).filter(|v| {
    v.len() >= 3
      && v.len() <= 1024
      && v.starts_with('"')
      && v.ends_with('"')
      && v.as_bytes()[1..v.len() - 1]
        .iter()
        .all(|b| b.is_ascii_graphic() && !matches!(*b, b'"' | b'\\'))
  }) else {
    return record(
      db,
      clock,
      p,
      "attention",
      "missing_etag",
      "pending",
      "unresolved",
      false,
      true,
      0,
    )
    .await;
  };
  let url = client.url(&["calendars", &p.calendar, "events", &p.event])?;
  if !probes::dispatch(db, clock, p, "cleanup").await? {
    return record(
      db,
      clock,
      p,
      "cleanup",
      "cleanup_uncertain",
      "pending",
      "unresolved",
      false,
      true,
      0,
    )
    .await;
  }
  let result = client
    .bounded(
      client
        .request(Method::DELETE, url, &access.token)
        .header("if-match", etag),
      64 * 1024,
    )
    .await;
  match result {
    Ok(reply) if reply.status.is_success() && reply.bytes.is_empty() => {
      record_evidence(
        db,
        clock,
        p,
        Evidence {
          phase: "complete",
          reason: "deleted",
          conference: "pending",
          cleanup: "deleted",
          created: false,
          own: true,
          read_and_deleted: true,
          delay: 0,
        },
      )
      .await
    }
    Ok(reply) if classify(&reply) == Failure::Deleted => {
      record(
        db, clock, p, "complete", "deleted", "pending", "deleted", false, true, 0,
      )
      .await
    }
    Ok(reply) if classify(&reply) == Failure::Unauthorized => {
      let attempted = p.row.try_get::<bool>("", "delete_auth_attempted")?;
      if attempted {
        credentials::unauthorized(db, &access).await?;
      } else {
        probes::begin_delete_correction(db, clock, p).await?;
        let _ = authorize(db, clock, cfg, p, client, Some(access.version)).await;
      }
      record(
        db,
        clock,
        p,
        if attempted { "attention" } else { "cleanup" },
        "cleanup_permission",
        "pending",
        "unresolved",
        false,
        true,
        1_000,
      )
      .await
    }
    Ok(reply) if classify(&reply) == Failure::Rejected => {
      record(
        db,
        clock,
        p,
        "attention",
        "cleanup_permission",
        "pending",
        "unresolved",
        false,
        true,
        0,
      )
      .await
    }
    Ok(reply) if matches!(classify(&reply), Failure::Transient(_)) => {
      cleanup_delay(db, clock, p, i64::from(reply.retry_after) * 1000, true).await
    }
    _ => {
      record(
        db,
        clock,
        p,
        "cleanup",
        "cleanup_uncertain",
        "pending",
        "unresolved",
        false,
        true,
        1_000,
      )
      .await
    }
  }
}
async fn cleanup_delay(
  db: &Db,
  clock: &dyn Clock,
  p: &Probe,
  wait: i64,
  own: bool,
) -> Result<(), Error> {
  let wait = wait.max(1_000);
  let exhausted = p
    .row
    .try_get::<Option<i64>>("", "cleanup_deadline")?
    .is_none_or(|t| clock.now_millis() + wait >= t);
  record(
    db,
    clock,
    p,
    if exhausted { "attention" } else { "cleanup" },
    "cleanup_uncertain",
    "pending",
    "unresolved",
    false,
    own,
    wait,
  )
  .await
}
