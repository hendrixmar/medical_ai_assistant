use crate::common::config::Config;
use chrono::{Datelike, NaiveDate, Weekday};
use serde::Deserialize;
use std::{sync::LazyLock, time::Duration};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const SCHEMA_VERSION: &str =
  "schedule_in_person|agenda/date_phrase:string|null/time_phrase:string|null/uncertain:bool:v2";
const RESPONSE_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preference {
  pub date: NaiveDate,
  pub minute: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
  intent: String,
  date_phrase: RequiredNullableString,
  time_phrase: RequiredNullableString,
  uncertain: bool,
}

#[derive(Deserialize)]
#[serde(transparent)]
struct RequiredNullableString(Option<String>);

#[derive(Deserialize)]
struct AnthropicResponse {
  content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
  #[serde(rename = "type")]
  kind: String,
  text: Option<String>,
}

static CLIENT: LazyLock<Option<reqwest::Client>> = LazyLock::new(|| {
  reqwest::Client::builder()
    .no_proxy()
    .redirect(reqwest::redirect::Policy::none())
    .retry(reqwest::retry::never())
    .connect_timeout(Duration::from_secs(2))
    .timeout(Duration::from_secs(8))
    .build()
    .ok()
});

/// Interprets only a sanitized in-person date/time preference. The model never chooses a service,
/// patient, format, availability, or state transition.
pub async fn interpret(cfg: &Config, text: &str, today: NaiveDate) -> Option<Preference> {
  let key = cfg.claude_api_key.as_deref()?;
  let model = cfg.claude_model.as_deref()?;
  let safe = scheduling_text(text)?;
  interpret_at(
    key,
    model,
    &safe,
    today,
    "schedule_in_person",
    true,
    provider_endpoint(cfg),
  )
  .await
}

/// Uses the same closed schema when the Contact is already in the date-intake step.
pub async fn interpret_date(cfg: &Config, text: &str, today: NaiveDate) -> Option<Preference> {
  let key = cfg.claude_api_key.as_deref()?;
  let model = cfg.claude_model.as_deref()?;
  let safe = safe_text(text)?;
  interpret_at(
    key,
    model,
    &safe,
    today,
    "schedule_in_person",
    true,
    provider_endpoint(cfg),
  )
  .await
}

/// Interprets only a Doctor's agenda date. Model-authored facts and times are never returned.
pub async fn interpret_agenda(cfg: &Config, text: &str, today: NaiveDate) -> Option<Preference> {
  let key = cfg.claude_api_key.as_deref()?;
  let model = cfg.claude_model.as_deref()?;
  let safe = safe_text(text)?;
  interpret_at(
    key,
    model,
    &safe,
    today,
    "agenda",
    false,
    provider_endpoint(cfg),
  )
  .await
}

pub(crate) fn parse_simple(text: &str, today: NaiveDate) -> Option<Preference> {
  let safe = safe_text(text)?;
  let date = parse_date(&safe, today)?;
  let minute = parse_time_in_text(&safe);
  Some(Preference { date, minute })
}

#[cfg(test)]
fn provider_endpoint(cfg: &Config) -> &str {
  cfg.claude_test_endpoint.as_deref().unwrap_or(ENDPOINT)
}

#[cfg(not(test))]
fn provider_endpoint(_: &Config) -> &str {
  ENDPOINT
}

