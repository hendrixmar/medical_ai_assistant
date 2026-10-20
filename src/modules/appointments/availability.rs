use crate::{
  common::config::Config,
  database::Db,
  modules::{
    doctor::Clock,
    google_calendar::{self, client::Client},
    practice,
    whatsapp::inbox::sql,
  },
};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike};
use sea_orm::{ConnectionTrait, DbErr};
use uuid::Uuid;

use super::{
  model::{Appointment, Format},
  store,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CalendarState {
  Checked,
  Unavailable,
  NotConfigured,
  VideoDisabled,
}

#[derive(Clone, Copy)]
pub struct Choice {
  pub starts_at: i64,
  pub ends_at: i64,
  pub receipt_deadline: Option<i64>,
  pub hold_expires_at: Option<i64>,
}

#[derive(Clone)]
pub struct Offers {
  pub calendar: CalendarState,
  pub choices: Vec<Choice>,
  pub(crate) target: Option<Target>,
  pub(crate) practice_snapshot: Option<String>,
  pub(crate) service_snapshot: Option<ServiceSnapshot>,
}

#[derive(Clone, Copy)]
pub struct Request<'a> {
  pub contact: Uuid,
  pub service_id: Uuid,
  pub format: Format,
  pub date: NaiveDate,
  pub period: Option<(u16, u16)>,
  pub exclude_appointment: Option<Uuid>,
  pub duration_override: Option<i32>,
  pub payment_state: Option<&'a str>,
  pub receipt_deadline: Option<i64>,
  pub hold_expires_at: Option<i64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Check {
  Available,
  Conflict,
  Unavailable,
}

#[derive(Clone)]
pub(crate) struct Target {
  pub calendar: String,
  pub generation: i64,
  pub selection_version: i64,
}

#[derive(Clone)]
pub(crate) struct ServiceSnapshot {
  pub id: Uuid,
  pub name: String,
  pub duration_minutes: i32,
  pub price_centavos: i64,
}

pub async fn offer(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
  request: Request<'_>,
) -> Result<Offers, DbErr> {
  let now = clock.now_millis();
  let practice = practice::store::load(db).await?;
  let fields = practice.fields();
  let Some(details) = practice::model::Details::parse(&fields).ok() else {
    return Ok(empty(CalendarState::NotConfigured));
  };
  let Some(schedule) = practice::schedule::Schedule::parse(&fields).ok() else {
    return Ok(empty(CalendarState::NotConfigured));
  };
  let service = practice::store::services(db)
    .await?
    .into_iter()
    .find(|service| service.id == request.service_id);
  let (duration, service_snapshot) = if let Some(duration) = request.duration_override {
    if duration <= 0 {
      return Ok(empty(CalendarState::NotConfigured));
    }
    (duration, None)
  } else {
    let Some(service) = service else {
      return Ok(empty(CalendarState::NotConfigured));
    };
    (
      service.duration_minutes,
      Some(ServiceSnapshot {
        id: service.id,
        name: service.name,
        duration_minutes: service.duration_minutes,
        price_centavos: service.price_centavos,
      }),
    )
  };
  let payments = if request.format == Format::Video && request.exclude_appointment.is_none() {
    match practice::payments::Payments::parse(&fields) {
      Ok(payments) => Some(payments),
      Err(_) => return Ok(empty(CalendarState::VideoDisabled)),
    }
  } else {
    None
  };
  let Some(target) = selection(db, request.format).await? else {
    return Ok(empty(if request.format == Format::Video {
      CalendarState::VideoDisabled
    } else {
      CalendarState::NotConfigured
    }));
  };
  if request.exclude_appointment.is_none()
    && store::open_for_contact(&db.conn, request.contact, now)
      .await?
      .len()
      >= 3
  {
    return Ok(empty(CalendarState::Checked));
  }
  let Some(day_start) = local_midnight(details.timezone, request.date) else {
    return Ok(empty(CalendarState::Checked));
  };
  let Some(next_day) = request.date.succ_opt() else {
    return Ok(empty(CalendarState::Checked));
  };
  let Some(day_end) = local_midnight(details.timezone, next_day) else {
    return Ok(empty(CalendarState::Checked));
  };
  let Some(local_now) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now)
    .map(|time| time.with_timezone(&details.timezone))
  else {
    return Ok(empty(CalendarState::Checked));
  };
  if request.date < local_now.date_naive()
    || request.date > local_now.date_naive() + Duration::days(i64::from(schedule.horizon_days))
  {
    return Ok(empty(CalendarState::Checked));
  }

  let before = i64::from(schedule.buffer_before_minutes).saturating_mul(60_000);
  let after = i64::from(schedule.buffer_after_minutes).saturating_mul(60_000);
  let scan_start = day_start.saturating_sub(before);
  let scan_end = day_end.saturating_add(after);
  let exclude_event = if let Some(id) = request.exclude_appointment {
    let Some(appointment) = store::get(&db.conn, id).await? else {
      return Ok(empty(CalendarState::NotConfigured));
    };
    if appointment.contact_id != request.contact {
      return Ok(empty(CalendarState::NotConfigured));
    }
    if appointment.booking_calendar.as_deref() != Some(target.calendar.as_str()) {
      return Ok(empty(CalendarState::Unavailable));
    }
    appointment.event_id
  } else {
    None
  };
  let busy = google_calendar::appointments::availability(
    db,
    clock,
    cfg,
    client,
    scan_start,
    scan_end,
    exclude_event.as_deref(),
  )
  .await
  .ok();
  let calendar = if busy.is_some() {
    CalendarState::Checked
  } else {
    CalendarState::Unavailable
  };
  if request.format == Format::Video && busy.is_none() {
    return Ok(empty(CalendarState::Unavailable));
  }
  let holds = active_holds(&db.conn, request.exclude_appointment, scan_start, scan_end).await?;
  let ranges = open_ranges(
    &fields,
    request.date.weekday().num_days_from_monday() as usize,
  );
  let grid = u16::try_from(schedule.grid_minutes).unwrap_or(u16::MAX);
  let duration = u16::try_from(duration).unwrap_or(u16::MAX);
  if grid == 0 || duration == 0 {
    return Ok(empty(CalendarState::NotConfigured));
  }
  let mut choices = Vec::with_capacity(5);
  'ranges: for (open, close) in ranges {
    let earliest = open.saturating_add(grid - 1) / grid * grid;
    let Some(latest) = close.checked_sub(duration) else {
      continue;
    };
    let mut minute = earliest;
    while minute <= latest {
      if request
        .period
        .is_none_or(|(from, to)| minute >= from && minute < to)
      {
        if let Some(choice) = choice_at(
          &request,
          details.timezone,
          minute,
          duration,
          now,
          &practice,
          payments.as_ref(),
        ) {
          let blocked_start = choice.starts_at.saturating_sub(before);
          let blocked_end = choice.ends_at.saturating_add(after);
          // ponytail: O(same-day candidate starts × active Holds); one-day scans and the three-open limit keep ordinary practice volume small.
          if holds
            .iter()
            .any(|(start, end)| overlaps(blocked_start, blocked_end, *start, *end))
            || busy.as_ref().is_some_and(|events| {
              events
                .iter()
                .any(|(start, end)| overlaps(blocked_start, blocked_end, *start, *end))
            })
          {
            minute = minute.saturating_add(grid);
            continue;
          }
          choices.push(choice);
          if choices.len() == 5 {
            break 'ranges;
          }
        }
      }
      minute = minute.saturating_add(grid);
    }
  }
  let practice_snapshot = serde_json::to_string(&fields).ok();
  Ok(Offers {
    calendar,
    choices,
    target: Some(target),
    practice_snapshot,
    service_snapshot,
  })
}

