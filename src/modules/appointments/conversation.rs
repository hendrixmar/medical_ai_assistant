use crate::{
  common::config::Config,
  database::Db,
  modules::{
    appointments::{
      availability::{self, CalendarState, Offers, Request},
      booking::{self, Commit, Prepared as BookingPrepared, SelectionRequest},
      model::{Appointment, Format},
      store,
    },
    conversation::{
      parser,
      store::{enqueue_text, Message},
      templates::MENU,
    },
    doctor::Clock,
    google_calendar::client::Client,
    practice,
    whatsapp::inbox::sql,
  },
};
use chrono::{NaiveDate, Timelike};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult};
use serde_json::Value;
use uuid::Uuid;

const APPOINTMENT_SELECT: &str = "SELECT id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,booking_calendar,google_generation,google_selection_version,event_id,event_etag,meet_url,revision,attendance_confirmed_at,cancelled_at,expired_at,created_at,updated_at FROM appointments";

pub(crate) struct Turn<'a> {
  pub tx: &'a DatabaseTransaction,
  pub contact: Uuid,
  pub inbox: &'a str,
  pub original: &'a str,
  pub origin: &'a str,
  pub now: i64,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Token {
  stage: String,
  data: Value,
  updated_at: i64,
}

struct State {
  token: Token,
}

pub(crate) enum Preflight {
  StartIntent(parser::Preference),
  Agenda(NaiveDate),
  Formats {
    token: Token,
    video_enabled: bool,
  },
  Format {
    token: Token,
    video_enabled: bool,
    preference: Option<parser::Preference>,
    timezone: Option<String>,
    offers: Option<Offers>,
  },
  Date {
    token: Token,
    preference: Option<parser::Preference>,
    timezone: Option<String>,
    offers: Option<Offers>,
  },
  Selection {
    token: Token,
    starts_at: Option<i64>,
    result: Option<BookingPrepared>,
  },
}

/// Prepares read-only provider work before the processing transaction is opened.
pub(crate) async fn prepare(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  google: Option<&Client>,
  contact: Option<Uuid>,
  text: &str,
) -> Result<Option<Preflight>, DbErr> {
  let now = clock.now_millis();
  let state = if let Some(contact) = contact {
    read_state(&db.conn, contact, false).await?
  } else {
    None
  };
  let Some(state) = state.filter(|state| state.token.stage != "idle") else {
    let Some(today) = today(db, now).await? else {
      return Ok(None);
    };
    return Ok(
      parser::interpret(cfg, text, today)
        .await
        .map(Preflight::StartIntent),
    );
  };
  let Some(contact) = contact else {
    return Ok(None);
  };
  let token = state.token;
  match token.stage.as_str() {
    "booking_service" => Ok(Some(Preflight::Formats {
      token,
      video_enabled: availability::video_enabled(db).await?,
    })),
    "booking_format" => {
      let Some(format) = parse_format(text) else {
        return Ok(None);
      };
      let enabled = format != Format::Video || availability::video_enabled(db).await?;
      let preferred = saved_preference(&token.data);
      let (timezone, offers) = if enabled {
        if let Some(preference) = preferred {
          let timezone = practice_timezone(db).await?;
          let offers = offer_for_state(
            db,
            clock,
            cfg,
            google,
            contact,
            &token,
            (format, preference),
          )
          .await?;
          (timezone, offers)
        } else {
          (practice_timezone(db).await?, None)
        }
      } else {
        (practice_timezone(db).await?, None)
      };
      Ok(Some(Preflight::Format {
        token,
        video_enabled: enabled,
        preference: preferred,
        timezone,
        offers,
      }))
    }
    "booking_date" | "reschedule_date" | "late_recovery_date" => {
      let appointment = appointment_snapshot(&db.conn, &token.data).await?;
      let format = state_format(&token.data).or_else(|| {
        appointment
          .as_ref()
          .and_then(|appointment| Format::from_db(&appointment.format))
      });
      let date = parse_date_preference(db, cfg, text, format, now).await?;
      let timezone = practice_timezone(db).await?;
      let offers = if let Some(preference) = date {
        offer_for_state(
          db,
          clock,
          cfg,
          google,
          contact,
          &token,
          (format.unwrap_or(Format::InPerson), preference),
        )
        .await?
      } else {
        None
      };
      Ok(Some(Preflight::Date {
        token,
        preference: date,
        timezone,
        offers,
      }))
    }
    "booking_choice" | "reschedule_choice" | "late_recovery_choice" => {
      let Some(starts_at) = offered_start(&token.data, text) else {
        let format = state_format(&token.data);
        let preference = parse_date_preference(db, cfg, text, format, now).await?;
        if let Some(preference) = preference {
          let appointment = appointment_snapshot(&db.conn, &token.data).await?;
          let format = appointment
            .as_ref()
            .and_then(|appointment| Format::from_db(&appointment.format))
            .or(format)
            .unwrap_or(Format::InPerson);
          let timezone = practice_timezone(db).await?;
          let offers = offer_for_state(
            db,
            clock,
            cfg,
            google,
            contact,
            &token,
            (format, preference),
          )
          .await?;
          return Ok(Some(Preflight::Date {
            token,
            preference: Some(preference),
            timezone,
            offers,
          }));
        }
        return Ok(Some(Preflight::Selection {
          token,
          starts_at: None,
          result: None,
        }));
      };
      let request = selection_request(db, contact, &token.data, starts_at).await?;
      let result = booking::prepare_selection(
        db,
        clock,
        cfg,
        google,
        request,
        token.data["patient_name"].as_str(),
      )
      .await?;
      Ok(Some(Preflight::Selection {
        token,
        starts_at: Some(starts_at),
        result: Some(result),
      }))
    }
    _ => Ok(None),
  }
}

