use super::{
  calendars,
  client::{classify, Client, Failure, Reply},
  config::GoogleConfig,
  credentials::{self, Access},
  store, Error,
};
use crate::{
  common::config::Config,
  database::Db,
  modules::{
    appointments::{availability::Check, booking, model::Appointment, store as appointment_store},
    conversation::store::{enqueue_text, Message},
    doctor::Clock,
  },
};
use chrono::SecondsFormat;
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use serde_json::{json, Value};
use std::collections::HashSet;
use uuid::Uuid;

const CLAIM_MILLIS: i64 = 60_000;
const CONFLICT_INTERVAL_MILLIS: i64 = 3 * 60_000;
const MEET_POLL_MILLIS: i64 = 2_000;

#[derive(Clone)]
struct Selection {
  generation: i64,
  version: i64,
  booking: String,
  blocking: Vec<String>,
}

/// Reads all Google busy ranges or fails closed. Booking Calendar events are
/// read with events.list; other selected Blocking Calendars use freeBusy only.
pub async fn availability(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
  start: i64,
  end: i64,
  exclude_event: Option<&str>,
) -> Result<Vec<(i64, i64)>, Error> {
  let config = cfg.google.as_ref().ok_or(Error::Unavailable)?;
  let client = client.ok_or(Error::Unavailable)?;
  if start >= end {
    return Err(Error::Invalid);
  }
  if exclude_event.is_some_and(|id| !valid_event_id(id)) {
    return Err(Error::Invalid);
  }
  let selected = selection(db).await?;
  let own = own_event_ids(db, exclude_event).await?;
  let mut access = credentials::access(db, clock, config, client, None, None).await?;
  if access.generation != selected.generation {
    return Err(Error::Unavailable);
  }
  let busy = match read_busy(client, &access.token, &selected, start, end, &own).await {
    Ok(busy) => busy,
    Err(Failure::Unauthorized) => {
      access = credentials::access(db, clock, config, client, Some(access.version), None).await?;
      if access.generation != selected.generation {
        return Err(Error::Unavailable);
      }
      match read_busy(client, &access.token, &selected, start, end, &own).await {
        Ok(busy) => busy,
        Err(Failure::Unauthorized) => {
          credentials::unauthorized(db, &access).await?;
          return Err(Error::Reauthorize);
        }
        Err(failure) => return Err(failure.into()),
      }
    }
    Err(failure) => return Err(failure.into()),
  };
  let current = selection(db).await?;
  if current.generation != selected.generation || current.version != selected.version {
    return Err(Error::Unavailable);
  }
  Ok(merge(busy))
}

async fn selection(db: &Db) -> Result<Selection, Error> {
  let row = store::load(db).await?;
  let status: String = row.try_get("", "status")?;
  if status != "connected" {
    return Err(if status == "reauthorize" {
      Error::Reauthorize
    } else {
      Error::Unavailable
    });
  }
  let booking: Option<String> = row.try_get("", "booking_calendar")?;
  let booking = booking.ok_or(Error::Unavailable)?;
  if !calendars::valid_id(&booking) {
    return Err(Error::Unavailable);
  }
  let values: Value = row.try_get("", "blocking_calendars")?;
  let values = values.as_array().ok_or(Error::Unavailable)?;
  if values.len() > 100 {
    return Err(Error::Unavailable);
  }
  let mut seen = HashSet::new();
  let mut blocking = Vec::with_capacity(values.len());
  for value in values {
    let id = value.as_str().ok_or(Error::Unavailable)?;
    if !calendars::valid_id(id) || !seen.insert(id) {
      return Err(Error::Unavailable);
    }
    blocking.push(id.to_owned());
  }
  Ok(Selection {
    generation: row.try_get("", "generation")?,
    version: row.try_get("", "selection_version")?,
    booking,
    blocking,
  })
}

async fn own_event_ids(db: &Db, exclude_event: Option<&str>) -> Result<HashSet<String>, Error> {
  let rows = db
    .conn
    .query_all(store::sql(
      "SELECT event_id FROM appointments WHERE event_id IS NOT NULL",
      vec![],
    ))
    .await?;
  let mut ids = HashSet::with_capacity(rows.len() + usize::from(exclude_event.is_some()));
  for row in rows {
    let id: String = row.try_get("", "event_id")?;
    if !valid_event_id(&id) {
      return Err(Error::Unavailable);
    }
    ids.insert(id);
  }
  if let Some(id) = exclude_event {
    ids.insert(id.to_owned());
  }
  Ok(ids)
}

async fn read_busy(
  client: &Client,
  access: &str,
  selection: &Selection,
  start: i64,
  end: i64,
  own: &HashSet<String>,
) -> Result<Vec<(i64, i64)>, Failure> {
  let mut busy = if selection.blocking.contains(&selection.booking) {
    client
      .booking_busy(access, &selection.booking, start, end, own)
      .await?
  } else {
    Vec::new()
  };
  let others: Vec<_> = selection
    .blocking
    .iter()
    .filter(|id| id.as_str() != selection.booking)
    .cloned()
    .collect();
  busy.extend(
    client
      .freebusy_intervals(access, &others, start, end)
      .await?,
  );
  Ok(busy)
}

fn merge(mut ranges: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
  ranges.sort_unstable();
  let mut merged: Vec<(i64, i64)> = Vec::with_capacity(ranges.len());
  for (start, end) in ranges {
    if let Some(last) = merged.last_mut() {
      if start <= last.1 {
        last.1 = last.1.max(end);
        continue;
      }
    }
    merged.push((start, end));
  }
  merged
}

#[derive(Clone)]
struct Job {
  id: Uuid,
  appointment: Uuid,
  kind: String,
  key: String,
  claim: Uuid,
  attempts: i32,
}

struct EventResult {
  etag: String,
  meet_url: Option<String>,
  pending: bool,
}

enum MeetState {
  Missing,
  Pending,
  Failed,
  Success(String),
}

/// Processes at most one persisted appointment Calendar job. Provider HTTP is
/// always outside the short PostgreSQL claim/result transactions.
pub async fn run_once(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
) -> Result<bool, Error> {
  let Some(job) = claim(db, clock).await? else {
    return Ok(false);
  };
  if let Err(error) = process(db, clock, cfg, client, &job).await {
    record_failure(db, clock, cfg, &job, error).await?;
  }
  Ok(true)
}