pub async fn recheck(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
  appointment: &Appointment,
) -> Result<Check, DbErr> {
  let Some(_) = Format::from_db(&appointment.format) else {
    return Ok(Check::Conflict);
  };
  if appointment.booking_calendar.is_none() {
    return Ok(Check::Unavailable);
  }
  let practice = practice::store::load(db).await?;
  let fields = practice.fields();
  let Some(details) = practice::model::Details::parse(&fields).ok() else {
    return Ok(Check::Conflict);
  };
  let Some(schedule) = practice::schedule::Schedule::parse(&fields).ok() else {
    return Ok(Check::Conflict);
  };
  let now = clock.now_millis();
  let Some(local) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(appointment.starts_at)
    .map(|time| time.with_timezone(&details.timezone))
  else {
    return Ok(Check::Conflict);
  };
  let minute = (local.hour() * 60 + local.minute()) as u16;
  let duration = u16::try_from(appointment.duration_minutes).unwrap_or(u16::MAX);
  let grid = u16::try_from(schedule.grid_minutes).unwrap_or(u16::MAX);
  let today = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now)
    .map(|time| time.with_timezone(&details.timezone).date_naive())
    .unwrap_or(local.date_naive());
  if appointment.ends_at.saturating_sub(appointment.starts_at)
    != i64::from(appointment.duration_minutes).saturating_mul(60_000)
    || !is_open_at(&fields, local.date_naive(), minute, duration)
    || local.second() != 0
    || local.nanosecond() != 0
    || grid == 0
    || !minute.is_multiple_of(grid)
    || appointment.starts_at
      < now.saturating_add(i64::from(schedule.minimum_notice_minutes).saturating_mul(60_000))
    || local.date_naive() < today
    || local.date_naive() > today + Duration::days(i64::from(schedule.horizon_days))
  {
    return Ok(Check::Conflict);
  }
  let before = i64::from(appointment.buffer_before_minutes).saturating_mul(60_000);
  let after = i64::from(appointment.buffer_after_minutes).saturating_mul(60_000);
  let start = appointment.starts_at.saturating_sub(before);
  let end = appointment.ends_at.saturating_add(after);
  if db
    .conn
    .query_one(sql(
      "SELECT id FROM appointment_holds WHERE appointment_id=$1 AND active AND (expires_at IS NULL OR expires_at>$2) LIMIT 1",
      vec![appointment.id.into(), now.into()],
    ))
    .await?
    .is_none()
  {
    return Ok(Check::Conflict);
  }
  if active_holds(&db.conn, Some(appointment.id), start, end)
    .await?
    .iter()
    .any(|(other_start, other_end)| overlaps(start, end, *other_start, *other_end))
  {
    return Ok(Check::Conflict);
  }
  match google_calendar::appointments::availability(
    db,
    clock,
    cfg,
    client,
    start,
    end,
    appointment.event_id.as_deref(),
  )
  .await
  {
    Ok(busy)
      if busy
        .iter()
        .any(|(from, to)| overlaps(start, end, *from, *to)) =>
    {
      Ok(Check::Conflict)
    }
    Ok(_) => Ok(Check::Available),
    Err(_) => Ok(Check::Unavailable),
  }
}