async fn interpret_at(
  key: &str,
  model: &str,
  safe_text: &str,
  today: NaiveDate,
  requested_intent: &str,
  allow_time: bool,
  endpoint: &str,
) -> Option<Preference> {
  let client = CLIENT.as_ref()?;
  let mut api_key = reqwest::header::HeaderValue::try_from(key).ok()?;
  api_key.set_sensitive(true);
  let system = format!(
    "Parse only into the requested intent. Return exactly one JSON object with exactly these required keys: intent, date_phrase, time_phrase, uncertain. intent must be exactly {requested_intent}. date_phrase and time_phrase must each be an exact substring copied from the supplied sanitized text, or null when absent. Set uncertain true for ambiguity or multiple alternatives. Never infer a missing date, year, or time. Never provide patient, service, format, availability, or other facts. Return no other text."
  );
  let response = client
    .post(endpoint)
    .header("x-api-key", api_key)
    .header("anthropic-version", ANTHROPIC_VERSION)
    .json(&serde_json::json!({
      "model": model,
      "max_tokens": 128,
      "system": system,
      "messages": [{"role": "user", "content": safe_text}]
    }))
    .send()
    .await
    .ok()?;
  if !response.status().is_success()
    || response
      .content_length()
      .is_some_and(|length| length > RESPONSE_LIMIT as u64)
  {
    return None;
  }
  let mut response = response;
  let mut body = Vec::with_capacity(2048);
  while let Some(chunk) = response.chunk().await.ok()? {
    if body.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
      return None;
    }
    body.extend_from_slice(&chunk);
  }
  let response: AnthropicResponse = serde_json::from_slice(&body).ok()?;
  if response.content.len() != 1 || response.content[0].kind != "text" {
    return None;
  }
  let intent: Intent = serde_json::from_str(response.content[0].text.as_deref()?).ok()?;
  if intent.intent != requested_intent || intent.uncertain {
    return None;
  }
  let date_phrase = intent.date_phrase.0.as_deref()?;
  if date_phrase.is_empty() || date_phrase.len() > 100 || !safe_text.contains(date_phrase) {
    return None;
  }
  let date = parse_date(date_phrase, today)?;
  if date_phrase != safe_text
    && parse_date(safe_text, today).is_some_and(|context_date| context_date != date)
  {
    return None;
  }
  let minute = match intent.time_phrase.0.as_deref() {
    Some(phrase)
      if allow_time && !phrase.is_empty() && phrase.len() <= 40 && safe_text.contains(phrase) =>
    {
      Some(parse_time(phrase)?)
    }
    Some(_) => return None,
    None => None,
  };
  tracing::info!(
    model,
    schema = SCHEMA_VERSION,
    "Claude closed intent parsed"
  );
  Some(Preference { date, minute })
}

/// Removes names, service labels, free-form clinical context, and all non-date/time digits before
/// any text is sent to Claude. Unsupported or ambiguous content is handled by the menu flow.
fn safe_text(text: &str) -> Option<String> {
  if text.len() > 640
    || text.chars().count() > 160
    || text.chars().any(char::is_control)
    || has_ambiguous_date_alternatives(text)
  {
    return None;
  }
  let tokens: Vec<_> = text
    .to_lowercase()
    .split_whitespace()
    .map(str::to_owned)
    .collect();
  let mut safe = Vec::with_capacity(tokens.len());
  for (index, token) in tokens.iter().enumerate() {
    let trimmed =
      token.trim_matches(|ch: char| !ch.is_alphanumeric() && !matches!(ch, ':' | '-' | '/'));
    if trimmed.is_empty()
      || (trimmed.chars().any(|ch| ch.is_ascii_digit())
        && has_number_identifier_context(&tokens, index))
    {
      continue;
    }
    if is_scheduling_word(trimmed) || numeric_date_or_time(trimmed) {
      safe.push(trimmed.to_owned());
      continue;
    }
    if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
      let is_day = trimmed
        .parse::<u8>()
        .is_ok_and(|day| (1..=31).contains(&day))
        && tokens
          .get(index + 1)
          .is_some_and(|next| next.trim_matches(|ch: char| !ch.is_alphanumeric()) == "de")
        && tokens
          .get(index + 2)
          .is_some_and(|next| is_month(next.trim_matches(|ch: char| !ch.is_alphanumeric())));
      let is_hour = trimmed
        .parse::<u8>()
        .is_ok_and(|hour| (1..=12).contains(&hour))
        && tokens.get(index + 1).is_some_and(|next| {
          matches!(
            next.trim_matches(|ch: char| !ch.is_alphanumeric()),
            "am" | "pm" | "manana" | "mañana" | "tarde" | "noche"
          )
        });
      let is_year = trimmed
        .parse::<i32>()
        .is_ok_and(|year| (2000..=2200).contains(&year))
        && index >= 2
        && tokens[index - 1].trim_matches(|ch: char| !ch.is_alphanumeric()) == "de"
        && is_month(tokens[index - 2].trim_matches(|ch: char| !ch.is_alphanumeric()));
      if is_day || is_hour || is_year {
        safe.push(trimmed.to_owned());
      }
    }
  }
  let safe = safe.join(" ");
  if safe.is_empty() || !safe.split_whitespace().any(is_date_token) {
    return None;
  }
  Some(safe)
}