/// Applies Contact-originated appointment choices in the same transaction as the inbox job/outbox.
pub(crate) async fn handle(
  turn: &Turn<'_>,
  text: &str,
  preflight: Option<Preflight>,
) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  let state = read_state(tx, contact, true).await?;
  let current = state.as_ref().map(|state| &state.token);
  let matching = match preflight.as_ref() {
    Some(Preflight::StartIntent(_)) => current.is_none_or(|token| token.stage == "idle"),
    Some(Preflight::Formats { token, .. })
    | Some(Preflight::Format { token, .. })
    | Some(Preflight::Date { token, .. })
    | Some(Preflight::Selection { token, .. }) => current == Some(token),
    Some(Preflight::Agenda(_)) | None => false,
  };
  let mut preflight = if matching { preflight } else { None };
  if current.is_none_or(|token| token.stage == "idle") {
    if let Some(Preflight::StartIntent(preference)) = preflight.take() {
      return begin_booking(turn, Some(preference)).await;
    }
  }
  let stage = current.map(|token| token.stage.as_str()).unwrap_or("idle");
  let data = current
    .map(|token| token.data.clone())
    .unwrap_or(Value::Null);
  if text.trim().eq_ignore_ascii_case("VOLVER") || text.trim().eq_ignore_ascii_case("MENU") {
    idle(tx, contact, now).await?;
    return send(turn, None, &format!("Volvió al menú principal:\n{MENU}")).await;
  }
  let fifth_offered = match stage {
    "booking_service" => data["services"]
      .as_array()
      .is_some_and(|rows| rows.len() >= 5),
    "booking_choice" | "reschedule_choice" | "late_recovery_choice" => data["choices"]
      .as_array()
      .is_some_and(|rows| rows.len() >= 5),
    "receipt_selection" => data["candidates"]
      .as_array()
      .is_some_and(|rows| rows.len() >= 5),
    _ => false,
  };
  if text.trim().eq_ignore_ascii_case("DOCTOR") || (text.trim() == "5" && !fifth_offered) {
    return create_followup(turn, appointment_id(&data)).await;
  }
  if matches!(
    stage,
    "reschedule_date" | "reschedule_choice" | "late_recovery_date" | "late_recovery_choice"
  ) {
    let appointment = if let Some(id) = appointment_id(&data) {
      locked_appointment(tx, id).await?
    } else {
      None
    };
    let valid = appointment.as_ref().is_some_and(|appointment| {
      appointment.contact_id == contact
        && data["revision"].as_i64() == Some(appointment.revision)
        && if stage.starts_with("late_recovery_") {
          appointment.status == "expired"
            && appointment.payment == "verified"
            && appointment.format == "video"
        } else {
          eligible_open(appointment, now)
        }
    });
    if !valid {
      idle(tx, contact, now).await?;
      return send(turn, None, "La cita cambió; no modifiqué sus registros ni reservé otro horario. Escriba 2 para consultar las opciones actuales o DOCTOR para atención personal.").await;
    }
  }
  if stage == "receipt_selection" {
    return handle_receipt_selection(turn, &data, text).await;
  }
  if stage == "attendance_select" {
    return handle_attendance_choice(turn, &data, text).await;
  }
  if stage == "cancel_select" {
    return handle_cancel_choice(turn, &data, text).await;
  }
  if stage == "cancel_confirm" {
    return handle_cancel_confirmation(turn, &data, text).await;
  }
  if matches!(
    stage,
    "booking_choice" | "reschedule_choice" | "late_recovery_choice"
  ) {
    if let Some(Preflight::Date { .. }) = preflight.as_ref() {
      if let Some(Preflight::Date {
        mut token,
        preference,
        timezone,
        offers,
      }) = preflight.take()
      {
        token.stage = state_date_stage(&data).to_owned();
        return handle_date(turn, text, &token, preference, timezone, offers).await;
      }
    }
    if let Some(Preflight::Selection {
      token,
      starts_at,
      result,
    }) = preflight.take()
    {
      if current == Some(&token) {
        return handle_booking_choice(turn, &data, starts_at, result).await;
      }
    }
    send(turn, None,
  "No pude validar esa opción. Responda con el número o la hora exacta de una opción mostrada, o envíe una nueva fecha para actualizarla.")
  .await?;
    return Ok(());
  }
  if matches!(
    stage,
    "booking_date" | "reschedule_date" | "late_recovery_date"
  ) {
    if let Some(Preflight::Date {
      token,
      preference,
      timezone,
      offers,
    }) = preflight.take()
    {
      if current == Some(&token) {
        return handle_date(turn, text, &token, preference, timezone, offers).await;
      }
    }
    send(turn, None,
  "Indíqueme la fecha en formato AAAA-MM-DD para consultar horarios. No se reserva nada al mostrar opciones.")
  .await?;
    return Ok(());
  }
  if stage == "booking_format" {
    if let Some(Preflight::Format {
      token,
      video_enabled,
      preference,
      timezone,
      offers,
    }) = preflight.take()
    {
      if current == Some(&token) {
        return handle_format(
          turn,
          &data,
          text,
          video_enabled,
          preference,
          timezone,
          offers,
        )
        .await;
      }
    }
    send(turn, None,
  "Elija el formato disponible: 1 presencial o 2 video. Escriba 5 para solicitar atención personal.")
  .await?;
    return Ok(());
  }
  if stage == "booking_service" {
    let video_enabled = match preflight.take() {
      Some(Preflight::Formats {
        token,
        video_enabled,
      }) if current == Some(&token) => video_enabled,
      _ => false,
    };
    return handle_service(turn, &data, text, video_enabled).await;
  }
  if stage == "booking_patient" {
    return handle_patient(turn, &data, text).await;
  }
  if stage == "reschedule_select" {
    return handle_reschedule_choice(turn, &data, text).await;
  }
  if stage == "idle" {
    if let Ok(option) = text.trim().parse::<u8>() {
      match option {
        1 => return begin_booking(turn, None).await,
        2 => return begin_reschedule(turn).await,
        3 => return begin_cancellation(turn).await,
        4 => {
          send(turn, None,
      "Para un pago pendiente, envíe el comprobante en este chat. El comprobante no confirma ni verifica el pago por sí solo.")
      .await?;
          return Ok(());
        }
        5 => return create_followup(turn, None).await,
        _ => {}
      }
    }
    if let Some(attendance) = attendance_reply(text) {
      return begin_attendance(turn, attendance).await;
    }
    send(
      turn,
      None,
      &format!("Por favor, escriba el número de la opción que necesita:\n{MENU}"),
    )
    .await?;
    return Ok(());
  }
  send(turn, None,
&format!("No pude identificar esa respuesta. Por favor, use la opción solicitada o escriba VOLVER para ver el menú:\n{MENU}"))
.await?;
  Ok(())
}

async fn parse_date_preference(
  db: &Db,
  cfg: &Config,
  text: &str,
  format: Option<Format>,
  now: i64,
) -> Result<Option<parser::Preference>, DbErr> {
  let Some(today) = today(db, now).await? else {
    return Ok(None);
  };
  if let Some(preference) = parser::parse_simple(text, today) {
    return Ok(Some(preference));
  }
  if format == Some(Format::InPerson) {
    return Ok(parser::interpret_date(cfg, text, today).await);
  }
  Ok(None)
}

