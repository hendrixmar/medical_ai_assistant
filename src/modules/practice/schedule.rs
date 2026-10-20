use super::model::{number, text, value, Errors, Fields};
use chrono::{Datelike, Timelike};
use serde::{Deserialize, Serialize};

pub const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Day {
  #[default]
  Unconfigured,
  Closed,
  Open {
    ranges: Vec<TimeRange>,
  },
}

#[derive(Clone, Deserialize, Serialize)]
pub struct TimeRange {
  start: u16,
  end: u16,
}

fn minute(raw: &str, end: bool) -> Option<u16> {
  let bytes = raw.as_bytes();
  if bytes.len() != 5
    || bytes[2] != b':'
    || !bytes
      .iter()
      .enumerate()
      .all(|(i, b)| i == 2 || b.is_ascii_digit())
  {
    return None;
  }
  let hours = raw[..2].parse::<u16>().ok()?;
  let minutes = raw[3..].parse::<u16>().ok()?;
  if hours < 24 && minutes < 60 || end && hours == 24 && minutes == 0 {
    Some(hours * 60 + minutes)
  } else {
    None
  }
}

fn ranges(raw: &str) -> Option<Vec<TimeRange>> {
  let mut ranges = raw
    .split(',')
    .map(|range| {
      let (start, end) = range.trim().split_once('-')?;
      let start = minute(start.trim(), false)?;
      let end = minute(end.trim(), true)?;
      (start < end).then_some(TimeRange { start, end })
    })
    .collect::<Option<Vec<_>>>()?;
  ranges.sort_by_key(|r| r.start);
  if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
    return None;
  }
  Some(ranges)
}

pub struct Schedule {
  pub weekly_hours: [Day; 7],
  pub buffer_before_minutes: i32,
  pub buffer_after_minutes: i32,
  pub minimum_notice_minutes: i32,
  pub grid_minutes: i32,
  pub horizon_days: i32,
  pub cancellation_policy: String,
  pub reminder_2h_enabled: bool,
}

impl Schedule {
  pub fn parse(fields: &Fields) -> Result<Self, Errors> {
    let mut errors = vec![];
    let weekly_hours = std::array::from_fn(|index| {
      let key = DAYS[index];
      let status = format!("{key}_status");
      let hours = format!("{key}_hours");
      let raw = value(fields, &hours);
      match value(fields, &status) {
        "closed" if raw.is_empty() => Day::Closed,
        "closed" => {
          errors.push((
            hours,
            "Si el día está cerrado, deja los horarios vacíos.".into(),
          ));
          Day::Unconfigured
        }
        "open" => match ranges(raw) {
          Some(ranges) => Day::Open { ranges },
          None => {
            errors.push((hours, "Usa rangos HH:MM-HH:MM separados por comas, sin superposiciones. 24:00 solo al final; divide los horarios nocturnos entre dos días.".into()));
            Day::Unconfigured
          }
        },
        _ => {
          errors.push((status, "Indica si este día está abierto o cerrado.".into()));
          Day::Unconfigured
        }
      }
    });
    let reminder_2h_enabled = match value(fields, "reminder_2h_enabled") {
      "" | "false" => false,
      "true" => true,
      _ => {
        errors.push((
          "reminder_2h_enabled".into(),
          "Elige si quieres enviar un recordatorio adicional dos horas antes.".into(),
        ));
        false
      }
    };
    let result = Self {
      weekly_hours,
      buffer_before_minutes: number(fields, "buffer_before_minutes", false, &mut errors),
      buffer_after_minutes: number(fields, "buffer_after_minutes", false, &mut errors),
      minimum_notice_minutes: number(fields, "minimum_notice_minutes", false, &mut errors),
      grid_minutes: number(fields, "grid_minutes", true, &mut errors),
      horizon_days: number(fields, "horizon_days", true, &mut errors),
      cancellation_policy: text(fields, "cancellation_policy", 4000, true, &mut errors),
      reminder_2h_enabled,
    };
    if errors.is_empty() {
      Ok(result)
    } else {
      Err(errors)
    }
  }
}

pub fn fields(days: &[Day; 7], fields: &mut Fields) {
  for (key, day) in DAYS.iter().zip(days) {
    let (status, hours) = match day {
      Day::Unconfigured => ("", String::new()),
      Day::Closed => ("closed", String::new()),
      Day::Open { ranges } => (
        "open",
        ranges
          .iter()
          .map(|r| {
            format!(
              "{:02}:{:02}-{:02}:{:02}",
              r.start / 60,
              r.start % 60,
              r.end / 60,
              r.end % 60
            )
          })
          .collect::<Vec<_>>()
          .join(", "),
      ),
    };
    fields.insert(format!("{key}_status"), status.into());
    fields.insert(format!("{key}_hours"), hours);
  }
}
/// Returns `after_ms` when it falls in configured working hours, otherwise the next
/// open local minute within one weekly cycle. UTC-minute scanning respects DST gaps/folds.
// ponytail: scan at most eight days of UTC minutes; jump between weekly intervals if this ever matters.
pub fn next_working_time(days: &[Day; 7], timezone: chrono_tz::Tz, after_ms: i64) -> Option<i64> {
  let working = |at_ms| {
    let local =
      chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at_ms)?.with_timezone(&timezone);
    let weekday = local.weekday().num_days_from_monday() as usize;
    let minute = (local.hour() * 60 + local.minute()) as u16;
    Some(match &days[weekday] {
      Day::Open { ranges } => ranges
        .iter()
        .any(|range| range.start <= minute && minute < range.end),
      Day::Unconfigured | Day::Closed => false,
    })
  };
  if working(after_ms)? {
    return Some(after_ms);
  }
  for offset in 1..=8 * 24 * 60 {
    let at_ms = after_ms
      .div_euclid(60_000)
      .checked_add(offset)?
      .checked_mul(60_000)?;
    if working(at_ms)? {
      return Some(at_ms);
    }
  }
  None
}
