use crate::modules::whatsapp::inbox::sql;
use chrono::{Datelike, NaiveDate, NaiveTime, TimeZone};
use sea_orm::ConnectionTrait;

#[derive(Clone, Copy)]
enum Request {
  Today,
  Tomorrow,
  Week,
  Date(NaiveDate),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Question<'a> {
  Text(&'a str),
  Date(NaiveDate),
}

fn normalize(text: &str) -> Option<String> {
  if text.len() > 640 || text.chars().count() > 160 || text.chars().any(char::is_control) {
    return None;
  }
  Some(
    text
      .trim()
      .to_lowercase()
      .split_whitespace()
      .collect::<Vec<_>>()
      .join(" "),
  )
}

fn without_question_marks(text: &str) -> &str {
  text.trim().trim_matches(['¿', '?', '¡', '!']).trim()
}

fn parse_request(text: &str) -> Option<Request> {
  let normalized = normalize(text)?;
  let normalized = without_question_marks(&normalized);
  let query = normalized
    .strip_prefix("qué tengo ")
    .or_else(|| normalized.strip_prefix("que tengo "))
    .unwrap_or(normalized);
  match query {
    "agenda" | "citas" | "hoy" | "agenda hoy" | "agenda de hoy" | "citas hoy" => {
      Some(Request::Today)
    }
    "mañana" | "manana" | "agenda mañana" | "agenda manana" | "agenda de mañana"
    | "agenda de manana" | "citas mañana" | "citas manana" => Some(Request::Tomorrow),
    "semana"
    | "esta semana"
    | "agenda semana"
    | "agenda esta semana"
    | "agenda de esta semana"
    | "citas esta semana" => Some(Request::Week),
    _ => {
      let date = query.strip_prefix("agenda ").unwrap_or(query);
      let date = date.strip_prefix("el ").unwrap_or(date);
      ["%Y-%m-%d", "%d/%m/%Y", "%d-%m-%Y"]
        .iter()
        .find_map(|pattern| NaiveDate::parse_from_str(date, pattern).ok())
        .map(Request::Date)
    }
  }
}

/// Requests interpretation only for a read-only agenda date question the deterministic set misses.
pub(crate) fn needs_parser(text: &str) -> bool {
  if parse_request(text).is_some() || has_mutation_word(text) {
    return false;
  }
  let Some(normalized) = normalize(text) else {
    return false;
  };
  let query = without_question_marks(&normalized);
  let Some(date_question) = query
    .strip_prefix("qué tengo el ")
    .or_else(|| query.strip_prefix("que tengo el "))
  else {
    return false;
  };
  !has_mutation_word(date_question) && date_question.split_whitespace().any(is_date_cue)
}

fn has_mutation_word(text: &str) -> bool {
  text.split_whitespace().any(|word| {
    matches!(
      word.trim_matches(|ch: char| !ch.is_alphanumeric()),
      "cancela"
        | "cancele"
        | "cancelar"
        | "cancelación"
        | "cancelacion"
        | "cambia"
        | "cambie"
        | "cambiar"
        | "reprograma"
        | "reprograme"
        | "reprogramar"
        | "mueve"
        | "mover"
        | "modifica"
        | "modificar"
        | "confirma"
        | "confirmar"
        | "paga"
        | "pagar"
        | "verifica"
        | "verificar"
        | "agendar"
        | "reservar"
        | "reserva"
        | "elimina"
        | "eliminar"
        | "borra"
        | "borrar"
        | "crea"
        | "crear"
    )
  })
}

fn is_date_cue(word: &str) -> bool {
  let word = word.trim_matches(|ch: char| !ch.is_alphanumeric() && !matches!(ch, '-' | '/' | ':'));
  matches!(
    word,
    "hoy"
      | "mañana"
      | "manana"
      | "lunes"
      | "martes"
      | "miercoles"
      | "miércoles"
      | "jueves"
      | "viernes"
      | "sabado"
      | "sábado"
      | "domingo"
      | "enero"
      | "febrero"
      | "marzo"
      | "abril"
      | "mayo"
      | "junio"
      | "julio"
      | "agosto"
      | "septiembre"
      | "octubre"
      | "noviembre"
      | "diciembre"
  )
}

fn local_midnight(date: NaiveDate, timezone: chrono_tz::Tz) -> Option<i64> {
  let midnight = date.and_time(NaiveTime::MIN);
  for minute in 0..24 * 60 {
    let local = midnight.checked_add_signed(chrono::Duration::minutes(minute))?;
    match timezone.from_local_datetime(&local) {
      chrono::LocalResult::Single(value) if value.date_naive() == date => {
        return Some(value.timestamp_millis());
      }
      chrono::LocalResult::Ambiguous(first, second)
        if first.date_naive() == date && second.date_naive() == date =>
      {
        return Some(first.timestamp_millis().min(second.timestamp_millis()));
      }
      chrono::LocalResult::None
      | chrono::LocalResult::Single(_)
      | chrono::LocalResult::Ambiguous(_, _) => {}
    }
  }
  None
}

fn weekday(date: NaiveDate) -> &'static str {
  match date.weekday() {
    chrono::Weekday::Mon => "lunes",
    chrono::Weekday::Tue => "martes",
    chrono::Weekday::Wed => "miércoles",
    chrono::Weekday::Thu => "jueves",
    chrono::Weekday::Fri => "viernes",
    chrono::Weekday::Sat => "sábado",
    chrono::Weekday::Sun => "domingo",
  }
}