pub(crate) async fn today(db: &Db, now: i64) -> Result<Option<NaiveDate>, DbErr> {
  let Some(timezone) = practice_timezone(db)
    .await?
    .and_then(|name| name.parse::<chrono_tz::Tz>().ok())
  else {
    return Ok(None);
  };
  Ok(
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now)
      .map(|value| value.with_timezone(&timezone).date_naive()),
  )
}

async fn practice_timezone(db: &Db) -> Result<Option<String>, DbErr> {
  Ok(practice::store::load(db).await?.timezone)
}

async fn offer_for_state(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  google: Option<&Client>,
  contact: Uuid,
  token: &Token,
  selection: (Format, parser::Preference),
) -> Result<Option<Offers>, DbErr> {
  let (format, preference) = selection;
  let appointment_id = token.data["appointment_id"]
    .as_str()
    .and_then(|value| value.parse::<Uuid>().ok());
  let appointment = if let Some(id) = appointment_id {
    let Some(appointment) = store::get(&db.conn, id).await? else {
      return Ok(None);
    };
    if appointment.contact_id != contact
      || token.data["revision"].as_i64() != Some(appointment.revision)
      || token.stage.starts_with("late_recovery_")
        && (appointment.status != "expired" || appointment.payment != "verified")
      || token.stage.starts_with("reschedule_")
        && (!matches!(
          appointment.status.as_str(),
          "requested" | "awaiting_payment" | "confirmed"
        ) || appointment.starts_at <= clock.now_millis())
    {
      return Ok(None);
    }
    Some(appointment)
  } else {
    None
  };
  let service_id = appointment
    .as_ref()
    .map(|appointment| appointment.service_id)
    .or_else(|| {
      token.data["service_id"]
        .as_str()
        .and_then(|value| value.parse::<Uuid>().ok())
    });
  let Some(service_id) = service_id else {
    return Ok(None);
  };
  let saved_payment = appointment
    .as_ref()
    .map(|appointment| appointment.payment.as_str());
  let active_expiry = if let Some(appointment) = appointment.as_ref() {
    if token.stage.starts_with("late_recovery_") {
      if db
        .conn
        .query_one(sql(
          "SELECT id FROM appointment_holds WHERE appointment_id=$1 AND active LIMIT 1",
          vec![appointment.id.into()],
        ))
        .await?
        .is_some()
      {
        return Ok(None);
      }
      None
    } else {
      db.conn
        .query_one(sql(
          "SELECT expires_at FROM appointment_holds WHERE appointment_id=$1 AND active LIMIT 1",
          vec![appointment.id.into()],
        ))
        .await?
        .map(|row| row.try_get::<Option<i64>>("", "expires_at"))
        .transpose()?
        .flatten()
    }
  } else {
    None
  };
  let request = Request {
    contact,
    service_id,
    format: appointment
      .as_ref()
      .and_then(|appointment| Format::from_db(&appointment.format))
      .unwrap_or(format),
    date: preference.date,
    period: preference
      .minute
      .map(|minute| (minute, minute.saturating_add(1))),
    exclude_appointment: appointment.as_ref().map(|appointment| appointment.id),
    duration_override: appointment
      .as_ref()
      .map(|appointment| appointment.duration_minutes),
    payment_state: saved_payment,
    receipt_deadline: appointment
      .as_ref()
      .and_then(|appointment| appointment.receipt_deadline),
    hold_expires_at: active_expiry,
  };
  let mut offers = availability::offer(db, clock, cfg, google, request).await?;
  if preference.minute.is_some() && offers.choices.is_empty() {
    offers = availability::offer(
      db,
      clock,
      cfg,
      google,
      Request {
        period: None,
        ..request
      },
    )
    .await?;
  }
  Ok(Some(offers))
}

fn saved_preference(data: &Value) -> Option<parser::Preference> {
  let date = NaiveDate::parse_from_str(data["preferred_date"].as_str()?, "%Y-%m-%d").ok()?;
  let minute = data["preferred_minute"]
    .as_u64()
    .and_then(|value| u16::try_from(value).ok());
  Some(parser::Preference { date, minute })
}

fn state_format(data: &Value) -> Option<Format> {
  Format::from_db(data["format"].as_str()?)
}

fn parse_format(text: &str) -> Option<Format> {
  match text.trim() {
    "1" => Some(Format::InPerson),
    "2" => Some(Format::Video),
    _ => None,
  }
}

async fn selection_request(
  db: &Db,
  contact: Uuid,
  data: &Value,
  starts_at: i64,
) -> Result<SelectionRequest, DbErr> {
  let appointment_id = data["appointment_id"]
    .as_str()
    .and_then(|value| value.parse::<Uuid>().ok());
  let appointment = if let Some(id) = appointment_id {
    store::get(&db.conn, id).await?
  } else {
    None
  };
  let service_id = appointment
    .as_ref()
    .map(|appointment| appointment.service_id)
    .or_else(|| {
      data["service_id"]
        .as_str()
        .and_then(|value| value.parse::<Uuid>().ok())
    })
    .unwrap_or_else(Uuid::nil);
  let format = appointment
    .as_ref()
    .and_then(|appointment| Format::from_db(&appointment.format))
    .or_else(|| state_format(data))
    .unwrap_or(Format::InPerson);
  Ok(SelectionRequest {
    contact,
    service_id,
    format,
    starts_at,
    appointment_id,
    expected_revision: data["revision"].as_i64(),
    late_recovery: data["late_recovery"].as_bool().unwrap_or(false),
  })
}

fn offered_start(data: &Value, text: &str) -> Option<i64> {
  let choices = data["choices"].as_array()?;
  if let Some(index) = text.trim().parse::<usize>().ok().filter(|value| *value > 0) {
    return choices.get(index - 1)?.as_i64();
  }
  let timezone = data["timezone"].as_str()?.parse::<chrono_tz::Tz>().ok()?;
  let wanted = normalize_time_choice(text);
  let matching: Vec<_> = choices
    .iter()
    .filter_map(Value::as_i64)
    .filter(|starts_at| {
      let Some(local) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(*starts_at)
        .map(|time| time.with_timezone(&timezone))
      else {
        return false;
      };
      let time = local.format("%H:%M").to_string();
      let full = local.format("%d/%m/%Y a las %H:%M").to_string();
      wanted == time || wanted == full
    })
    .collect();
  (matching.len() == 1).then(|| matching[0])
}