pub async fn video_enabled(db: &Db) -> Result<bool, DbErr> {
  let practice = practice::store::load(db).await?;
  if practice::payments::Payments::parse(&practice.fields()).is_err() {
    return Ok(false);
  }
  Ok(selection(db, Format::Video).await?.is_some())
}

async fn selection(db: &Db, format: Format) -> Result<Option<Target>, DbErr> {
  let Some(row) = db
    .conn
    .query_one(sql(
      "SELECT status,generation,selection_version,booking_calendar,capability_checked_at,permission_checked_at FROM google_connection WHERE id=1",
      vec![],
    ))
    .await?
  else {
    return Ok(None);
  };
  let status: String = row.try_get("", "status")?;
  let calendar: Option<String> = row.try_get("", "booking_calendar")?;
  let generation: i64 = row.try_get("", "generation")?;
  let selection_version: i64 = row.try_get("", "selection_version")?;
  let capability: Option<i64> = row.try_get("", "capability_checked_at")?;
  let permission: Option<i64> = row.try_get("", "permission_checked_at")?;
  let Some(calendar) = calendar else {
    return Ok(None);
  };
  let video_ready = status == "connected" && capability.is_some() && permission.is_some();
  if format == Format::Video && !video_ready {
    return Ok(None);
  }
  Ok(Some(Target {
    calendar,
    generation,
    selection_version,
  }))
}

fn empty(calendar: CalendarState) -> Offers {
  Offers {
    calendar,
    choices: Vec::new(),
    target: None,
    practice_snapshot: None,
    service_snapshot: None,
  }
}