async fn claim(db: &Db, clock: &dyn Clock) -> Result<Option<Job>, Error> {
  let now = clock.now_millis();
  let tx = store::transaction(db).await?;
  let Some(candidate) = tx
    .query_one(store::sql(
      "SELECT id,appointment_id FROM appointment_jobs WHERE kind IN ('calendar_sync','calendar_cancel','conflict') AND finished_at IS NULL AND due_at<=$1 AND (claim_until IS NULL OR claim_until<=$1) ORDER BY due_at,id LIMIT 1",
      vec![now.into()],
    ))
    .await?
  else {
    tx.commit().await?;
    return Ok(None);
  };
  let id: Uuid = candidate.try_get("", "id")?;
  let appointment: Uuid = candidate.try_get("", "appointment_id")?;
  let Some(owner) = tx
    .query_one(store::sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![appointment.into()],
    ))
    .await?
  else {
    tx.commit().await?;
    return Ok(None);
  };
  let contact: Uuid = owner.try_get("", "contact_id")?;
  if tx
    .query_one(store::sql(
      "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
      vec![contact.into()],
    ))
    .await?
    .is_none()
    || tx
      .query_one(store::sql(
        "SELECT id FROM appointments WHERE id=$1 FOR UPDATE",
        vec![appointment.into()],
      ))
      .await?
      .is_none()
  {
    tx.commit().await?;
    return Ok(None);
  }
  let Some(row) = tx
    .query_one(store::sql(
      "SELECT kind,dedupe_key,attempts FROM appointment_jobs WHERE id=$1 AND finished_at IS NULL AND due_at<=$2 AND (claim_until IS NULL OR claim_until<=$2) FOR UPDATE SKIP LOCKED",
      vec![id.into(), now.into()],
    ))
    .await?
  else {
    tx.commit().await?;
    return Ok(None);
  };
  let kind: String = row.try_get("", "kind")?;
  if matches!(kind.as_str(), "calendar_sync" | "calendar_cancel")
    && tx
      .query_one(store::sql(
        "SELECT id FROM appointment_jobs WHERE appointment_id=$1 AND id<>$2 AND kind IN ('calendar_sync','calendar_cancel') AND finished_at IS NULL AND claim IS NOT NULL AND claim_until>$3",
        vec![appointment.into(), id.into(), now.into()],
      ))
      .await?
      .is_some()
  {
    tx.commit().await?;
    return Ok(None);
  }
  let key: String = row.try_get("", "dedupe_key")?;
  let attempts: i32 = row.try_get("", "attempts")?;
  let claim = Uuid::now_v7();
  let changed = tx
    .execute(store::sql(
      "UPDATE appointment_jobs SET claim=$2,claim_until=$3,attempts=attempts+1 WHERE id=$1 AND finished_at IS NULL",
      vec![id.into(), claim.into(), now.saturating_add(CLAIM_MILLIS).into()],
    ))
    .await?;
  if changed.rows_affected() != 1 {
    tx.rollback().await?;
    return Ok(None);
  }
  tx.commit().await?;
  Ok(Some(Job {
    id,
    appointment,
    kind,
    key,
    claim,
    attempts: attempts.saturating_add(1),
  }))
}

async fn process(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
  job: &Job,
) -> Result<(), Error> {
  let now = clock.now_millis();
  let Some(appointment) = appointment_store::get(&db.conn, job.appointment).await? else {
    return finish_job(db, clock, job).await;
  };
  if !job_revision_matches(job, appointment.revision) {
    return finish_job(db, clock, job).await;
  }
  match job.kind.as_str() {
    "calendar_sync" if sync_eligible(&appointment) => {
      if appointment.status == "requested" {
        return recover_requested(db, clock, cfg, client, job, &appointment).await;
      }
      let result = sync_event(db, clock, cfg, client, &appointment).await?;
      if result.pending {
        return save_progress(db, clock, job, &appointment, &result.etag).await;
      }
      finish_sync(db, clock, cfg, job, &appointment, result).await
    }
    "calendar_cancel" if appointment.status == "cancelled" => {
      cancel_event(db, clock, cfg, client, job, &appointment).await
    }
    "conflict" if conflict_eligible(&appointment, now) => {
      check_conflict(db, clock, cfg, client, job, &appointment).await
    }
    _ => finish_job(db, clock, job).await,
  }
}

async fn recover_requested(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
  job: &Job,
  appointment: &Appointment,
) -> Result<(), Error> {
  let checked =
    crate::modules::appointments::availability::recheck(db, clock, cfg, client, appointment)
      .await?;
  match checked {
    Check::Available => promote_requested(db, clock, job, appointment).await,
    Check::Conflict => {
      record_attention_and_retry(
        db,
        clock,
        job,
        Some(appointment),
        "calendar_conflict",
        false,
        CONFLICT_INTERVAL_MILLIS,
      )
      .await
    }
    Check::Unavailable => Err(Error::Unavailable),
  }
}

async fn promote_requested(
  db: &Db,
  clock: &dyn Clock,
  job: &Job,
  expected: &Appointment,
) -> Result<(), Error> {
  let now = clock.now_millis();
  let tx = store::transaction(db).await?;
  let Some(current) = lock_appointment(&tx, expected.id).await? else {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  };
  if !job_claim_valid(&tx, job, now).await?
    || current.revision != expected.revision
    || current.status != "requested"
  {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  }
  let revision = current.revision.checked_add(1).ok_or(Error::Unavailable)?;
  tx.execute(store::sql(
    "UPDATE appointments SET status='confirmed',calendar_sync='pending',revision=$2,updated_at=$3 WHERE id=$1 AND revision=$4",
    vec![current.id.into(), revision.into(), now.into(), current.revision.into()],
  ))
  .await?;
  appointment_store::history(
    &tx,
    current.id,
    "System",
    "calendar_availability_recovered",
    now,
  )
  .await?;
  rearm_job_in_tx(&tx, job, revision, now, None).await?;
  appointment_store::job(
    &tx,
    current.id,
    "conflict",
    now.saturating_add(CONFLICT_INTERVAL_MILLIS),
    &job_key(current.id, revision, "conflict"),
  )
  .await?;
  tx.commit().await?;
  Ok(())
}