fn normalize_time_choice(text: &str) -> String {
  let value = text.trim().to_lowercase();
  value
    .strip_prefix("a las ")
    .unwrap_or(&value)
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

async fn read_state<C: ConnectionTrait>(
  conn: &C,
  contact: Uuid,
  lock: bool,
) -> Result<Option<State>, DbErr> {
  let query = if lock {
    "SELECT stage,data,updated_at FROM conversation_state WHERE contact_id=$1 FOR UPDATE"
  } else {
    "SELECT stage,data,updated_at FROM conversation_state WHERE contact_id=$1"
  };
  conn
    .query_one(sql(query, vec![contact.into()]))
    .await?
    .map(|row| {
      Ok(State {
        token: Token {
          stage: row.try_get("", "stage")?,
          data: row.try_get("", "data")?,
          updated_at: row.try_get("", "updated_at")?,
        },
      })
    })
    .transpose()
}

async fn set_state(
  tx: &DatabaseTransaction,
  contact: Uuid,
  stage: &str,
  data: &Value,
  now: i64,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "INSERT INTO conversation_state(contact_id,stage,data,updated_at) VALUES ($1,$2,$3,$4) ON CONFLICT (contact_id) DO UPDATE SET stage=EXCLUDED.stage,data=EXCLUDED.data,updated_at=EXCLUDED.updated_at",
    vec![contact.into(), stage.into(), data.clone().into(), now.into()],
  ))
  .await?;
  Ok(())
}

async fn idle(tx: &DatabaseTransaction, contact: Uuid, now: i64) -> Result<(), DbErr> {
  set_state(tx, contact, "idle", &serde_json::json!({}), now).await
}

async fn send(turn: &Turn<'_>, appointment: Option<(Uuid, i64)>, text: &str) -> Result<(), DbErr> {
  let Turn {
    tx,
    contact,
    inbox,
    origin,
    now,
    ..
  } = *turn;
  let (appointment, revision) =
    appointment.map_or((None, None), |(id, revision)| (Some(id), Some(revision)));
  let key = format!("appointment-conversation:{inbox}");
  enqueue_text(
    tx,
    Message {
      contact,
      appointment,
      key: &key,
      text,
      purpose: "conversation",
      revision,
    },
    origin,
    now,
  )
  .await
}

async fn begin_booking(
  turn: &Turn<'_>,
  preference: Option<parser::Preference>,
) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  if booking::open_limit_reached(tx, contact, None, now).await? {
    return open_limit_reply(turn).await;
  }
  let data = match preference {
    Some(preference) => serde_json::json!({
      "preferred_date": preference.date.to_string(),
      "preferred_minute": preference.minute,
    }),
    None => serde_json::json!({}),
  };
  set_state(tx, contact, "booking_patient", &data, now).await?;
  send(turn, None,
    "Para agendar, indique únicamente el nombre de la persona que recibirá la cita. No necesito el motivo de consulta."
  ).await
}

async fn open_limit_reply(turn: &Turn<'_>) -> Result<(), DbErr> {
  idle(turn.tx, turn.contact, turn.now).await?;
  send(turn, None, "Ya tiene tres citas abiertas. No se creó otra; use 2 o 3 para modificar una cita, o escriba 5.").await
}

async fn handle_patient(turn: &Turn<'_>, data: &Value, text: &str) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  if !booking::valid_patient(Some(text)) {
    send(
      turn,
      None,
      "Escriba solo un nombre válido, sin datos clínicos ni motivo de consulta.",
    )
    .await?;
    return Ok(());
  }
  let services = services(tx).await?;
  if services.is_empty() {
    return create_followup(turn, None).await;
  }
  let offered = services
    .iter()
    .map(|service| {
      serde_json::json!({
        "id": service.id,
        "name": service.name,
        "duration_minutes": service.duration_minutes,
        "price_centavos": service.price_centavos,
      })
    })
    .collect::<Vec<_>>();
  let mut next = data.clone();
  next["patient_name"] = Value::String(text.trim().to_owned());
  next["services"] = Value::Array(offered);
  set_state(tx, contact, "booking_service", &next, now).await?;
  let options = services
    .iter()
    .enumerate()
    .map(|(index, service)| {
      format!(
        "{}. {} — {} minutos — ${}.{:02} MXN",
        index + 1,
        service.name,
        service.duration_minutes,
        service.price_centavos / 100,
        service.price_centavos % 100
      )
    })
    .collect::<Vec<_>>()
    .join("\n");
  send(
    turn,
    None,
    &format!("Elija un servicio escribiendo su número:\n{options}"),
  )
  .await
}

async fn services<C: ConnectionTrait>(conn: &C) -> Result<Vec<practice::model::Service>, DbErr> {
  conn
    .query_all(sql(
      "SELECT id,name,duration_minutes,price_centavos FROM services ORDER BY id",
      vec![],
    ))
    .await?
    .iter()
    .map(|row| practice::model::Service::from_query_result(row, ""))
    .collect()
}

async fn handle_service(
  turn: &Turn<'_>,
  data: &Value,
  text: &str,
  video_enabled: bool,
) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  let Some(offered) = data["services"].as_array() else {
    idle(tx, contact, now).await?;
    return send(turn, None, &format!("Volvamos al menú:\n{MENU}")).await;
  };
  let selected = text
    .trim()
    .parse::<usize>()
    .ok()
    .filter(|value| *value > 0)
    .and_then(|index| offered.get(index - 1))
    .or_else(|| {
      let wanted = text.trim().to_lowercase();
      let matches = offered
        .iter()
        .filter(|service| {
          service["name"]
            .as_str()
            .is_some_and(|name| name.to_lowercase() == wanted)
        })
        .collect::<Vec<_>>();
      (matches.len() == 1).then(|| matches[0])
    });
  let Some(selected) = selected else {
    send(
      turn,
      None,
      "Responda con el número o el nombre exacto de uno de los servicios ofrecidos.",
    )
    .await?;
    return Ok(());
  };
  let id = selected["id"]
    .as_str()
    .and_then(|value| value.parse::<Uuid>().ok());
  let current = services(tx).await?;
  let Some(service) = id.and_then(|id| current.into_iter().find(|service| service.id == id)) else {
    idle(tx, contact, now).await?;
    send(
      turn,
      None,
      "El servicio cambió y no pude conservar esa opción. Escriba 1 para iniciar de nuevo.",
    )
    .await?;
    return Ok(());
  };
  let mut next = data.clone();
  next["service_id"] = Value::String(service.id.to_string());
  next["service_name"] = Value::String(service.name);
  next["duration_minutes"] = Value::from(service.duration_minutes);
  next["price_centavos"] = Value::from(service.price_centavos);
  set_state(tx, contact, "booking_format", &next, now).await?;
  let format_choice = if video_enabled {
    "1 Presencial\n2 Video"
  } else {
    "1 Presencial\nVideo no está disponible por ahora."
  };
  send(
    turn,
    None,
    &format!("Elija el formato de la cita:\n{format_choice}"),
  )
  .await
}