fn status(value: &str) -> &'static str {
  match value {
    "requested" => "solicitada, sin confirmación",
    "awaiting_payment" => "pendiente de pago",
    "confirmed" => "confirmada",
    "cancelled" => "cancelada",
    "expired" => "vencida",
    _ => "estado registrado desconocido",
  }
}

fn payment(value: &str) -> &'static str {
  match value {
    "not_required" => "no requiere pago",
    "awaiting_receipt" => "pendiente de comprobante",
    "under_review" => "comprobante en revisión",
    "verified" => "pago verificado por el Doctor",
    "rejected" => "comprobante rechazado",
    _ => "estado de pago desconocido",
  }
}

fn calendar(value: &str) -> &'static str {
  match value {
    "not_yet" => "sin sincronización",
    "pending" => "sincronización pendiente",
    "synced" => "sincronizado",
    "failed" => "sincronización fallida",
    _ => "estado de calendario desconocido",
  }
}

/// Answers only a small, read-only set of Doctor agenda queries. Recognition is performed by
/// the caller after verified Doctor Number matching; this function never changes records.
pub(crate) async fn answer<C: ConnectionTrait>(
  conn: &C,
  now: i64,
  question: Question<'_>,
) -> Result<Option<String>, sea_orm::DbErr> {
  let request = match question {
    Question::Text(text) => parse_request(text),
    Question::Date(date) => Some(Request::Date(date)),
  };
  let Some(request) = request else {
    return Ok(None);
  };
  let row = conn
    .query_one(sql("SELECT timezone FROM practice WHERE id=1", vec![]))
    .await?
    .ok_or_else(|| sea_orm::DbErr::Custom("Practice is unavailable".into()))?;
  let timezone = row
    .try_get::<Option<String>>("", "timezone")?
    .and_then(|value| value.parse::<chrono_tz::Tz>().ok());
  let Some(timezone) = timezone else {
    return Ok(Some(
      "No puedo mostrar la agenda con seguridad: configura explícitamente la zona horaria del consultorio.".into(),
    ));
  };
  let Some(utc_now) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now) else {
    return Ok(Some("No pude calcular la fecha local de la agenda.".into()));
  };
  let today = utc_now.with_timezone(&timezone).date_naive();
  let (date, end_date) = match request {
    Request::Today => (today, today.checked_add_signed(chrono::Duration::days(1))),
    Request::Tomorrow => {
      let date = today.checked_add_signed(chrono::Duration::days(1));
      let end = date.and_then(|date| date.checked_add_signed(chrono::Duration::days(1)));
      let Some(date) = date else {
        return Ok(Some("No pude calcular la fecha local de la agenda.".into()));
      };
      (date, end)
    }
    Request::Week => {
      let date = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
      (date, date.checked_add_signed(chrono::Duration::days(7)))
    }
    Request::Date(date) => (date, date.checked_add_signed(chrono::Duration::days(1))),
  };
  let Some(end_date) = end_date else {
    return Ok(Some(
      "No pude calcular el intervalo local de la agenda.".into(),
    ));
  };
  let Some(start) = local_midnight(date, timezone) else {
    return Ok(Some(
      "No pude calcular el inicio local de la agenda.".into(),
    ));
  };
  let Some(end) = local_midnight(end_date, timezone) else {
    return Ok(Some("No pude calcular el fin local de la agenda.".into()));
  };
  let rows = conn
    .query_all(sql(
      "SELECT patient_name,service_name,format,starts_at,status,payment,calendar_sync FROM appointments WHERE starts_at >= $1 AND starts_at < $2 ORDER BY starts_at,id LIMIT 11",
      vec![start.into(), end.into()],
    ))
    .await?;
  let mut answer = format!(
    "Agenda para {} {:02}/{:02}/{} ({})",
    weekday(date),
    date.day(),
    date.month(),
    date.year(),
    timezone.name()
  );
  if rows.is_empty() {
    answer.push_str(". No hay citas registradas para ese intervalo.");
    return Ok(Some(answer));
  }
  answer.push(':');
  for row in rows.iter().take(10) {
    let starts_at: i64 = row.try_get("", "starts_at")?;
    let Some(utc) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(starts_at) else {
      continue;
    };
    let patient: String = row.try_get("", "patient_name")?;
    let service: String = row.try_get("", "service_name")?;
    let format: String = row.try_get("", "format")?;
    let status_value: String = row.try_get("", "status")?;
    let payment_value: String = row.try_get("", "payment")?;
    let calendar_value: String = row.try_get("", "calendar_sync")?;
    let format = match format.as_str() {
      "in_person" => "presencial",
      "video" => "video",
      _ => "formato desconocido",
    };
    let clean = |value: String| {
      value
        .chars()
        .map(|character| {
          if character.is_control() {
            ' '
          } else {
            character
          }
        })
        .collect::<String>()
    };
    answer.push_str(&format!(
      "\n- {} · {} · {} · formato: {} · estado: {}; pago: {}; calendario: {}",
      utc.with_timezone(&timezone).format("%H:%M"),
      clean(patient),
      clean(service),
      format,
      status(&status_value),
      payment(&payment_value),
      calendar(&calendar_value)
    ));
  }
  if rows.len() > 10 {
    answer.push_str("\nHay más citas; consulta una fecha específica.");
  }
  Ok(Some(answer))
}
#[cfg(test)]
#[path = "agenda_tests.rs"]
mod tests;