async fn check_conflict(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
  job: &Job,
  appointment: &Appointment,
) -> Result<(), Error> {
  let before = i64::from(appointment.buffer_before_minutes).saturating_mul(60_000);
  let after = i64::from(appointment.buffer_after_minutes).saturating_mul(60_000);
  let start = appointment.starts_at.saturating_sub(before);
  let end = appointment.ends_at.saturating_add(after);
  let busy = availability(
    db,
    clock,
    cfg,
    client,
    start,
    end,
    appointment.event_id.as_deref(),
  )
  .await?;
  let conflict = busy.iter().any(|(from, to)| *from < end && *to > start);
  record_attention_and_retry(
    db,
    clock,
    job,
    Some(appointment),
    if conflict { "calendar_conflict" } else { "" },
    true,
    CONFLICT_INTERVAL_MILLIS,
  )
  .await
}

async fn record_attention_and_retry(
  db: &Db,
  clock: &dyn Clock,
  job: &Job,
  expected: Option<&Appointment>,
  kind: &str,
  google_verified: bool,
  delay: i64,
) -> Result<(), Error> {
  let now = clock.now_millis();
  let tx = store::transaction(db).await?;
  let current = if let Some(expected) = expected {
    lock_appointment(&tx, expected.id).await?
  } else {
    None
  };
  if !job_claim_valid(&tx, job, now).await? {
    tx.commit().await?;
    return Ok(());
  }
  if let Some(current) = current {
    if expected.is_some_and(|expected| {
      expected.revision != current.revision || !job_revision_matches(job, current.revision)
    }) {
      finish_job_in_tx(&tx, job, now).await?;
      tx.commit().await?;
      return Ok(());
    }
    if google_verified {
      tx.execute(store::sql(
        "UPDATE attention_items SET resolved_at=$2 WHERE appointment_id=$1 AND resolved_at IS NULL AND kind IN ('calendar_check_unavailable','google_reauthorization_required')",
        vec![current.id.into(), now.into()],
      ))
      .await?;
    }
    if !kind.is_empty() {
      appointment_store::attention(&tx, Some(current.id), None, kind, now).await?;
    }
    rearm_job_in_tx(&tx, job, current.revision, now.saturating_add(delay), None).await?;
  } else if expected.is_none() {
    rearm_job_in_tx(&tx, job, 0, now.saturating_add(delay), None).await?;
  } else {
    finish_job_in_tx(&tx, job, now).await?;
  }
  tx.commit().await?;
  Ok(())
}

async fn sync_event(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
  appointment: &Appointment,
) -> Result<EventResult, Error> {
  let config = cfg.google.as_ref().ok_or(Error::Unavailable)?;
  let client = client.ok_or(Error::Unavailable)?;
  let calendar = appointment
    .booking_calendar
    .as_deref()
    .ok_or(Error::Unavailable)?;
  if !calendars::valid_id(calendar) {
    return Err(Error::Unavailable);
  }
  let event_id = appointment.event_id.as_deref().ok_or(Error::Unavailable)?;
  if !valid_event_id(event_id) {
    return Err(Error::Unavailable);
  }
  let video = appointment.format == "video";
  let conference_id = conference_request_id(event_id);
  let mut access = authorize_calendar(db, clock, config, client, calendar).await?;
  let desired = event_body(appointment, cfg)?;
  let request_meet = json!({
    "conferenceData": {
      "createRequest": {
        "requestId": conference_id,
        "conferenceSolutionKey": {"type":"hangoutsMeet"}
      }
    }
  });

  if appointment.event_etag.is_none() {
    let mut body = desired.clone();
    if video {
      merge_object(&mut body, &request_meet)?;
    }
    let reply = client
      .insert_event(&access.token, calendar, &body, video)
      .await;
    return mutation_result(
      db,
      clock,
      config,
      client,
      cfg,
      EventMutation {
        access: &mut access,
        calendar,
        event_id,
        appointment,
        video,
        conference_id: &conference_id,
        allow_schedule_reconcile: true,
      },
      reply,
    )
    .await;
  }

  let event = read_event(db, clock, config, client, &mut access, calendar, event_id).await?;
  if !event_identity_matches(appointment, cfg, &event)? {
    return Err(Error::Unavailable);
  }
  let etag = event_etag(&event)?;
  let meets = if video {
    Some(meet_state(&event, &conference_id)?)
  } else {
    None
  };
  let current_schedule_matches = event_schedule_matches(appointment, &event)?;
  if current_schedule_matches {
    if let Some(state) = meets {
      match state {
        MeetState::Success(url) => {
          return Ok(EventResult {
            etag,
            meet_url: Some(url),
            pending: false,
          });
        }
        MeetState::Pending => {
          return Ok(EventResult {
            etag,
            meet_url: None,
            pending: true,
          });
        }
        MeetState::Missing | MeetState::Failed => {
          let mut body = json!({});
          merge_object(&mut body, &request_meet)?;
          let reply = client
            .patch_event(&access.token, calendar, event_id, &body, true, &etag)
            .await;
          return mutation_result(
            db,
            clock,
            config,
            client,
            cfg,
            EventMutation {
              access: &mut access,
              calendar,
              event_id,
              appointment,
              video: true,
              conference_id: &conference_id,
              allow_schedule_reconcile: false,
            },
            reply,
          )
          .await;
        }
      }
    }
    return Ok(EventResult {
      etag,
      meet_url: None,
      pending: false,
    });
  }

  let mut body = desired;
  if video && matches!(meets, Some(MeetState::Missing | MeetState::Failed)) {
    merge_object(&mut body, &request_meet)?;
  }
  if let Some(object) = body.as_object_mut() {
    object.remove("id");
  }
  let reply = client
    .patch_event(&access.token, calendar, event_id, &body, video, &etag)
    .await;
  mutation_result(
    db,
    clock,
    config,
    client,
    cfg,
    EventMutation {
      access: &mut access,
      calendar,
      event_id,
      appointment,
      video,
      conference_id: &conference_id,
      allow_schedule_reconcile: false,
    },
    reply,
  )
  .await
}