async fn handle_format(
  turn: &Turn<'_>,
  data: &Value,
  text: &str,
  video_enabled: bool,
  preference: Option<parser::Preference>,
  timezone: Option<String>,
  offers: Option<Offers>,
) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  let Some(format) = parse_format(text) else {
    send(
      turn,
      None,
      "Elija 1 para presencial o 2 para video, cuando esa opción esté habilitada.",
    )
    .await?;
    return Ok(());
  };
  if format == Format::Video && !video_enabled {
    send(turn, None, "No puedo ofrecer horarios de video porque ese formato no está habilitado. Elija 1 para presencial o escriba VOLVER.").await?;
    return Ok(());
  }
  let mut next = data.clone();
  next["format"] = Value::String(format.as_str().to_owned());
  if let Some(preference) = preference {
    next["preferred_date"] = Value::String(preference.date.to_string());
    next["preferred_minute"] = preference.minute.map_or(Value::Null, Value::from);
  }
  set_state(tx, contact, "booking_date", &next, now).await?;
  if let (Some(preference), Some(timezone), Some(offers)) = (preference, timezone, offers) {
    return show_date_offers(turn, "booking_date", &next, preference, &timezone, offers).await;
  }
  send(turn, None,
"Indique la fecha deseada en formato AAAA-MM-DD. Puede escribir una fecha y hora; no se reserva nada al mostrar opciones.")
.await
}

async fn handle_date(
  turn: &Turn<'_>,
  text: &str,
  token: &Token,
  preference: Option<parser::Preference>,
  timezone: Option<String>,
  offers: Option<Offers>,
) -> Result<(), DbErr> {
  let Turn { tx, .. } = *turn;
  let Some(preference) = preference else {
    if token.stage == "reschedule_date" && requests_rule_change(text) {
      return create_followup(turn, appointment_id(&token.data)).await;
    }
    send(turn, appointment_snapshot(tx, &token.data).await?.as_ref().map(|appointment| (appointment.id, appointment.revision)),
  "No pude identificar una fecha segura. Envíe una fecha en formato AAAA-MM-DD; su cita original no cambió.")
  .await?;
    return Ok(());
  };
  let Some(timezone) = timezone else {
    if token.stage != "booking_date" {
      return create_followup(turn, appointment_id(&token.data)).await;
    }
    send(turn, None, "No puedo buscar horarios porque el consultorio no tiene una zona horaria válida. Escriba 5 para solicitar atención personal.").await?;
    return Ok(());
  };
  let Some(offers) = offers else {
    if token.stage == "reschedule_date" || token.stage == "late_recovery_date" {
      return create_followup(turn, appointment_id(&token.data)).await;
    }
    send(turn, None, "No pude validar el servicio, formato o calendario. Puede intentarlo de nuevo con otra fecha o escribir 5 para atención personal.").await?;
    return Ok(());
  };
  let mut next = token.data.clone();
  next["late_recovery"] = Value::Bool(token.stage == "late_recovery_date");
  if let Some(appointment) = appointment_snapshot(tx, &next).await? {
    next["patient_name"] = Value::String(appointment.patient_name);
    next["service_id"] = Value::String(appointment.service_id.to_string());
    next["service_name"] = Value::String(appointment.service_name);
    next["duration_minutes"] = Value::from(appointment.duration_minutes);
    next["price_centavos"] = Value::from(appointment.price_centavos);
    next["format"] = Value::String(appointment.format);
    next["appointment_id"] = Value::String(appointment.id.to_string());
    next["revision"] = Value::from(appointment.revision);
  }
  next["date"] = Value::String(preference.date.to_string());
  next["timezone"] = Value::String(timezone.clone());
  show_date_offers(turn, &token.stage, &next, preference, &timezone, offers).await
}

fn requests_rule_change(text: &str) -> bool {
  let value = text.to_lowercase();
  [
    "servicio",
    "formato",
    "video",
    "presencial",
    "cambiar",
    "cambio",
  ]
  .iter()
  .any(|word| value.contains(word))
}

async fn show_date_offers(
  turn: &Turn<'_>,
  source_stage: &str,
  data: &Value,
  preference: parser::Preference,
  timezone: &str,
  offers: Offers,
) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  if offers.choices.is_empty() {
    if matches!(source_stage, "reschedule_date" | "late_recovery_date") {
      return create_followup(turn, appointment_id(data)).await;
    }
    let message = match offers.calendar {
    CalendarState::Unavailable => "No pude confirmar disponibilidad de Google Calendar, así que no puedo ofrecer un horario seguro ahora. No se reservó nada; intente con otra fecha más tarde o escriba 5.",
    CalendarState::VideoDisabled => "El formato video no está habilitado y no se reservó nada. Escriba VOLVER para elegir otra opción.",
    CalendarState::NotConfigured => "No puedo buscar horarios hasta que el consultorio complete su configuración. Escriba 5 para solicitar atención personal.",
    CalendarState::Checked => "No encontré horarios que cumplan las reglas del consultorio para esa fecha. Pruebe otra fecha o escriba 5 para solicitar atención personal.",
  };
    send(turn, None, message).await?;
    return Ok(());
  }
  let mut next = data.clone();
  next["date"] = Value::String(preference.date.to_string());
  next["timezone"] = Value::String(timezone.to_owned());
  next["choices"] = Value::Array(
    offers
      .choices
      .iter()
      .map(|choice| Value::from(choice.starts_at))
      .collect(),
  );
  next["calendar_unavailable"] = Value::Bool(offers.calendar == CalendarState::Unavailable);
  let choice_stage = match source_stage {
    "reschedule_date" => "reschedule_choice",
    "late_recovery_date" => "late_recovery_choice",
    _ => "booking_choice",
  };
  set_state(tx, contact, choice_stage, &next, now).await?;
  let local_date = preference.date.format("%d/%m/%Y");
  let mut message = format!("Horarios posibles para el {local_date} ({timezone}):\n");
  if offers.calendar == CalendarState::Unavailable {
    message.push_str("Google Calendar no pudo consultarse. Estas opciones aún no están confirmadas y la selección quedará como solicitud pendiente.\n");
  } else {
    message.push_str(
      "Mostrar opciones no reserva el horario. Confirme con el número o la hora exacta:\n",
    );
  }
  let requested_available = preference.minute.is_none_or(|minute| {
    let Ok(timezone) = timezone.parse::<chrono_tz::Tz>() else {
      return false;
    };
    offers.choices.iter().any(|choice| {
      chrono::DateTime::<chrono::Utc>::from_timestamp_millis(choice.starts_at).is_some_and(|utc| {
        let local = utc.with_timezone(&timezone);
        (local.hour() * 60 + local.minute()) as u16 == minute
      })
    })
  });
  if !requested_available {
    message
      .push_str("La hora solicitada no está disponible; puede elegir una de estas alternativas.\n");
  }
  for (index, choice) in offers.choices.iter().enumerate() {
    message.push_str(&format!(
      "{}. {}\n",
      index + 1,
      store::local_time(choice.starts_at, timezone)
    ));
  }
  message.push_str("Para atención personal, escriba DOCTOR (opción 5 del menú).");
  send(turn, None, &message).await
}