fn choice_at(
  request: &Request<'_>,
  timezone: chrono_tz::Tz,
  minute: u16,
  duration: u16,
  now: i64,
  practice: &practice::model::Practice,
  payments: Option<&practice::payments::Payments>,
) -> Option<Choice> {
  let time = NaiveTime::from_hms_opt(u32::from(minute / 60), u32::from(minute % 60), 0)?;
  let starts_at = timezone
    .from_local_datetime(&NaiveDateTime::new(request.date, time))
    .single()?
    .timestamp_millis();
  let ends_at = starts_at.checked_add(i64::from(duration).checked_mul(60_000)?)?;
  if starts_at
    < now.saturating_add(i64::from(practice.minimum_notice_minutes?).saturating_mul(60_000))
  {
    return None;
  }
  let (receipt_deadline, hold_expires_at) = if request.format == Format::Video {
    if let Some(deadline) = request.receipt_deadline {
      if matches!(request.payment_state, Some("awaiting_receipt" | "rejected"))
        && starts_at
          < deadline
            .saturating_add(i64::from(practice.review_margin_minutes?).saturating_mul(60_000))
      {
        return None;
      }
      let expiry = if matches!(request.payment_state, Some("under_review" | "verified")) {
        None
      } else {
        request.hold_expires_at
      };
      (Some(deadline), expiry)
    } else {
      let payments = payments?;
      let deadline = now
        .saturating_add(i64::from(payments.receipt_window_minutes).saturating_mul(60_000))
        .min(
          starts_at
            .saturating_sub(i64::from(payments.review_margin_minutes).saturating_mul(60_000)),
        );
      if deadline.saturating_sub(now) < 30 * 60_000 {
        return None;
      }
      (
        Some(deadline),
        Some(deadline.saturating_add(i64::from(payments.grace_minutes).saturating_mul(60_000))),
      )
    }
  } else {
    (None, None)
  };
  Some(Choice {
    starts_at,
    ends_at,
    receipt_deadline,
    hold_expires_at,
  })
}

async fn active_holds<C: ConnectionTrait>(
  conn: &C,
  exclude: Option<Uuid>,
  start: i64,
  end: i64,
) -> Result<Vec<(i64, i64)>, DbErr> {
  conn
    .query_all(sql(
      "SELECT starts_at,ends_at FROM appointment_holds WHERE active AND starts_at<$2 AND ends_at>$1 AND ($3::uuid IS NULL OR appointment_id<>$3)",
      vec![start.into(), end.into(), exclude.into()],
    ))
    .await?
    .iter()
    .map(|row| Ok((row.try_get("", "starts_at")?, row.try_get("", "ends_at")?)))
    .collect()
}

fn open_ranges(fields: &practice::model::Fields, weekday: usize) -> Vec<(u16, u16)> {
  let day = practice::schedule::DAYS[weekday];
  fields
    .get(&format!("{day}_hours"))
    .into_iter()
    .flat_map(|hours| hours.split(','))
    .filter_map(parse_range)
    .collect()
}

fn parse_range(raw: &str) -> Option<(u16, u16)> {
  let (start, end) = raw.trim().split_once('-')?;
  let start = parse_minute(start, false)?;
  let end = parse_minute(end, true)?;
  (end > start).then_some((start, end))
}

fn parse_minute(raw: &str, end: bool) -> Option<u16> {
  let (hours, minutes) = raw.trim().split_once(':')?;
  if hours.len() != 2 || minutes.len() != 2 {
    return None;
  }
  let hours = hours.parse::<u16>().ok()?;
  let minutes = minutes.parse::<u16>().ok()?;
  if minutes >= 60 || hours > 24 || hours == 24 && (!end || minutes != 0) {
    return None;
  }
  Some(hours * 60 + minutes)
}

fn local_midnight(timezone: chrono_tz::Tz, date: NaiveDate) -> Option<i64> {
  timezone
    .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
    .single()
    .map(|time| time.timestamp_millis())
}

fn is_open_at(
  fields: &practice::model::Fields,
  date: NaiveDate,
  start: u16,
  duration: u16,
) -> bool {
  let end = start.saturating_add(duration);
  open_ranges(fields, date.weekday().num_days_from_monday() as usize)
    .into_iter()
    .any(|(open, close)| start >= open && end <= close)
}

fn overlaps(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> bool {
  a_start < b_end && b_start < a_end
}