async fn authorize_calendar(
  db: &Db,
  clock: &dyn Clock,
  config: &super::config::GoogleConfig,
  client: &Client,
  calendar: &str,
) -> Result<Access, Error> {
  let mut access = credentials::access(db, clock, config, client, None, None).await?;
  let result = client.candidate(&access.token, calendar).await;
  if result == Err(Failure::Unauthorized) {
    access = credentials::access(db, clock, config, client, Some(access.version), None).await?;
    let result = client.candidate(&access.token, calendar).await;
    if result == Err(Failure::Unauthorized) {
      credentials::unauthorized(db, &access).await?;
    }
    result?;
  } else {
    result?;
  }
  Ok(access)
}

async fn refresh_access(
  db: &Db,
  clock: &dyn Clock,
  config: &super::config::GoogleConfig,
  client: &Client,
  access: &Access,
) -> Result<Access, Error> {
  credentials::access(db, clock, config, client, Some(access.version), None).await
}

async fn read_event(
  db: &Db,
  clock: &dyn Clock,
  config: &super::config::GoogleConfig,
  client: &Client,
  access: &mut Access,
  calendar: &str,
  event_id: &str,
) -> Result<Value, Error> {
  let mut reply = client.get_event(&access.token, calendar, event_id).await;
  if is_unauthorized(&reply) {
    *access = refresh_access(db, clock, config, client, access).await?;
    reply = client.get_event(&access.token, calendar, event_id).await;
    if is_unauthorized(&reply) {
      credentials::unauthorized(db, access).await?;
      return Err(Error::Reauthorize);
    }
  }
  let reply = reply?;
  if !reply.status.is_success() {
    return Err(classify(&reply).into());
  }
  serde_json::from_slice(&reply.bytes).map_err(|_| Error::Unavailable)
}

fn is_unauthorized(reply: &Result<Reply, Failure>) -> bool {
  matches!(reply, Err(Failure::Unauthorized))
    || matches!(reply, Ok(reply) if reply.status.as_u16() == 401)
}

struct EventMutation<'a> {
  access: &'a mut Access,
  calendar: &'a str,
  event_id: &'a str,
  appointment: &'a Appointment,
  video: bool,
  conference_id: &'a str,
  allow_schedule_reconcile: bool,
}

async fn mutation_result(
  db: &Db,
  clock: &dyn Clock,
  config: &super::config::GoogleConfig,
  client: &Client,
  cfg: &Config,
  mutation: EventMutation<'_>,
  reply: Result<Reply, Failure>,
) -> Result<EventResult, Error> {
  let EventMutation {
    access,
    calendar,
    event_id,
    appointment,
    video,
    conference_id,
    mut allow_schedule_reconcile,
  } = mutation;
  let mut reply = reply;
  loop {
    let event = match reply {
      Ok(reply) if reply.status.is_success() => {
        if let Ok(event) = serde_json::from_slice::<Value>(&reply.bytes) {
          if event_identity_matches(appointment, cfg, &event)?
            && event_schedule_matches(appointment, &event)?
          {
            let etag = event_etag(&event)?;
            return if video {
              event_result(&event, &etag, conference_id)
            } else {
              Ok(EventResult {
                etag,
                meet_url: None,
                pending: false,
              })
            };
          }
        }
        read_event(db, clock, config, client, access, calendar, event_id).await?
      }
      Ok(reply) => {
        match classify(&reply) {
          Failure::Rejected => return Err(Error::Unavailable),
          Failure::Unauthorized => {
            *access = refresh_access(db, clock, config, client, access).await?;
          }
          _ => {}
        }
        read_event(db, clock, config, client, access, calendar, event_id).await?
      }
      Err(Failure::Unauthorized) => {
        *access = refresh_access(db, clock, config, client, access).await?;
        read_event(db, clock, config, client, access, calendar, event_id).await?
      }
      Err(Failure::Rejected) => return Err(Error::Unavailable),
      Err(_) => read_event(db, clock, config, client, access, calendar, event_id).await?,
    };

    if !event_identity_matches(appointment, cfg, &event)? {
      return Err(Error::Unavailable);
    }
    if event_schedule_matches(appointment, &event)? {
      let etag = event_etag(&event)?;
      return if video {
        event_result(&event, &etag, conference_id)
      } else {
        Ok(EventResult {
          etag,
          meet_url: None,
          pending: false,
        })
      };
    }
    if !allow_schedule_reconcile {
      return Err(Error::Unavailable);
    }

    let etag = event_etag(&event)?;
    let mut body = event_body(appointment, cfg)?;
    if let Some(object) = body.as_object_mut() {
      object.remove("id");
    }
    if video
      && matches!(
        meet_state(&event, conference_id)?,
        MeetState::Missing | MeetState::Failed
      )
    {
      merge_object(
        &mut body,
        &json!({"conferenceData":{"createRequest":{"requestId":conference_id,"conferenceSolutionKey":{"type":"hangoutsMeet"}}}}),
      )?;
    }
    allow_schedule_reconcile = false;
    reply = client
      .patch_event(&access.token, calendar, event_id, &body, video, &etag)
      .await;
  }
}

fn event_result(event: &Value, etag: &str, conference_id: &str) -> Result<EventResult, Error> {
  match meet_state(event, conference_id)? {
    MeetState::Success(url) => Ok(EventResult {
      etag: etag.to_owned(),
      meet_url: Some(url),
      pending: false,
    }),
    MeetState::Pending => Ok(EventResult {
      etag: etag.to_owned(),
      meet_url: None,
      pending: true,
    }),
    MeetState::Missing | MeetState::Failed => Err(Error::Unavailable),
  }
}

fn event_body(appointment: &Appointment, cfg: &Config) -> Result<Value, Error> {
  let origin = cfg.public_origin.as_deref().ok_or(Error::Unavailable)?;
  let start =
    chrono::DateTime::from_timestamp_millis(appointment.starts_at).ok_or(Error::Unavailable)?;
  let end =
    chrono::DateTime::from_timestamp_millis(appointment.ends_at).ok_or(Error::Unavailable)?;
  let format = match appointment.format.as_str() {
    "in_person" => "presencial",
    "video" => "video",
    _ => return Err(Error::Unavailable),
  };
  let description = format!(
    "Servicio: {}\nFormato: {}\n{}/dashboard",
    appointment.service_name, format, origin
  );
  let body = json!({
    "id": appointment.event_id.as_deref().ok_or(Error::Unavailable)?,
    "summary": format!("Cita: {}", appointment.patient_name),
    "description": description,
    "start": {"dateTime": start.to_rfc3339_opts(SecondsFormat::Millis, true)},
    "end": {"dateTime": end.to_rfc3339_opts(SecondsFormat::Millis, true)}
  });
  Ok(body)
}