async fn handle_booking_choice(
  turn: &Turn<'_>,
  data: &Value,
  starts_at: Option<i64>,
  result: Option<BookingPrepared>,
) -> Result<(), DbErr> {
  let Turn {
    tx,
    inbox,
    origin,
    now,
    ..
  } = *turn;
  let (Some(_), Some(result)) = (starts_at, result) else {
    send(
      turn,
      None,
      "Elija una opción numerada o escriba la hora exacta de una de las opciones mostradas.",
    )
    .await?;
    return Ok(());
  };
  match result {
  BookingPrepared::Ready(preflight) => {
    match booking::commit_selection(tx, preflight, inbox, data["patient_name"].as_str(), origin, now).await? {
      Commit::Applied { .. } => Ok(()),
      Commit::Followup => create_followup(turn, appointment_id(data)).await,
      Commit::OpenLimit => open_limit_reply(turn).await,
      // Release this transaction and let the durable worker repeat provider reads outside it.
      Commit::Stale => Err(DbErr::Custom("Appointment selection changed; reprepare".into())),
    }
  }
  BookingPrepared::Refresh(offers) => {
    let timezone = data["timezone"].as_str().unwrap_or("UTC");
    let date = NaiveDate::parse_from_str(data["date"].as_str().unwrap_or_default(), "%Y-%m-%d").ok();
    if let Some(date) = date {
      show_date_offers(turn, state_date_stage(data), data, parser::Preference { date, minute: None }, timezone, offers).await
    } else {
      send(turn, None, "La opción quedó desactualizada. Envíe una nueva fecha para consultar horarios; la cita original no cambió.").await
    }
  }
  BookingPrepared::Followup => {
    create_followup(turn, appointment_id(data)).await
  }
  BookingPrepared::Invalid => {
    send(turn, None, "No pude validar la selección actual. Envíe una fecha nueva para revisar horarios; no se modificó su cita.").await
  }
}
}

fn state_date_stage(data: &Value) -> &'static str {
  if data["late_recovery"].as_bool() == Some(true) {
    "late_recovery_date"
  } else if data["appointment_id"].is_string() {
    "reschedule_date"
  } else {
    "booking_date"
  }
}

async fn begin_reschedule(turn: &Turn<'_>) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  let appointments = store::open_for_contact(tx, contact, now).await?;
  if appointments.is_empty() {
    send(turn, None, "No encuentro citas futuras abiertas para cambiar. Escriba 1 para agendar o 5 para solicitar atención personal.").await?;
    return Ok(());
  }
  let data = appointment_candidates(&appointments);
  set_state(tx, contact, "reschedule_select", &data, now).await?;
  send(turn, None, &format!("Seleccione la cita que desea cambiar. La cita original se conserva mientras elige un nuevo horario:\n{}", render_appointment_options(&appointments))).await
}

async fn handle_reschedule_choice(turn: &Turn<'_>, data: &Value, text: &str) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  let candidates = data["appointments"].as_array();
  let Some(selected) = candidates.and_then(|rows| numbered_json(rows, text)) else {
    send(turn, None, "Responda con el número de una cita mostrada.").await?;
    return Ok(());
  };
  let Some(id) = selected["id"]
    .as_str()
    .and_then(|value| value.parse::<Uuid>().ok())
  else {
    idle(tx, contact, now).await?;
    return Ok(());
  };
  let Some(appointment) = locked_appointment(tx, id).await? else {
    idle(tx, contact, now).await?;
    send(
      turn,
      None,
      "No pude encontrar esa cita. No se cambió ningún horario.",
    )
    .await?;
    return Ok(());
  };
  if appointment.contact_id != contact
    || selected["revision"].as_i64() != Some(appointment.revision)
    || !eligible_open(&appointment, now)
  {
    idle(tx, contact, now).await?;
    send(turn, None, "La cita cambió y ya no puedo iniciar el cambio con esa opción. Su cita permanece según el estado actual.").await?;
    return Ok(());
  }
  let next = serde_json::json!({
    "appointment_id": appointment.id,
    "revision": appointment.revision,
  });
  set_state(tx, contact, "reschedule_date", &next, now).await?;
  store::history(tx, appointment.id, "Contact", "reschedule_requested", now).await?;
  send(turn, Some((appointment.id, appointment.revision)),
&format!("Indique una nueva fecha en formato AAAA-MM-DD para cambiar su cita de {}. Se conservan servicio, formato, pago y la cita original hasta que el nuevo horario se adquiera.", store::local_time(appointment.starts_at, &appointment.timezone)))
.await
}

async fn begin_cancellation(turn: &Turn<'_>) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  let appointments = store::open_for_contact(tx, contact, now).await?;
  if appointments.is_empty() {
    send(
      turn,
      None,
      "No encuentro citas futuras abiertas para cancelar.",
    )
    .await?;
    return Ok(());
  }
  let data = appointment_candidates(&appointments);
  set_state(tx, contact, "cancel_select", &data, now).await?;
  send(
    turn,
    None,
    &format!(
      "Seleccione la cita que desea cancelar:\n{}",
      render_appointment_options(&appointments)
    ),
  )
  .await
}