fn has_number_identifier_context(tokens: &[String], index: usize) -> bool {
  let start = index.saturating_sub(4);
  // Look ahead by one token only so later labels cannot suppress a valid time.
  let end = index.saturating_add(2).min(tokens.len());
  tokens[start..end].iter().any(|token| {
    matches!(
      token.trim_matches(|ch: char| !ch.is_alphanumeric()),
      "cuenta"
        | "clabe"
        | "opcion"
        | "opción"
        | "menu"
        | "menú"
        | "option"
        | "choice"
        | "codigo"
        | "código"
        | "desafio"
        | "desafío"
        | "challenge"
        | "clave"
        | "pin"
        | "contraseña"
        | "token"
        | "referencia"
        | "folio"
        | "numero"
        | "número"
        | "telefono"
        | "teléfono"
        | "celular"
    )
  })
}

fn has_ambiguous_date_alternatives(text: &str) -> bool {
  let lower = text.to_lowercase();
  let mut raw_words = Vec::new();
  let words: Vec<_> = lower
    .split_whitespace()
    .filter_map(|raw| {
      let word =
        raw.trim_matches(|ch: char| !ch.is_alphanumeric() && !matches!(ch, ':' | '-' | '/'));
      if word.is_empty() {
        None
      } else {
        raw_words.push(raw);
        Some(word)
      }
    })
    .collect();
  let weekdays = words
    .iter()
    .filter(|word| parse_weekday(word).is_some())
    .count();
  let months = words.iter().filter(|word| is_month(word)).count();
  let numeric_dates = words.iter().filter(|word| is_numeric_date(word)).count();
  let relative_dates = words
    .iter()
    .enumerate()
    .filter(|(index, word)| {
      **word == "hoy"
        || (matches!(**word, "mañana" | "manana")
          && !(*index >= 2 && words[*index - 1] == "la" && words[*index - 2] == "de"))
    })
    .count();
  if relative_dates > 1
    || relative_dates > 0 && (weekdays > 0 || months > 0 || numeric_dates > 0)
    || words.iter().enumerate().any(|(index, word)| {
      matches!(*word, "ayer" | "anteayer")
        || (*word == "pasado"
          && !words
            .get(index + 1)
            .is_some_and(|next| matches!(*next, "mañana" | "manana")))
    })
  {
    return true;
  }
  if weekdays > 1 || months > 1 || numeric_dates > 1 {
    return true;
  }
  let has_date = words.iter().any(|word| is_date_token(word));
  if has_date && words.iter().any(|word| matches!(*word, "o" | "u")) {
    return true;
  }
  if months > 0 {
    for month_index in 0..words.len() {
      if !is_month(words[month_index]) {
        continue;
      }
      let day_index = if month_index >= 2 && words[month_index - 1] == "de" {
        month_index - 2
      } else if month_index > 0 {
        month_index - 1
      } else {
        month_index + 1
      };
      if let Some(previous_day) = (0..day_index)
        .rev()
        .find(|index| is_day_number(words[*index]))
        .filter(|index| raw_words[*index].ends_with(','))
      {
        if words[previous_day + 1..day_index]
          .iter()
          .all(|word| matches!(*word, "el" | "de"))
        {
          return true;
        }
      }
    }
  }
  months > 0
    && words.iter().enumerate().any(|(index, word)| {
      *word == "y"
        && index > 0
        && index + 1 < words.len()
        && is_day_number(words[index - 1])
        && is_day_number(words[index + 1])
    })
}

fn is_day_number(value: &str) -> bool {
  value.parse::<u8>().is_ok_and(|day| (1..=31).contains(&day))
}

fn scheduling_text(text: &str) -> Option<String> {
  let safe = safe_text(text)?;
  safe
    .split_whitespace()
    .any(|word| {
      matches!(
        word,
        "cita" | "consulta" | "agendar" | "reservar" | "agenda"
      )
    })
    .then_some(safe)
}