fn event_identity_matches(
  appointment: &Appointment,
  cfg: &Config,
  event: &Value,
) -> Result<bool, Error> {
  let expected = event_body(appointment, cfg)?;
  let status = event
    .get("status")
    .and_then(Value::as_str)
    .ok_or(Error::Unavailable)?;
  Ok(
    event.get("id") == expected.get("id")
      && event.get("summary") == expected.get("summary")
      && event.get("description") == expected.get("description")
      && matches!(status, "confirmed" | "tentative"),
  )
}

fn event_schedule_matches(appointment: &Appointment, event: &Value) -> Result<bool, Error> {
  Ok(
    event_timestamp(event.get("start"))? == appointment.starts_at
      && event_timestamp(event.get("end"))? == appointment.ends_at,
  )
}

fn event_timestamp(value: Option<&Value>) -> Result<i64, Error> {
  let value = value.ok_or(Error::Unavailable)?;
  if let Some(raw) = value.get("dateTime").and_then(Value::as_str) {
    return chrono::DateTime::parse_from_rfc3339(raw)
      .map(|date| date.timestamp_millis())
      .map_err(|_| Error::Unavailable);
  }
  Err(Error::Unavailable)
}

fn event_etag(event: &Value) -> Result<String, Error> {
  event
    .get("etag")
    .and_then(Value::as_str)
    .filter(|etag| strong_etag(etag))
    .map(str::to_owned)
    .ok_or(Error::Unavailable)
}

fn strong_etag(etag: &str) -> bool {
  etag.len() >= 3
    && etag.len() <= 1024
    && etag.starts_with('"')
    && etag.ends_with('"')
    && !etag[1..etag.len() - 1].contains('"')
    && !etag.chars().any(char::is_control)
}

fn meet_state(event: &Value, request_id: &str) -> Result<MeetState, Error> {
  let Some(data) = event.get("conferenceData") else {
    return Ok(MeetState::Missing);
  };
  let Some(request) = data.get("createRequest") else {
    return Ok(MeetState::Missing);
  };
  if request.is_null() {
    return Ok(MeetState::Missing);
  }
  if request.get("requestId").and_then(Value::as_str) != Some(request_id) {
    return Err(Error::Unavailable);
  }
  match request
    .pointer("/status/statusCode")
    .and_then(Value::as_str)
  {
    Some("pending") => Ok(MeetState::Pending),
    Some("failure") => Ok(MeetState::Failed),
    Some("success") => {
      if data
        .pointer("/conferenceSolution/key/type")
        .and_then(Value::as_str)
        != Some("hangoutsMeet")
      {
        return Err(Error::Unavailable);
      }
      let entries = data
        .get("entryPoints")
        .and_then(Value::as_array)
        .ok_or(Error::Unavailable)?;
      let mut videos = entries
        .iter()
        .filter(|entry| entry.get("entryPointType").and_then(Value::as_str) == Some("video"));
      let video = videos.next().ok_or(Error::Unavailable)?;
      if videos.next().is_some() {
        return Err(Error::Unavailable);
      }
      let url = video
        .get("uri")
        .and_then(Value::as_str)
        .filter(|url| super::worker::valid_meet(url))
        .ok_or(Error::Unavailable)?;
      Ok(MeetState::Success(url.to_owned()))
    }
    _ => Err(Error::Unavailable),
  }
}

fn conference_request_id(event_id: &str) -> String {
  format!("ma-{event_id}")
}

fn merge_object(target: &mut Value, source: &Value) -> Result<(), Error> {
  let target = target.as_object_mut().ok_or(Error::Unavailable)?;
  let source = source.as_object().ok_or(Error::Unavailable)?;
  for (key, value) in source {
    target.insert(key.clone(), value.clone());
  }
  Ok(())
}

async fn save_progress(
  db: &Db,
  clock: &dyn Clock,
  job: &Job,
  expected: &Appointment,
  etag: &str,
) -> Result<(), Error> {
  let now = clock.now_millis();
  let tx = store::transaction(db).await?;
  let Some(current) = lock_appointment(&tx, expected.id).await? else {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  };
  if !job_claim_valid(&tx, job, now).await?
    || current.revision != expected.revision
    || !sync_eligible(&current)
  {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  }
  tx.execute(store::sql(
    "UPDATE appointments SET event_etag=$2 WHERE id=$1",
    vec![current.id.into(), etag.to_owned().into()],
  ))
  .await?;
  rearm_job_in_tx(
    &tx,
    job,
    current.revision,
    now.saturating_add(MEET_POLL_MILLIS),
    None,
  )
  .await?;
  tx.commit().await?;
  Ok(())
}