async fn handle_cancel_choice(turn: &Turn<'_>, data: &Value, text: &str) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  let candidates = data["appointments"].as_array();
  let Some(selected) = candidates.and_then(|rows| numbered_json(rows, text)) else {
    send(turn, None, "Responda con el número de una cita mostrada.").await?;
    return Ok(());
  };
  let Some(id) = selected["id"]
    .as_str()
    .and_then(|value| value.parse::<Uuid>().ok())
  else {
    idle(tx, contact, now).await?;
    return Ok(());
  };
  let Some(appointment) = locked_appointment(tx, id).await? else {
    idle(tx, contact, now).await?;
    send(turn, None, "No pude encontrar esa cita. No se canceló.").await?;
    return Ok(());
  };
  if appointment.contact_id != contact
    || selected["revision"].as_i64() != Some(appointment.revision)
    || !eligible_open(&appointment, now)
  {
    idle(tx, contact, now).await?;
    send(
      turn,
      None,
      "La cita cambió y ya no puedo cancelar con esa opción. No hice cambios.",
    )
    .await?;
    return Ok(());
  }
  let policy = tx
    .query_one(sql(
      "SELECT cancellation_policy FROM practice WHERE id=1",
      vec![],
    ))
    .await?
    .and_then(|row| {
      row
        .try_get::<Option<String>>("", "cancellation_policy")
        .ok()
        .flatten()
    })
    .filter(|value| !value.trim().is_empty());
  let policy_text = policy
    .as_deref()
    .unwrap_or("El consultorio no tiene una política de cancelación registrada.");
  let next = serde_json::json!({
    "appointment_id": appointment.id,
    "revision": appointment.revision,
  });
  set_state(tx, contact, "cancel_confirm", &next, now).await?;
  let text = format!(
  "Cita de {} para {} el {}. Política de cancelación: {}\nSi desea cancelar, escriba exactamente CONFIRMAR. Si no confirma, la cita y su horario permanecen sin cambios.",
  appointment.service_name,
  appointment.patient_name,
  store::local_time(appointment.starts_at, &appointment.timezone),
  policy_text
);
  send(turn, Some((appointment.id, appointment.revision)), &text).await
}

async fn handle_cancel_confirmation(
  turn: &Turn<'_>,
  data: &Value,
  text: &str,
) -> Result<(), DbErr> {
  let Turn {
    tx,
    contact,
    origin,
    now,
    ..
  } = *turn;
  if text.trim().eq_ignore_ascii_case("VOLVER") {
    idle(tx, contact, now).await?;
    send(
      turn,
      None,
      "No cancelé su cita. Puede continuar con la opción 2 para cambiarla.",
    )
    .await?;
    return Ok(());
  }
  if !text.trim().eq_ignore_ascii_case("CONFIRMAR") {
    send(turn, None, "Para cancelar, responda exactamente CONFIRMAR. Cualquier otra respuesta deja la cita sin cambios.").await?;
    return Ok(());
  }
  let Some(id) = appointment_id(data) else {
    idle(tx, contact, now).await?;
    send(turn, None, "No pude validar la cita. No se canceló.").await?;
    return Ok(());
  };
  let Some(appointment) = locked_appointment(tx, id).await? else {
    idle(tx, contact, now).await?;
    return Ok(());
  };
  if appointment.contact_id != contact
    || data["revision"].as_i64() != Some(appointment.revision)
    || !eligible_open(&appointment, now)
  {
    idle(tx, contact, now).await?;
    send(
      turn,
      None,
      "La cita cambió o dejó de estar abierta. No hice una cancelación nueva.",
    )
    .await?;
    return Ok(());
  }
  if store::cancel(tx, id, "Contact", origin, now).await? {
    idle(tx, contact, now).await?;
  } else {
    idle(tx, contact, now).await?;
    send(
      turn,
      None,
      "No pude cancelar la cita con el estado actual; no se modificó.",
    )
    .await?;
  }
  Ok(())
}

fn appointment_candidates(appointments: &[Appointment]) -> Value {
  serde_json::json!({
    "appointments": appointments.iter().map(|appointment| serde_json::json!({
      "id": appointment.id,
      "revision": appointment.revision,
    })).collect::<Vec<_>>(),
  })
}

fn render_appointment_options(appointments: &[Appointment]) -> String {
  appointments
    .iter()
    .enumerate()
    .map(|(index, appointment)| {
      format!(
        "{}. {} para {} — {} — {}",
        index + 1,
        appointment.service_name,
        appointment.patient_name,
        format_name(&appointment.format),
        store::local_time(appointment.starts_at, &appointment.timezone)
      )
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn numbered_json<'a>(rows: &'a [Value], text: &str) -> Option<&'a Value> {
  let index = text.trim().parse::<usize>().ok()?.checked_sub(1)?;
  rows.get(index)
}

fn format_name(value: &str) -> &'static str {
  match value {
    "in_person" => "presencial",
    "video" => "video",
    _ => "formato registrado",
  }
}

fn eligible_open(appointment: &Appointment, now: i64) -> bool {
  matches!(
    appointment.status.as_str(),
    "requested" | "awaiting_payment" | "confirmed"
  ) && appointment.starts_at > now
}

async fn locked_appointment(
  tx: &DatabaseTransaction,
  id: Uuid,
) -> Result<Option<Appointment>, DbErr> {
  tx.query_one(sql(
    &format!("{APPOINTMENT_SELECT} WHERE id=$1 FOR UPDATE"),
    vec![id.into()],
  ))
  .await?
  .map(|row| Appointment::from_query_result(&row, ""))
  .transpose()
}

async fn appointment_snapshot<C: ConnectionTrait>(
  conn: &C,
  data: &Value,
) -> Result<Option<Appointment>, DbErr> {
  let Some(id) = appointment_id(data) else {
    return Ok(None);
  };
  store::get(conn, id).await
}

fn appointment_id(data: &Value) -> Option<Uuid> {
  data["appointment_id"]
    .as_str()
    .and_then(|value| value.parse().ok())
}

async fn create_followup(turn: &Turn<'_>, appointment: Option<Uuid>) -> Result<(), DbErr> {
  let Turn {
    tx,
    contact,
    inbox,
    original,
    origin,
    now,
    ..
  } = *turn;
  let existing = tx
    .query_one(sql(
      "SELECT id FROM followup_requests WHERE contact_id=$1 AND resolved_at IS NULL LIMIT 1",
      vec![contact.into()],
    ))
    .await?;
  if existing.is_none() {
    tx.execute(sql(
    "INSERT INTO followup_requests(id,contact_id,inbox_message_id,original_target,created_at) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (contact_id) WHERE resolved_at IS NULL DO NOTHING",
    vec![Uuid::now_v7().into(), contact.into(), inbox.into(), original.into(), now.into()],
  ))
  .await?;
  }
  if let Some(appointment) = appointment {
    store::history(tx, appointment, "Contact", "followup_requested", now).await?;
    tx.execute(sql(
    "INSERT INTO message_appointments(inbox_message_id,appointment_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
    vec![inbox.into(), appointment.into()],
  )).await?;
  }
  idle(tx, contact, now).await?;
  crate::modules::conversation::store::enqueue(
    tx,
    crate::modules::conversation::store::Reply {
      contact,
      inbox,
      target: original,
      followup: None,
      template: "followup",
    },
    origin,
    now,
  )
  .await
}