fn is_scheduling_word(word: &str) -> bool {
  matches!(
    word,
    "quiero"
      | "quisiera"
      | "me"
      | "gustaria"
      | "gustaría"
      | "agendar"
      | "agenda"
      | "reservar"
      | "cita"
      | "consulta"
      | "para"
      | "el"
      | "la"
      | "de"
      | "del"
      | "a"
      | "las"
      | "hora"
      | "favor"
      | "por"
      | "proximo"
      | "próximo"
      | "siguiente"
      | "hoy"
      | "manana"
      | "mañana"
      | "pasado"
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
      | "am"
      | "pm"
      | "tarde"
      | "noche"
      | "mediodia"
      | "mediodía"
      | "medianoche"
  )
}
fn parse_time_in_text(text: &str) -> Option<u16> {
  let words: Vec<_> = text.split_whitespace().collect();
  for (index, word) in words.iter().enumerate() {
    if word.contains(':') {
      if let Some(time) = parse_time(word) {
        return Some(time);
      }
    }
    if matches!(*word, "mediodía" | "mediodia" | "medianoche") {
      return parse_time(word);
    }
    if words.get(index + 1).is_some_and(|suffix| {
      matches!(
        *suffix,
        "am" | "pm" | "manana" | "mañana" | "tarde" | "noche"
      )
    }) {
      if let Some(time) = parse_time(&format!("{word} {}", words[index + 1])) {
        return Some(time);
      }
    }
  }
  None
}

fn numeric_date_or_time(value: &str) -> bool {
  if let Some((hour, minute)) = value.split_once(':') {
    return hour.parse::<u8>().is_ok_and(|hour| hour < 24)
      && minute.parse::<u8>().is_ok_and(|minute| minute < 60);
  }
  let pieces: Vec<_> = value.split(['-', '/']).collect();
  let numbers: Option<Vec<u16>> = pieces
    .iter()
    .map(|piece| piece.parse::<u16>().ok())
    .collect();
  match numbers.as_deref() {
    Some([day, month]) => (1..=31).contains(day) && (1..=12).contains(month),
    Some([year, month, day]) if (2000..=2200).contains(year) => {
      (1..=12).contains(month) && (1..=31).contains(day)
    }
    Some([day, month, year]) => {
      (1..=31).contains(day) && (1..=12).contains(month) && (2000..=2200).contains(year)
    }
    _ => false,
  }
}

fn is_numeric_date(value: &str) -> bool {
  !value.contains(':') && numeric_date_or_time(value)
}

fn is_date_token(value: &str) -> bool {
  is_numeric_date(value)
    || matches!(
      value,
      "hoy"
        | "manana"
        | "mañana"
        | "lunes"
        | "martes"
        | "miercoles"
        | "miércoles"
        | "jueves"
        | "viernes"
        | "sabado"
        | "sábado"
        | "domingo"
    )
    || is_month(value)
}