async fn finish_sync(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  job: &Job,
  expected: &Appointment,
  result: EventResult,
) -> Result<(), Error> {
  let now = clock.now_millis();
  let tx = store::transaction(db).await?;
  let Some(current) = lock_appointment(&tx, expected.id).await? else {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  };
  if !job_claim_valid(&tx, job, now).await?
    || current.revision != expected.revision
    || !sync_eligible(&current)
  {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  }
  if current.format == "video" && (current.payment != "verified" || result.meet_url.is_none()) {
    return Err(Error::Unavailable);
  }
  let state_changed = current.calendar_sync != "synced"
    || (current.format == "video" && current.meet_url != result.meet_url);
  let etag_changed = current.event_etag.as_deref() != Some(result.etag.as_str());
  if state_changed {
    let updated = tx
      .query_one(store::sql(
        "UPDATE appointments SET calendar_sync='synced',event_etag=$2,meet_url=CASE WHEN $3 THEN $4 ELSE meet_url END,revision=revision+1,updated_at=$5 WHERE id=$1 AND revision=$6 RETURNING revision,starts_at",
        vec![current.id.into(), result.etag.clone().into(), (current.format == "video").into(), result.meet_url.clone().into(), now.into(), current.revision.into()],
      ))
      .await?
      .ok_or(Error::Unavailable)?;
    let revision: i64 = updated.try_get("", "revision")?;
    let starts_at: i64 = updated.try_get("", "starts_at")?;
    appointment_store::history(
      &tx,
      current.id,
      "System",
      if current.format == "video" {
        "meet_ready"
      } else {
        "calendar_synced"
      },
      now,
    )
    .await?;
    tx.execute(store::sql(
      "UPDATE attention_items SET resolved_at=$2 WHERE appointment_id=$1 AND resolved_at IS NULL AND kind IN ('calendar_sync_failed','google_reauthorization_required','calendar_check_unavailable')",
      vec![current.id.into(), now.into()],
    ))
    .await?;
    let settings = tx
      .query_one(store::sql(
        "SELECT reminder_2h_enabled FROM practice WHERE id=1",
        vec![],
      ))
      .await?
      .ok_or(Error::Unavailable)?;
    booking::schedule_reminders(
      &tx,
      current.id,
      revision,
      starts_at,
      now,
      settings.try_get("", "reminder_2h_enabled")?,
    )
    .await?;
    enqueue_confirmation(
      &tx,
      cfg,
      &current,
      revision,
      result.meet_url.as_deref(),
      now,
    )
    .await?;
    if current.ends_at > now {
      appointment_store::job(
        &tx,
        current.id,
        "conflict",
        now.saturating_add(CONFLICT_INTERVAL_MILLIS),
        &job_key(current.id, revision, "conflict"),
      )
      .await?;
    }
  } else if etag_changed {
    tx.execute(store::sql(
      "UPDATE appointments SET event_etag=$2 WHERE id=$1",
      vec![current.id.into(), result.etag.into()],
    ))
    .await?;
  }
  finish_job_in_tx(&tx, job, now).await?;
  tx.commit().await?;
  Ok(())
}

async fn enqueue_confirmation(
  tx: &DatabaseTransaction,
  cfg: &Config,
  appointment: &Appointment,
  revision: i64,
  meet_url: Option<&str>,
  now: i64,
) -> Result<(), Error> {
  let local = local_time(appointment.starts_at, &appointment.timezone)?;
  let text = if appointment.format == "video" {
    let url = meet_url.ok_or(Error::Unavailable)?;
    format!("Su cita por video quedó confirmada para el {local}. Puede ingresar aquí: {url}")
  } else {
    format!(
      "Su cita quedó confirmada para el {local}, en {}.",
      appointment.location
    )
  };
  let key = format!(
    "appointment:{}:{revision}:calendar-confirmed",
    appointment.id
  );
  enqueue_text(
    tx,
    Message {
      contact: appointment.contact_id,
      appointment: Some(appointment.id),
      key: &key,
      text: &text,
      purpose: "status",
      revision: Some(revision),
    },
    cfg.public_origin.as_deref().ok_or(Error::Unavailable)?,
    now,
  )
  .await?;
  Ok(())
}

fn local_time(at: i64, timezone: &str) -> Result<String, Error> {
  let zone: chrono_tz::Tz = timezone.parse().map_err(|_| Error::Unavailable)?;
  let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at).ok_or(Error::Unavailable)?;
  let local = utc.with_timezone(&zone);
  Ok(format!(
    "{} a las {} ({timezone})",
    local.format("%d/%m/%Y"),
    local.format("%H:%M")
  ))
}

async fn cancel_event(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
  job: &Job,
  appointment: &Appointment,
) -> Result<(), Error> {
  let Some(event_id) = appointment.event_id.as_deref() else {
    return finish_cancel(db, clock, cfg, job, appointment).await;
  };
  if !valid_event_id(event_id) {
    return Err(Error::Unavailable);
  }
  let config = cfg.google.as_ref().ok_or(Error::Unavailable)?;
  let client = client.ok_or(Error::Unavailable)?;
  let calendar = appointment
    .booking_calendar
    .as_deref()
    .ok_or(Error::Unavailable)?;
  if !calendars::valid_id(calendar) {
    return Err(Error::Unavailable);
  }
  let mut access = authorize_calendar(db, clock, config, client, calendar).await?;
  let Some(event) =
    read_cancel_event(db, clock, config, client, &mut access, calendar, event_id).await?
  else {
    return finish_cancel(db, clock, cfg, job, appointment).await;
  };
  if event.get("status").and_then(Value::as_str) == Some("cancelled")
    && event.get("id").and_then(Value::as_str) == Some(event_id)
  {
    return finish_cancel(db, clock, cfg, job, appointment).await;
  }
  if !cancel_identity_matches(appointment, cfg, &event)? {
    return Err(Error::Unavailable);
  }
  let etag = event_etag(&event)?;
  let reply = client
    .delete_event(&access.token, calendar, event_id, &etag)
    .await;
  match reply {
    Ok(reply) if reply.status.is_success() => finish_cancel(db, clock, cfg, job, appointment).await,
    Ok(reply) if classify(&reply) == Failure::Deleted => {
      finish_cancel(db, clock, cfg, job, appointment).await
    }
    Ok(reply) if classify(&reply) == Failure::Rejected => Err(Error::Unavailable),
    Ok(reply) if classify(&reply) == Failure::Unauthorized => {
      access = refresh_access(db, clock, config, client, &access).await?;
      reconcile_cancel(
        db,
        clock,
        cfg,
        client,
        job,
        appointment,
        calendar,
        event_id,
        access,
      )
      .await
    }
    _ => {
      reconcile_cancel(
        db,
        clock,
        cfg,
        client,
        job,
        appointment,
        calendar,
        event_id,
        access,
      )
      .await
    }
  }
}