async fn begin_attendance(turn: &Turn<'_>, reply: bool) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  if !reply {
    send(
      turn,
      None,
      "No cancelé ni cambié su cita. Para cambiarla escriba 2; para cancelar, escriba 3.",
    )
    .await?;
    return Ok(());
  }
  let pending = tx
  .query_all(sql(
    "SELECT id,revision,patient_name,service_name,format,starts_at,timezone FROM appointments WHERE contact_id=$1 AND status='confirmed' AND attendance_confirmed_at IS NULL AND starts_at>$2 ORDER BY starts_at,id LIMIT 4",
    vec![contact.into(), now.into()],
  ))
  .await?;
  if pending.is_empty() {
    send(
      turn,
      None,
      "No encuentro una cita futura pendiente de confirmación de asistencia.",
    )
    .await?;
    return Ok(());
  }
  if pending.len() == 1 {
    let row = &pending[0];
    let id: Uuid = row.try_get("", "id")?;
    let revision: i64 = row.try_get("", "revision")?;
    return confirm_attendance(turn, id, revision).await;
  }
  let candidates = pending
    .iter()
    .map(|row| {
      Ok(serde_json::json!({
        "id": row.try_get::<Uuid>("", "id")?,
        "revision": row.try_get::<i64>("", "revision")?,
      }))
    })
    .collect::<Result<Vec<_>, DbErr>>()?;
  set_state(
    tx,
    contact,
    "attendance_select",
    &serde_json::json!({"appointments": candidates}),
    now,
  )
  .await?;
  let options = pending
    .iter()
    .enumerate()
    .map(|(index, row)| {
      let patient: String = row.try_get("", "patient_name")?;
      let service: String = row.try_get("", "service_name")?;
      let format: String = row.try_get("", "format")?;
      let starts_at: i64 = row.try_get("", "starts_at")?;
      let timezone: String = row.try_get("", "timezone")?;
      Ok::<_, DbErr>(format!(
        "{}. {} para {} — {} — {}",
        index + 1,
        service,
        patient,
        format_name(&format),
        store::local_time(starts_at, &timezone)
      ))
    })
    .collect::<Result<Vec<_>, _>>()?
    .join("\n");
  send(
    turn,
    None,
    &format!(
      "¿A cuál cita se refiere? Responda con su número para confirmar asistencia:\n{options}"
    ),
  )
  .await
}

async fn handle_attendance_choice(turn: &Turn<'_>, data: &Value, text: &str) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  let candidates = data["appointments"].as_array();
  let Some(selected) = candidates.and_then(|rows| numbered_json(rows, text)) else {
    send(
      turn,
      None,
      "Responda con el número de la cita cuya asistencia confirma.",
    )
    .await?;
    return Ok(());
  };
  let Some(id) = selected["id"]
    .as_str()
    .and_then(|value| value.parse::<Uuid>().ok())
  else {
    idle(tx, contact, now).await?;
    return Ok(());
  };
  let Some(revision) = selected["revision"].as_i64() else {
    idle(tx, contact, now).await?;
    return Ok(());
  };
  confirm_attendance(turn, id, revision).await
}

async fn confirm_attendance(
  turn: &Turn<'_>,
  id: Uuid,
  expected_revision: i64,
) -> Result<(), DbErr> {
  let Turn {
    tx, contact, now, ..
  } = *turn;
  let Some(appointment) = locked_appointment(tx, id).await? else {
    idle(tx, contact, now).await?;
    send(
      turn,
      None,
      "No encontré esa cita; no cambié ningún registro.",
    )
    .await?;
    return Ok(());
  };
  if appointment.contact_id != contact
    || appointment.revision != expected_revision
    || appointment.status != "confirmed"
    || appointment.attendance_confirmed_at.is_some()
    || appointment.starts_at <= now
  {
    idle(tx, contact, now).await?;
    send(
      turn,
      None,
      "La cita cambió o ya no está pendiente de confirmación de asistencia.",
    )
    .await?;
    return Ok(());
  }
  let revision = appointment.revision;
  tx.execute(sql(
    "UPDATE appointments SET attendance_confirmed_at=$2,updated_at=$2 WHERE id=$1",
    vec![id.into(), now.into()],
  ))
  .await?;
  store::history(tx, id, "Contact", "attendance_confirmed", now).await?;
  idle(tx, contact, now).await?;
  let text = format!("Gracias. Registré su confirmación de asistencia para {}. Si necesita cambiar o cancelar, escriba 2 o 3.", store::local_time(appointment.starts_at, &appointment.timezone));
  send(turn, Some((id, revision)), &text).await
}

fn attendance_reply(text: &str) -> Option<bool> {
  let normalized = text.trim().to_lowercase();
  match normalized.as_str() {
    "confirmo" | "confirmo asistencia" | "confirmar asistencia" | "si confirmo" | "sí confirmo" => {
      Some(true)
    }
    "no puedo asistir" | "no asistire" | "no asistiré" | "no podre asistir"
    | "no podré asistir" => Some(false),
    _ => None,
  }
}

async fn handle_receipt_selection(turn: &Turn<'_>, data: &Value, text: &str) -> Result<(), DbErr> {
  let Turn {
    tx,
    contact,
    origin,
    now,
    ..
  } = *turn;
  let Some(candidates) = data["candidates"].as_array() else {
    return Ok(());
  };
  let Some(selected) = numbered_json(candidates, text) else {
    send(
      turn,
      None,
      "Responda con el número de la cita a la que corresponde este comprobante.",
    )
    .await?;
    return Ok(());
  };
  let Some(receipt) = data["receipt_id"]
    .as_str()
    .and_then(|value| value.parse::<Uuid>().ok())
  else {
    return Ok(());
  };
  let Some(appointment) = selected["appointment_id"]
    .as_str()
    .and_then(|value| value.parse::<Uuid>().ok())
  else {
    return Ok(());
  };
  let _ = crate::modules::payments::intake::select_receipt(
    tx,
    contact,
    receipt,
    appointment,
    crate::modules::payments::intake::SelectionActor::Contact,
    origin,
    now,
  )
  .await?;
  Ok(())
}