fn is_month(value: &str) -> bool {
  matches!(
    value,
    "enero"
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

fn parse_date(phrase: &str, today: NaiveDate) -> Option<NaiveDate> {
  let normalized = phrase
    .to_lowercase()
    .trim_matches(|ch: char| !ch.is_alphanumeric() && !matches!(ch, '-' | '/'))
    .to_owned();
  for pattern in ["%Y-%m-%d", "%d/%m/%Y", "%d-%m-%Y"] {
    if let Ok(date) = NaiveDate::parse_from_str(&normalized, pattern) {
      return Some(date);
    }
  }
  match normalized.as_str() {
    "hoy" => return Some(today),
    "mañana" | "manana" => return today.succ_opt(),
    _ => {}
  }
  let words: Vec<_> = normalized
    .split_whitespace()
    .map(|word| word.trim_matches(|ch: char| !ch.is_alphanumeric()))
    .filter(|word| !word.is_empty())
    .collect();
  if words
    .windows(2)
    .any(|pair| pair[0] == "pasado" && matches!(pair[1], "mañana" | "manana"))
  {
    return today.checked_add_signed(chrono::Duration::days(2));
  }
  if words.contains(&"hoy") {
    return Some(today);
  }
  if words.iter().enumerate().any(|(index, word)| {
    matches!(*word, "mañana" | "manana")
      && !(index >= 2 && words[index - 1] == "la" && words[index - 2] == "de")
  }) {
    return today.succ_opt();
  }
  let weekday = words.iter().find_map(|word| parse_weekday(word));
  if let Some(month_index) = words.iter().position(|word| is_month(word)) {
    let day_index = if month_index >= 2 && words[month_index - 1] == "de" {
      month_index - 2
    } else if month_index > 0 {
      month_index - 1
    } else {
      month_index + 1
    };
    let day = words.get(day_index)?.parse::<u32>().ok()?;
    let month = month_number(words[month_index])?;
    let year = words
      .iter()
      .skip(month_index + 1)
      .find_map(|word| {
        word
          .parse::<i32>()
          .ok()
          .filter(|year| (2000..=2200).contains(year))
      })
      .unwrap_or(today.year());
    let mut date = NaiveDate::from_ymd_opt(year, month, day)?;
    if year == today.year() && date < today {
      date = NaiveDate::from_ymd_opt(year.checked_add(1)?, month, day)?;
    }
    if weekday.is_some_and(|weekday| weekday != date.weekday()) {
      return None;
    }
    return Some(date);
  }
  if let Some(weekday) = weekday {
    let delta = (i64::from(weekday.num_days_from_monday())
      - i64::from(today.weekday().num_days_from_monday()))
    .rem_euclid(7);
    let days = if delta == 0 { 7 } else { delta };
    return today.checked_add_signed(chrono::Duration::days(days));
  }
  None
}

fn parse_weekday(value: &str) -> Option<Weekday> {
  Some(match value {
    "lunes" => Weekday::Mon,
    "martes" => Weekday::Tue,
    "miercoles" | "miércoles" => Weekday::Wed,
    "jueves" => Weekday::Thu,
    "viernes" => Weekday::Fri,
    "sabado" | "sábado" => Weekday::Sat,
    "domingo" => Weekday::Sun,
    _ => return None,
  })
}

fn month_number(value: &str) -> Option<u32> {
  Some(match value {
    "enero" => 1,
    "febrero" => 2,
    "marzo" => 3,
    "abril" => 4,
    "mayo" => 5,
    "junio" => 6,
    "julio" => 7,
    "agosto" => 8,
    "septiembre" => 9,
    "octubre" => 10,
    "noviembre" => 11,
    "diciembre" => 12,
    _ => return None,
  })
}

fn parse_time(phrase: &str) -> Option<u16> {
  let normalized = phrase.to_lowercase().replace("a las ", "");
  let normalized = normalized.trim();
  if let Some((hour, minute)) = normalized.split_once(':') {
    if minute.len() != 2 || !minute.bytes().all(|byte| byte.is_ascii_digit()) {
      return None;
    }
    let hour = hour.parse::<u16>().ok()?;
    let minute = minute.parse::<u16>().ok()?;
    return (hour < 24 && minute < 60).then_some(hour * 60 + minute);
  }
  if normalized == "mediodía" || normalized == "mediodia" {
    return Some(12 * 60);
  }
  if normalized == "medianoche" {
    return Some(0);
  }
  let words: Vec<_> = normalized
    .split_whitespace()
    .map(|word| word.trim_matches(|ch: char| !ch.is_alphanumeric()))
    .filter(|word| !word.is_empty())
    .collect();
  let hour = words.iter().find_map(|word| word.parse::<u16>().ok())?;
  if hour > 12 || hour == 0 {
    return None;
  }
  let suffix = words.iter().find(|word| {
    matches!(
      **word,
      "am" | "pm" | "manana" | "mañana" | "tarde" | "noche"
    )
  })?;
  let is_pm = matches!(*suffix, "pm" | "tarde" | "noche");
  let hour = match (hour, is_pm) {
    (12, false) => 0,
    (12, true) => 12,
    (hour, true) => hour + 12,
    (hour, false) => hour,
  };
  Some(hour * 60)
}

#[cfg(test)]
mod tests {
  use super::{interpret, interpret_agenda, Preference};
  use crate::common::config::{Config, Configuration, Environment};
  use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, Request, Response, StatusCode},
    Router,
  };
  use chrono::NaiveDate;
  use serde_json::Value;
  use std::{collections::VecDeque, net::SocketAddr, sync::Arc};
  use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};

  struct RecordedRequest {
    api_key: Option<String>,
    body: Vec<u8>,
  }

  struct Reply {
    status: StatusCode,
    body: String,
    location: Option<String>,
  }

  struct Provider {
    calls: Mutex<Vec<RecordedRequest>>,
    replies: Mutex<VecDeque<Reply>>,
  }

  async fn provider(
    State(provider): State<Arc<Provider>>,
    request: Request<Body>,
  ) -> Response<Body> {
    let api_key = request
      .headers()
      .get("x-api-key")
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned);
    let body = to_bytes(request.into_body(), 32 * 1024)
      .await
      .unwrap()
      .to_vec();
    provider
      .calls
      .lock()
      .await
      .push(RecordedRequest { api_key, body });
    let reply = provider.replies.lock().await.pop_front().unwrap_or(Reply {
      status: StatusCode::INTERNAL_SERVER_ERROR,
      body: String::new(),
      location: None,
    });
    let mut response = Response::builder().status(reply.status);
    if let Some(location) = reply.location {
      response = response.header(header::LOCATION, location);
    }
    response.body(Body::from(reply.body)).unwrap()
  }

  async fn start_provider(replies: Vec<Reply>) -> (String, Arc<Provider>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let state = Arc::new(Provider {
      calls: Mutex::new(Vec::new()),
      replies: Mutex::new(replies.into()),
    });
    let app = Router::new().fallback(provider).with_state(state.clone());
    let task = tokio::spawn(async move {
      axum::serve(listener, app).await.unwrap();
    });
    (endpoint, state, task)
  }

  fn reply(output: &str) -> Reply {
    Reply {
      status: StatusCode::OK,
      body: serde_json::json!({
        "content": [{"type": "text", "text": output}]
      })
      .to_string(),
      location: None,
    }
  }

  fn configuration(endpoint: String) -> Config {
    Arc::new(Configuration {
      env: Environment::Development,
      listen_address: SocketAddr::from(([127, 0, 0, 1], 0)),
      app_port: 0,
      swagger_endpoint: "/docs".into(),
      db_dsn: String::new(),
      db_pool_max_size: 1,
      db_timeout: 1,
      db_run_migrations: false,
      bcrypt_cost: 4,
      public_origin: None,
      doctor_totp_key: None,
      waha_outbound_enabled: false,
      waha_contact_replies_per_hour: 20,
      claude_api_key: Some("synthetic-claude-key".into()),
      claude_model: Some("claude-test-model".into()),
      claude_test_endpoint: Some(endpoint),
      waha_takeover_enabled: false,
      waha: None,
      google: None,
    })
  }

  #[tokio::test]
  async fn closed_output_is_rejected_and_sensitive_context_is_absent_on_the_wire() {
    let (endpoint, provider, task) = start_provider(vec![
      reply(
        r#"{"intent":"unknown","date_phrase":"martes 20 de octubre","time_phrase":"10 am","uncertain":false}"#,
      ),
      reply(
        r#"{"intent":"schedule_in_person","date_phrase":"martes 20 de octubre","time_phrase":"10 am","uncertain":false,"patient_name":"Ana Pérez"}"#,
      ),
      reply(
        r#"{"intent":"schedule_in_person","date_phrase":"viernes 23 de octubre","time_phrase":"10 am","uncertain":false}"#,
      ),
      reply(
        r#"{"intent":"schedule_in_person","date_phrase":"martes 20 de octubre","time_phrase":"10 am","uncertain":true}"#,
      ),
      reply(
        r#"{"intent":"schedule_in_person","date_phrase":"martes 20 de octubre","time_phrase":"10 am","uncertain":false}"#,
      ),
    ])
    .await;
    let cfg = configuration(endpoint);
    let today = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
    let text = "Quiero cita para Ana Pérez, por dolor de cabeza, martes 20 de octubre a las 10 am; cuenta 1234567890; código 983721; opción 1";
    for _ in 0..4 {
      assert_eq!(interpret(&cfg, text, today).await, None);
    }
    assert_eq!(
      interpret(&cfg, text, today).await,
      Some(Preference {
        date: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
        minute: Some(600),
      })
    );
    let calls = provider.calls.lock().await;
    assert_eq!(calls.len(), 5);
    for call in calls.iter() {
      assert_eq!(call.api_key.as_deref(), Some("synthetic-claude-key"));
      let wire: Value = serde_json::from_slice(&call.body).unwrap();
      let user_text = wire["messages"][0]["content"].as_str().unwrap();
      assert!(!user_text.contains("ana pérez"));
      assert!(!user_text.contains("dolor de cabeza"));
      assert!(!user_text.contains("1234567890"));
      assert!(!user_text.contains("983721"));
      assert!(!user_text.contains("opción"));
      assert!(!user_text.split_whitespace().any(|word| word == "1"));
    }
    task.abort();
  }

  #[tokio::test]
  async fn agenda_parser_returns_only_a_date_and_rejects_time() {
    let (endpoint, _, task) = start_provider(vec![
      reply(
        r#"{"intent":"agenda","date_phrase":"martes 20 de octubre","time_phrase":null,"uncertain":false}"#,
      ),
      reply(
        r#"{"intent":"agenda","date_phrase":"martes 20 de octubre","time_phrase":"10 am","uncertain":false}"#,
      ),
    ])
    .await;
    let cfg = configuration(endpoint);
    let today = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
    assert_eq!(
      interpret_agenda(&cfg, "¿qué tengo el martes 20 de octubre?", today).await,
      Some(Preference {
        date: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
        minute: None,
      })
    );
    assert_eq!(
      interpret_agenda(
        &cfg,
        "¿qué tengo el martes 20 de octubre a las 10 am?",
        today
      )
      .await,
      None
    );
    task.abort();
  }

  #[tokio::test]
  async fn unavailable_provider_is_not_retried() {
    let (endpoint, provider, task) = start_provider(vec![Reply {
      status: StatusCode::SERVICE_UNAVAILABLE,
      body: String::new(),
      location: None,
    }])
    .await;
    let cfg = configuration(endpoint);
    let today = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
    assert_eq!(
      interpret(&cfg, "Quiero cita el martes 20 de octubre", today).await,
      None
    );
    assert_eq!(provider.calls.lock().await.len(), 1);
    task.abort();
  }

  #[tokio::test]
  async fn api_key_is_not_forwarded_through_redirects() {
    let (target_endpoint, target, target_task) =
      start_provider(vec![reply(
        r#"{"intent":"schedule_in_person","date_phrase":"martes 20 de octubre","time_phrase":null,"uncertain":false}"#,
      )])
      .await;
    let (endpoint, source, source_task) = start_provider(vec![Reply {
      status: StatusCode::FOUND,
      body: String::new(),
      location: Some(target_endpoint),
    }])
    .await;
    let cfg = configuration(endpoint);
    let today = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
    assert_eq!(
      interpret(&cfg, "Quiero cita el martes 20 de octubre", today).await,
      None
    );
    let source_calls = source.calls.lock().await;
    assert_eq!(source_calls.len(), 1);
    assert_eq!(
      source_calls[0].api_key.as_deref(),
      Some("synthetic-claude-key")
    );
    assert!(target.calls.lock().await.is_empty());
    source_task.abort();
    target_task.abort();
  }

  #[test]
  fn date_and_time_are_resolved_only_by_application_rules() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
    assert_eq!(
      super::parse_date("martes 20 de octubre", today),
      Some(NaiveDate::from_ymd_opt(2026, 10, 20).unwrap())
    );
    assert_eq!(super::parse_time("10 am"), Some(600));
    assert_eq!(super::parse_time("10"), None);
    assert!(super::safe_text("12, 13 de octubre").is_none());
    assert!(super::safe_text("10:00").is_none());
    assert!(super::safe_text("12 o 13 de octubre").is_none());
    assert!(super::safe_text("2026-10-20, 2026-10-21").is_none());
    assert!(super::safe_text("martes 20 de octubre o miércoles 21 de octubre").is_none());
  }
  #[test]
  fn relative_dates_preserve_qualifiers_and_reject_conflicting_references() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 31).unwrap();
    assert_eq!(
      super::parse_simple("cita pasado mañana a las 10 am", today),
      Some(Preference {
        date: NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
        minute: Some(600)
      })
    );
    for text in [
      "cita hoy y mañana",
      "cita pasado mañana o viernes",
      "cita el martes pasado",
      "cita ayer martes",
    ] {
      assert!(
        super::parse_simple(text, today).is_none(),
        "Unsupported or conflicting relative dates require clarification"
      );
    }
  }
}