fn cancel_identity_matches(
  appointment: &Appointment,
  cfg: &Config,
  event: &Value,
) -> Result<bool, Error> {
  let Some(event_id) = appointment.event_id.as_deref() else {
    return Ok(false);
  };
  if event.get("id").and_then(Value::as_str) != Some(event_id) {
    return Ok(false);
  }
  if event.get("status").and_then(Value::as_str) == Some("cancelled") {
    return Ok(true);
  }
  let expected_identity = event_identity_matches(appointment, cfg, event)?;
  let expected_schedule = event_schedule_matches(appointment, event).unwrap_or(false);
  let etag_matches = appointment
    .event_etag
    .as_deref()
    .is_some_and(|etag| event.get("etag").and_then(Value::as_str) == Some(etag));
  Ok(expected_identity && (expected_schedule || etag_matches))
}
async fn read_cancel_event(
  db: &Db,
  clock: &dyn Clock,
  config: &GoogleConfig,
  client: &Client,
  access: &mut Access,
  calendar: &str,
  event_id: &str,
) -> Result<Option<Value>, Error> {
  let mut reply = client.get_event(&access.token, calendar, event_id).await;
  if is_unauthorized(&reply) {
    *access = refresh_access(db, clock, config, client, access).await?;
    reply = client.get_event(&access.token, calendar, event_id).await;
    if is_unauthorized(&reply) {
      credentials::unauthorized(db, access).await?;
      return Err(Error::Reauthorize);
    }
  }
  match reply {
    Ok(reply) if reply.status.is_success() => {
      let event = serde_json::from_slice(&reply.bytes).map_err(|_| Error::Unavailable)?;
      Ok(Some(event))
    }
    Ok(reply) if matches!(classify(&reply), Failure::NotFound | Failure::Deleted) => Ok(None),
    Err(Failure::NotFound | Failure::Deleted) => Ok(None),
    _ => Err(Error::Unavailable),
  }
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_cancel(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: &Client,
  job: &Job,
  appointment: &Appointment,
  calendar: &str,
  event_id: &str,
  mut access: Access,
) -> Result<(), Error> {
  let config = cfg.google.as_ref().ok_or(Error::Unavailable)?;
  let Some(event) =
    read_cancel_event(db, clock, config, client, &mut access, calendar, event_id).await?
  else {
    return finish_cancel(db, clock, cfg, job, appointment).await;
  };
  if event.get("status").and_then(Value::as_str) == Some("cancelled")
    && event.get("id").and_then(Value::as_str) == Some(event_id)
  {
    finish_cancel(db, clock, cfg, job, appointment).await
  } else {
    Err(Error::Unavailable)
  }
}

async fn finish_cancel(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  job: &Job,
  expected: &Appointment,
) -> Result<(), Error> {
  let now = clock.now_millis();
  let tx = store::transaction(db).await?;
  let Some(current) = lock_appointment(&tx, expected.id).await? else {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  };
  if !job_claim_valid(&tx, job, now).await?
    || current.revision != expected.revision
    || current.status != "cancelled"
  {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  }
  if current.event_id.is_some() && current.calendar_sync != "synced" {
    let revision = current.revision.checked_add(1).ok_or(Error::Unavailable)?;
    tx.execute(store::sql(
      "UPDATE appointments SET calendar_sync='synced',revision=$2,updated_at=$3 WHERE id=$1 AND revision=$4",
      vec![current.id.into(), revision.into(), now.into(), current.revision.into()],
    ))
    .await?;
    appointment_store::history(&tx, current.id, "System", "calendar_cancelled", now).await?;
    tx.execute(store::sql(
      "UPDATE attention_items SET resolved_at=$2 WHERE appointment_id=$1 AND resolved_at IS NULL AND kind IN ('calendar_sync_failed','google_reauthorization_required','calendar_check_unavailable')",
      vec![current.id.into(), now.into()],
    ))
    .await?;
    let local = local_time(current.starts_at, &current.timezone)?;
    let text = format!("Su cita del {local} quedó cancelada también en Google Calendar.");
    let key = format!("appointment:{}:{revision}:calendar-cancelled", current.id);
    enqueue_text(
      &tx,
      Message {
        contact: current.contact_id,
        appointment: Some(current.id),
        key: &key,
        text: &text,
        purpose: "status",
        revision: Some(revision),
      },
      cfg.public_origin.as_deref().ok_or(Error::Unavailable)?,
      now,
    )
    .await?;
  }
  finish_job_in_tx(&tx, job, now).await?;
  tx.commit().await?;
  Ok(())
}

fn sync_eligible(appointment: &Appointment) -> bool {
  matches!(
    (
      appointment.status.as_str(),
      appointment.format.as_str(),
      appointment.payment.as_str()
    ),
    ("requested" | "confirmed", "in_person", "not_required") | ("confirmed", "video", "verified")
  )
}

fn conflict_eligible(appointment: &Appointment, now: i64) -> bool {
  matches!(
    appointment.status.as_str(),
    "requested" | "awaiting_payment" | "confirmed"
  ) && appointment.ends_at > now
}

fn job_revision_matches(job: &Job, revision: i64) -> bool {
  job.key == job_key(job.appointment, revision, &job.kind)
}

fn job_key(appointment: Uuid, revision: i64, kind: &str) -> String {
  format!("{appointment}:{revision}:{kind}")
}

fn valid_event_id(id: &str) -> bool {
  id.len() == 32
    && id
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn retry_delay(attempts: i32) -> i64 {
  let exponent = u32::try_from(attempts.clamp(1, 9)).unwrap_or(9);
  1_000_i64.saturating_mul(1_i64 << exponent).min(5 * 60_000)
}

fn failure_code(error: &Error) -> &'static str {
  if matches!(error, Error::Reauthorize) {
    "reauthorize"
  } else {
    "provider_unavailable"
  }
}

async fn record_failure(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  job: &Job,
  error: Error,
) -> Result<(), Error> {
  let now = clock.now_millis();
  let tx = store::transaction(db).await?;
  let Some(current) = lock_appointment(&tx, job.appointment).await? else {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  };
  if !job_claim_valid(&tx, job, now).await? || !job_revision_matches(job, current.revision) {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  }
  if job.kind == "conflict" {
    appointment_store::attention(
      &tx,
      Some(current.id),
      None,
      "calendar_check_unavailable",
      now,
    )
    .await?;
    if matches!(&error, Error::Reauthorize) {
      appointment_store::attention(
        &tx,
        Some(current.id),
        None,
        "google_reauthorization_required",
        now,
      )
      .await?;
    }
    rearm_job_in_tx(
      &tx,
      job,
      current.revision,
      now.saturating_add(retry_delay(job.attempts)),
      Some(failure_code(&error)),
    )
    .await?;
    tx.commit().await?;
    return Ok(());
  }
  let sync_kind = matches!(job.kind.as_str(), "calendar_sync" | "calendar_cancel");
  if !sync_kind {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  }
  let valid_state = if job.kind == "calendar_cancel" {
    current.status == "cancelled"
  } else {
    sync_eligible(&current)
  };
  if !valid_state {
    finish_job_in_tx(&tx, job, now).await?;
    tx.commit().await?;
    return Ok(());
  }
  let changed = current.calendar_sync != "failed";
  let revision = if changed {
    let revision = current.revision.checked_add(1).ok_or(Error::Unavailable)?;
    tx.execute(store::sql(
      "UPDATE appointments SET calendar_sync='failed',revision=$2,updated_at=$3 WHERE id=$1 AND revision=$4",
      vec![current.id.into(), revision.into(), now.into(), current.revision.into()],
    ))
    .await?;
    appointment_store::history(&tx, current.id, "System", "calendar_sync_failed", now).await?;
    revision
  } else {
    current.revision
  };
  if changed
    && job.kind == "calendar_sync"
    && current.status != "cancelled"
    && current.ends_at > now
  {
    appointment_store::job(
      &tx,
      current.id,
      "conflict",
      now.saturating_add(CONFLICT_INTERVAL_MILLIS),
      &job_key(current.id, revision, "conflict"),
    )
    .await?;
  }
  appointment_store::attention(&tx, Some(current.id), None, "calendar_sync_failed", now).await?;
  if matches!(&error, Error::Reauthorize) {
    appointment_store::attention(
      &tx,
      Some(current.id),
      None,
      "google_reauthorization_required",
      now,
    )
    .await?;
  }
  if changed
    && ((job.kind == "calendar_sync" && current.status == "confirmed")
      || (job.kind == "calendar_cancel" && current.status == "cancelled"))
  {
    enqueue_pending(&tx, cfg, &current, revision, now).await?;
  }
  rearm_job_in_tx(
    &tx,
    job,
    revision,
    now.saturating_add(retry_delay(job.attempts)),
    Some(failure_code(&error)),
  )
  .await?;
  tx.commit().await?;
  Ok(())
}

async fn enqueue_pending(
  tx: &DatabaseTransaction,
  cfg: &Config,
  appointment: &Appointment,
  revision: i64,
  now: i64,
) -> Result<(), Error> {
  let text = if appointment.status == "cancelled" {
    "Su cita quedó cancelada; la actualización de Google Calendar sigue pendiente.".to_owned()
  } else if appointment.format == "video" {
    "El pago de su cita quedó verificado. La confirmación y el enlace siguen en proceso; no necesita pagar otra vez.".to_owned()
  } else {
    "Su cita está en proceso de confirmación. Le avisaremos cuando quede completada.".to_owned()
  };
  let key = format!("appointment:{}:{revision}:calendar-pending", appointment.id);
  enqueue_text(
    tx,
    Message {
      contact: appointment.contact_id,
      appointment: Some(appointment.id),
      key: &key,
      text: &text,
      purpose: "status",
      revision: Some(revision),
    },
    cfg.public_origin.as_deref().ok_or(Error::Unavailable)?,
    now,
  )
  .await?;
  Ok(())
}

async fn lock_appointment(
  tx: &DatabaseTransaction,
  id: Uuid,
) -> Result<Option<Appointment>, Error> {
  let Some(owner) = tx
    .query_one(store::sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  let contact: Uuid = owner.try_get("", "contact_id")?;
  tx.query_one(store::sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![contact.into()],
  ))
  .await?
  .ok_or(Error::Unavailable)?;
  if tx
    .query_one(store::sql(
      "SELECT id FROM appointments WHERE id=$1 FOR UPDATE",
      vec![id.into()],
    ))
    .await?
    .is_none()
  {
    return Ok(None);
  }
  appointment_store::get(tx, id).await.map_err(Into::into)
}

async fn job_claim_valid(tx: &DatabaseTransaction, job: &Job, now: i64) -> Result<bool, Error> {
  let row = tx
    .query_one(store::sql(
      "SELECT claim,claim_until,finished_at FROM appointment_jobs WHERE id=$1 FOR UPDATE",
      vec![job.id.into()],
    ))
    .await?;
  let Some(row) = row else { return Ok(false) };
  Ok(
    row.try_get::<Option<Uuid>>("", "claim")? == Some(job.claim)
      && row
        .try_get::<Option<i64>>("", "claim_until")?
        .is_some_and(|until| until > now)
      && row.try_get::<Option<i64>>("", "finished_at")?.is_none(),
  )
}

async fn finish_job(db: &Db, clock: &dyn Clock, job: &Job) -> Result<(), Error> {
  let now = clock.now_millis();
  let tx = store::transaction(db).await?;
  finish_job_in_tx(&tx, job, now).await?;
  tx.commit().await?;
  Ok(())
}

async fn finish_job_in_tx(tx: &DatabaseTransaction, job: &Job, now: i64) -> Result<(), Error> {
  tx.execute(store::sql(
    "UPDATE appointment_jobs SET finished_at=$3,claim=NULL,claim_until=NULL,last_error=NULL WHERE id=$1 AND claim=$2 AND finished_at IS NULL",
    vec![job.id.into(), job.claim.into(), now.into()],
  ))
  .await?;
  Ok(())
}

async fn rearm_job_in_tx(
  tx: &DatabaseTransaction,
  job: &Job,
  revision: i64,
  due: i64,
  error: Option<&str>,
) -> Result<(), Error> {
  let key = if revision > 0 {
    job_key(job.appointment, revision, &job.kind)
  } else {
    job.key.clone()
  };
  tx.execute(store::sql(
    "UPDATE appointment_jobs SET dedupe_key=$3,due_at=$4,finished_at=NULL,claim=NULL,claim_until=NULL,last_error=$5 WHERE id=$1 AND claim=$2 AND finished_at IS NULL",
    vec![job.id.into(), job.claim.into(), key.into(), due.into(), error.into()],
  ))
  .await?;
  Ok(())
}
#[cfg(test)]
#[path = "appointments_tests.rs"]
mod tests;
