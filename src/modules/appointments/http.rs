use super::{
  booking::{self, Commit, Prepared, SelectionRequest},
  model::Format,
  store,
  views::{
    self, CalendarDayGroup, CalendarMode, CalendarSlotItem, CalendarView, ContactOption,
    CreateFormView, DetailView, HistoryEntry, ListItem, ListView, Period, ReceiptLink,
    ServiceOption,
  },
};
use crate::{
  app::AppState,
  modules::{
    doctor::DoctorSession,
    whatsapp::{self, inbox::sql},
  },
};
use axum::{
  extract::{Path, Query, State},
  http::StatusCode,
  response::{Html, IntoResponse, Redirect, Response},
  routing::{get, post},
  Extension, Form, Router,
};
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Utc};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};
use serde::Deserialize;
use uuid::Uuid;

const PAGE_SIZE: i64 = 25;

pub fn protected_router() -> Router<AppState> {
  Router::new()
    .route("/appointments", get(list))
    .route("/appointments/create", post(create))
    .route("/appointments/{id}", get(detail))
    .route("/appointments/{id}/cancel", post(cancel))
    .route("/appointments/{id}/reschedule", post(request_reschedule))
    .route("/appointments/{id}/refund/resolve", post(resolve_refund))
}

#[derive(Default, Deserialize)]
struct ListQuery {
  period: Option<String>,
  view: Option<String>,
  date: Option<String>,
  #[serde(default)]
  page: u32,
}

#[derive(Deserialize)]
struct CreateForm {
  patient_name: String,
  #[serde(default)]
  contact_id: String,
  #[serde(default)]
  contact_phone: String,
  service_id: String,
  format: String,
  date: String,
  time: String,
}

fn period(value: Option<&str>) -> Option<Period> {
  match value.unwrap_or("upcoming") {
    "upcoming" => Some(Period::Upcoming),
    "past" => Some(Period::Past),
    _ => None,
  }
}

fn calendar_mode(value: Option<&str>) -> Option<Option<CalendarMode>> {
  match value {
    None | Some("list") => Some(None),
    Some("day") => Some(Some(CalendarMode::Day)),
    Some("week") => Some(Some(CalendarMode::Week)),
    Some("month") => Some(Some(CalendarMode::Month)),
    _ => None,
  }
}

fn local_time(timestamp: i64, timezone: &str) -> Result<String, DbErr> {
  let zone = timezone
    .parse::<chrono_tz::Tz>()
    .map_err(|_| DbErr::Custom("Invalid Appointment timezone".into()))?;
  DateTime::<Utc>::from_timestamp_millis(timestamp)
    .map(|time| {
      time
        .with_timezone(&zone)
        .format("%Y-%m-%d %H:%M %Z")
        .to_string()
    })
    .ok_or_else(|| DbErr::Custom("Invalid Appointment timestamp".into()))
}

fn appointment_status(value: &str) -> &'static str {
  match value {
    "requested" => "Solicitud sin confirmar",
    "awaiting_payment" => "Pendiente de pago",
    "confirmed" => "Confirmada",
    "cancelled" => "Cancelada",
    "expired" => "Vencida",
    _ => "Estado no reconocido",
  }
}

fn payment_status(value: &str) -> &'static str {
  match value {
    "not_required" => "No requiere pago",
    "awaiting_receipt" => "Pendiente de comprobante",
    "under_review" => "Comprobante en revisión",
    "verified" => "Pago verificado por el doctor",
    "rejected" => "Comprobante rechazado",
    _ => "Estado de pago no reconocido",
  }
}

fn calendar_status(value: &str) -> &'static str {
  match value {
    "not_yet" => "Sin sincronización",
    "pending" => "Sincronización pendiente",
    "synced" => "Sincronizado",
    "failed" => "Sincronización con error",
    _ => "Estado de calendario no reconocido",
  }
}

fn appointment_format(value: &str) -> &'static str {
  match value {
    "in_person" => "Presencial",
    "video" => "Videollamada",
    _ => "Formato no reconocido",
  }
}

async fn load_list(
  state: &AppState,
  period: Period,
  page: u32,
  now: i64,
) -> Result<(Vec<ListItem>, bool), DbErr> {
  let query = match period {
    Period::Upcoming => "SELECT id,patient_name,service_name,format,starts_at,timezone,status,payment,calendar_sync FROM appointments WHERE starts_at >= $1 AND status NOT IN ('cancelled','expired') ORDER BY starts_at,id LIMIT 26 OFFSET $2",
    Period::Past => "SELECT id,patient_name,service_name,format,starts_at,timezone,status,payment,calendar_sync FROM appointments WHERE starts_at < $1 OR status IN ('cancelled','expired') ORDER BY starts_at DESC,id DESC LIMIT 26 OFFSET $2",
  };
  let rows = state
    .db
    .conn
    .query_all(sql(
      query,
      vec![now.into(), (i64::from(page) * PAGE_SIZE).into()],
    ))
    .await?;
  let next_page = rows.len() > PAGE_SIZE as usize;
  let mut items = Vec::with_capacity(rows.len().min(PAGE_SIZE as usize));
  for row in rows.into_iter().take(PAGE_SIZE as usize) {
    let timezone: String = row.try_get("", "timezone")?;
    items.push(ListItem {
      id: row.try_get::<Uuid>("", "id")?.to_string(),
      patient_name: row.try_get("", "patient_name")?,
      service_name: row.try_get("", "service_name")?,
      format: appointment_format(&row.try_get::<String>("", "format")?).to_owned(),
      starts_at: local_time(row.try_get("", "starts_at")?, &timezone)?,
      status: appointment_status(&row.try_get::<String>("", "status")?).to_owned(),
      payment: payment_status(&row.try_get::<String>("", "payment")?).to_owned(),
      calendar_sync: calendar_status(&row.try_get::<String>("", "calendar_sync")?).to_owned(),
    });
  }
  Ok((items, next_page))
}

async fn list(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Query(query): Query<ListQuery>,
) -> Response {
  let Some(mode) = calendar_mode(query.view.as_deref()) else {
    return StatusCode::BAD_REQUEST.into_response();
  };
  if let Some(mode) = mode {
    return render_calendar(&state, &session, mode, query.date.as_deref(), None).await;
  }
  let Some(period) = period(query.period.as_deref()) else {
    return StatusCode::BAD_REQUEST.into_response();
  };
  let result = load_list(&state, period, query.page, state.clock.now_millis()).await;
  let load_error = result.is_err();
  let (items, next) = result.unwrap_or_default();
  let form_data = if load_error {
    None
  } else {
    load_create_options(&state).await.ok()
  };
  let create_form = form_data
    .as_ref()
    .map(|(services, contacts, timezone)| CreateFormView {
      services,
      contacts,
      timezone: timezone.as_deref(),
      error: None,
    });
  (
    if load_error {
      StatusCode::SERVICE_UNAVAILABLE
    } else {
      StatusCode::OK
    },
    Html(views::appointment_list(ListView {
      csrf_token: &session.csrf,
      period,
      page: query.page,
      items: &items,
      previous_page: query.page.checked_sub(1),
      next_page: next.then(|| query.page.saturating_add(1)),
      load_error,
      create_form,
    })),
  )
    .into_response()
}

async fn load_create_options(
  state: &AppState,
) -> Result<(Vec<ServiceOption>, Vec<ContactOption>, Option<String>), DbErr> {
  let practice = crate::modules::practice::store::load(&state.db).await?;
  let services = crate::modules::practice::store::services(&state.db).await?;
  let service_options = services
    .into_iter()
    .map(|svc| ServiceOption {
      id: svc.id.to_string(),
      label: format!(
        "{} — {} minutos — ${} MXN",
        svc.name,
        svc.duration_minutes,
        svc.price_mxn()
      ),
    })
    .collect();
  let contact_rows = state
    .db
    .conn
    .query_all(sql(
      "SELECT c.id,COALESCE((SELECT a.jid FROM contact_aliases a WHERE a.contact_id=c.id ORDER BY a.jid LIMIT 1),c.id::text) AS label FROM contacts c ORDER BY c.created_at DESC,c.id DESC LIMIT 100",
      vec![],
    ))
    .await?;
  let mut contacts = Vec::with_capacity(contact_rows.len());
  for row in contact_rows {
    let id: Uuid = row.try_get("", "id")?;
    let label: String = row.try_get("", "label")?;
    contacts.push(ContactOption {
      id: id.to_string(),
      label,
    });
  }
  Ok((service_options, contacts, practice.timezone))
}

fn local_day_start_millis(date: NaiveDate, tz: chrono_tz::Tz) -> i64 {
  let midnight = date.and_time(NaiveTime::MIN);
  for minute in 0..24 * 60 {
    if let Some(local) = midnight.checked_add_signed(chrono::Duration::minutes(minute)) {
      match tz.from_local_datetime(&local) {
        chrono::LocalResult::Single(value) if value.date_naive() == date => {
          return value.timestamp_millis();
        }
        chrono::LocalResult::Ambiguous(first, second)
          if first.date_naive() == date && second.date_naive() == date =>
        {
          return first.timestamp_millis().min(second.timestamp_millis());
        }
        _ => {}
      }
    }
  }
  date
    .and_hms_opt(0, 0, 0)
    .unwrap()
    .and_utc()
    .timestamp_millis()
}

async fn render_calendar(
  state: &AppState,
  session: &DoctorSession,
  mode: CalendarMode,
  date_param: Option<&str>,
  error: Option<&str>,
) -> Response {
  let Ok((services, contacts, configured_tz)) = load_create_options(state).await else {
    return (
      StatusCode::SERVICE_UNAVAILABLE,
      Html(views::load_error(&session.csrf, None)),
    )
      .into_response();
  };
  let tz_name = configured_tz.as_deref().unwrap_or("UTC");
  let tz = tz_name.parse::<chrono_tz::Tz>().unwrap_or(chrono_tz::UTC);
  let default_date = DateTime::<Utc>::from_timestamp_millis(state.clock.now_millis())
    .map(|dt| dt.with_timezone(&tz).date_naive())
    .unwrap_or_else(|| NaiveDate::from_ymd_opt(2027, 1, 15).unwrap());
  let anchor = match date_param {
    Some(raw) => {
      let Ok(parsed) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") else {
        return StatusCode::BAD_REQUEST.into_response();
      };
      parsed
    }
    None => default_date,
  };
  let (start_date, end_date, prev_date, next_date, range_label) = match mode {
    CalendarMode::Day => (
      anchor,
      anchor,
      anchor - chrono::Duration::days(1),
      anchor + chrono::Duration::days(1),
      anchor.format("%Y-%m-%d").to_string(),
    ),
    CalendarMode::Week => {
      let offset = i64::from(anchor.weekday().num_days_from_monday());
      let monday = anchor - chrono::Duration::days(offset);
      let sunday = monday + chrono::Duration::days(6);
      (
        monday,
        sunday,
        monday - chrono::Duration::days(7),
        monday + chrono::Duration::days(7),
        format!(
          "{} a {}",
          monday.format("%Y-%m-%d"),
          sunday.format("%Y-%m-%d")
        ),
      )
    }
    CalendarMode::Month => {
      let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1).unwrap_or(anchor);
      let next_month = if anchor.month() == 12 {
        NaiveDate::from_ymd_opt(anchor.year() + 1, 1, 1).unwrap_or(anchor)
      } else {
        NaiveDate::from_ymd_opt(anchor.year(), anchor.month() + 1, 1).unwrap_or(anchor)
      };
      let last = next_month - chrono::Duration::days(1);
      let prev_month = if anchor.month() == 1 {
        NaiveDate::from_ymd_opt(anchor.year() - 1, 1, 1).unwrap_or(anchor)
      } else {
        NaiveDate::from_ymd_opt(anchor.year(), anchor.month() - 1, 1).unwrap_or(anchor)
      };
      (
        first,
        last,
        prev_month,
        next_month,
        first.format("%Y-%m").to_string(),
      )
    }
  };
  let window_start = local_day_start_millis(start_date, tz);
  let window_end = local_day_start_millis(end_date + chrono::Duration::days(1), tz);
  let Ok(rows) = state
    .db
    .conn
    .query_all(sql(
      "SELECT a.id,a.patient_name,a.service_name,a.format,a.starts_at,a.ends_at,a.status,EXISTS(SELECT 1 FROM appointment_holds h WHERE h.appointment_id=a.id AND h.active) AS hold_active FROM appointments a WHERE a.starts_at < $2 AND a.ends_at > $1 ORDER BY a.starts_at,a.id",
      vec![window_start.into(), window_end.into()],
    ))
    .await
  else {
    return (
      StatusCode::SERVICE_UNAVAILABLE,
      Html(views::load_error(&session.csrf, None)),
    )
      .into_response();
  };
  let mut days = Vec::new();
  let mut cursor = start_date;
  while cursor <= end_date {
    let mut day_items = Vec::new();
    for row in &rows {
      let starts_at: i64 = row.try_get("", "starts_at").unwrap_or_default();
      let ends_at: i64 = row.try_get("", "ends_at").unwrap_or_default();
      let Some(start_local) =
        DateTime::<Utc>::from_timestamp_millis(starts_at).map(|dt| dt.with_timezone(&tz))
      else {
        continue;
      };
      if start_local.date_naive() != cursor {
        continue;
      }
      let end_str = DateTime::<Utc>::from_timestamp_millis(ends_at)
        .map(|dt| dt.with_timezone(&tz).format("%H:%M").to_string())
        .unwrap_or_default();
      let id: Uuid = row.try_get("", "id").unwrap_or_default();
      let format_code: String = row.try_get("", "format").unwrap_or_default();
      let status_code: String = row.try_get("", "status").unwrap_or_default();
      day_items.push(CalendarSlotItem {
        id: id.to_string(),
        patient_name: row.try_get("", "patient_name").unwrap_or_default(),
        service_name: row.try_get("", "service_name").unwrap_or_default(),
        format: appointment_format(&format_code).to_owned(),
        time_range: format!("{}-{}", start_local.format("%H:%M"), end_str),
        status: appointment_status(&status_code).to_owned(),
        hold_active: row.try_get("", "hold_active").unwrap_or(false),
      });
    }
    days.push(CalendarDayGroup {
      date_label: cursor.format("%Y-%m-%d").to_string(),
      items: day_items,
    });
    cursor += chrono::Duration::days(1);
  }
  let status = if error.is_some() {
    StatusCode::UNPROCESSABLE_ENTITY
  } else {
    StatusCode::OK
  };
  (
    status,
    Html(views::appointment_calendar(CalendarView {
      csrf_token: &session.csrf,
      mode,
      anchor_date: &anchor.format("%Y-%m-%d").to_string(),
      prev_date: &prev_date.format("%Y-%m-%d").to_string(),
      next_date: &next_date.format("%Y-%m-%d").to_string(),
      range_label: &range_label,
      timezone: tz_name,
      days: &days,
      create_form: CreateFormView {
        services: &services,
        contacts: &contacts,
        timezone: configured_tz.as_deref(),
        error,
      },
    })),
  )
    .into_response()
}

async fn create(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Form(form): Form<CreateForm>,
) -> Response {
  let Some(origin) = state.cfg.public_origin.as_deref() else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  if !booking::valid_patient(Some(&form.patient_name)) {
    return render_calendar(
      &state,
      &session,
      CalendarMode::Day,
      None,
      Some("Escriba un nombre de paciente válido de hasta 120 caracteres."),
    )
    .await;
  }
  let Ok(service_id) = Uuid::parse_str(form.service_id.trim()) else {
    return render_calendar(
      &state,
      &session,
      CalendarMode::Day,
      None,
      Some("Elija un servicio válido del consultorio."),
    )
    .await;
  };
  let Some(format) = Format::from_db(form.format.trim()) else {
    return render_calendar(
      &state,
      &session,
      CalendarMode::Day,
      None,
      Some("Elija un formato válido (presencial o videollamada)."),
    )
    .await;
  };
  let Ok(practice) = crate::modules::practice::store::load(&state.db).await else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  let Some(tz) = practice
    .timezone
    .as_deref()
    .and_then(|name| name.parse::<chrono_tz::Tz>().ok())
  else {
    return render_calendar(
      &state,
      &session,
      CalendarMode::Day,
      None,
      Some("Configure primero la zona horaria del consultorio."),
    )
    .await;
  };
  let Ok(date) = NaiveDate::parse_from_str(form.date.trim(), "%Y-%m-%d") else {
    return render_calendar(
      &state,
      &session,
      CalendarMode::Day,
      None,
      Some("Indique la fecha en formato AAAA-MM-DD."),
    )
    .await;
  };
  let Ok(time) = NaiveTime::parse_from_str(form.time.trim(), "%H:%M") else {
    return render_calendar(
      &state,
      &session,
      CalendarMode::Day,
      Some(form.date.trim()),
      Some("Indique la hora en formato HH:MM."),
    )
    .await;
  };
  let Some(local_dt) = tz.from_local_datetime(&date.and_time(time)).earliest() else {
    return render_calendar(
      &state,
      &session,
      CalendarMode::Day,
      Some(form.date.trim()),
      Some("La hora indicada no es válida en la zona horaria del consultorio."),
    )
    .await;
  };
  let starts_at = local_dt.timestamp_millis();
  let now = state.clock.now_millis();
  let contact = match resolve_or_create_contact(
    &state,
    form.contact_id.trim(),
    form.contact_phone.trim(),
    now,
  )
  .await
  {
    Ok(Some(id)) => id,
    Ok(None) => {
      return render_calendar(
        &state,
        &session,
        CalendarMode::Day,
        Some(form.date.trim()),
        Some("Seleccione un contacto existente o indique un teléfono de WhatsApp válido (solo dígitos)."),
      )
      .await;
    }
    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
  };
  let prepared = match booking::prepare_selection(
    &state.db,
    state.clock.as_ref(),
    &state.cfg,
    state.google.as_ref(),
    SelectionRequest {
      contact,
      service_id,
      format,
      starts_at,
      appointment_id: None,
      expected_revision: None,
      late_recovery: false,
    },
    Some(&form.patient_name),
  )
  .await
  {
    Ok(prepared) => prepared,
    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
  };
  let Prepared::Ready(preflight) = prepared else {
    return render_calendar(
      &state,
      &session,
      CalendarMode::Day,
      Some(form.date.trim()),
      Some("El horario no está disponible según las reglas del consultorio, las retenciones activas o Google Calendar."),
    )
    .await;
  };
  let Ok(tx) = state.db.conn.begin().await else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  if whatsapp::bound_transaction(&tx).await.is_err() {
    let _ = tx.rollback().await;
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  }
  match booking::commit_selection_for_actor(
    &tx,
    preflight,
    None,
    "Doctor",
    Some(&form.patient_name),
    origin,
    now,
  )
  .await
  {
    Ok(Commit::Applied { appointment_id, .. }) => {
      if tx.commit().await.is_err() {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
      } else {
        Redirect::to(&format!("/appointments/{appointment_id}")).into_response()
      }
    }
    Ok(Commit::OpenLimit) => {
      let _ = tx.rollback().await;
      render_calendar(
        &state,
        &session,
        CalendarMode::Day,
        Some(form.date.trim()),
        Some("El contacto ya tiene tres citas abiertas."),
      )
      .await
    }
    Ok(Commit::Stale | Commit::Followup) => {
      let _ = tx.rollback().await;
      render_calendar(
        &state,
        &session,
        CalendarMode::Day,
        Some(form.date.trim()),
        Some("El horario cambió o ya fue ocupado; no se creó la cita."),
      )
      .await
    }
    Err(_) => {
      let _ = tx.rollback().await;
      StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
  }
}

async fn resolve_or_create_contact(
  state: &AppState,
  contact_id: &str,
  contact_phone: &str,
  now: i64,
) -> Result<Option<Uuid>, DbErr> {
  if !contact_id.is_empty() {
    let Ok(id) = Uuid::parse_str(contact_id) else {
      return Ok(None);
    };
    let exists = state
      .db
      .conn
      .query_one(sql("SELECT id FROM contacts WHERE id=$1", vec![id.into()]))
      .await?
      .is_some();
    return Ok(exists.then_some(id));
  }
  let digits: String = contact_phone
    .chars()
    .filter(|ch| ch.is_ascii_digit())
    .collect();
  if !(8..=20).contains(&digits.len()) {
    return Ok(None);
  }
  let jid = format!("{digits}@c.us");
  if let Some(row) = state
    .db
    .conn
    .query_one(sql(
      "SELECT contact_id FROM contact_aliases WHERE jid=$1",
      vec![jid.into()],
    ))
    .await?
  {
    return Ok(Some(row.try_get("", "contact_id")?));
  }
  let id = Uuid::now_v7();
  state
    .db
    .conn
    .execute(sql(
      "INSERT INTO contacts(id,created_at) VALUES ($1,$2)",
      vec![id.into(), now.into()],
    ))
    .await?;
  Ok(Some(id))
}

async fn load_detail(
  state: &AppState,
  appointment_id: Uuid,
  now: i64,
) -> Result<Option<DetailView>, DbErr> {
  let Some(appointment) = store::get(&state.db.conn, appointment_id).await? else {
    return Ok(None);
  };
  let timezone = appointment.timezone.as_str();
  let receipt_rows = state
    .db
    .conn
    .query_all(sql(
      "SELECT id,review_outcome,reviewed_at,timely,late,sent_at,arrived_at,(deleted_at IS NULL AND bytes IS NOT NULL) AS file_available FROM receipts WHERE appointment_id=$1 AND contact_id=$2 ORDER BY created_at DESC,id DESC",
      vec![appointment_id.into(), appointment.contact_id.into()],
    ))
    .await?;
  let mut receipts = Vec::with_capacity(receipt_rows.len());
  for row in receipt_rows {
    let outcome = row.try_get::<Option<String>>("", "review_outcome")?;
    let reviewed_at = row.try_get::<Option<i64>>("", "reviewed_at")?;
    let timely = row.try_get::<bool>("", "timely")?;
    let late = row.try_get::<bool>("", "late")?;
    let status = receipt_status(outcome.as_deref(), reviewed_at.is_some(), timely, late);
    receipts.push(ReceiptLink {
      id: row.try_get::<Uuid>("", "id")?.to_string(),
      status: status.to_owned(),
      sent_at: local_time(row.try_get("", "sent_at")?, timezone)?,
      arrived_at: local_time(row.try_get("", "arrived_at")?, timezone)?,
      file_available: row.try_get("", "file_available")?,
    });
  }
  let history_rows = state
    .db
    .conn
    .query_all(sql(
      "SELECT actor,kind,created_at FROM appointment_history WHERE appointment_id=$1 ORDER BY created_at DESC,id DESC",
      vec![appointment_id.into()],
    ))
    .await?;
  let mut history = Vec::with_capacity(history_rows.len());
  for row in history_rows {
    let actor = match row.try_get::<String>("", "actor")?.as_str() {
      "Doctor" => "Doctor",
      "Contact" => "Contacto",
      "System" => "Sistema",
      _ => "Actor no reconocido",
    };
    let kind: String = row.try_get("", "kind")?;
    history.push(HistoryEntry {
      actor: actor.to_owned(),
      action: history_action(&kind).to_owned(),
      occurred_at: local_time(row.try_get("", "created_at")?, timezone)?,
    });
  }
  let active_upcoming = appointment.starts_at > now
    && matches!(
      appointment.status.as_str(),
      "requested" | "awaiting_payment" | "confirmed"
    );
  let price = if appointment.price_centavos < 0 {
    return Err(DbErr::Custom("Invalid Appointment price".into()));
  } else {
    format!(
      "${}.{:02} MXN",
      appointment.price_centavos / 100,
      appointment.price_centavos % 100
    )
  };
  let can_resolve_refund = matches!(appointment.status.as_str(), "cancelled" | "expired")
    && appointment.payment == "verified"
    && state.db.conn.query_one(sql(
      "SELECT EXISTS(SELECT FROM attention_items WHERE appointment_id=$1 AND kind='refund_due' AND resolved_at IS NULL) AS pending",
      vec![appointment_id.into()],
    )).await?.ok_or_else(|| DbErr::Custom("Refund state unavailable".into()))?.try_get::<bool>("", "pending")?;
  Ok(Some(DetailView {
    id: appointment.id.to_string(),
    contact_id: appointment.contact_id.to_string(),
    patient_name: appointment.patient_name,
    service_name: appointment.service_name,
    format: appointment_format(&appointment.format).to_owned(),
    starts_at: local_time(appointment.starts_at, timezone)?,
    ends_at: local_time(appointment.ends_at, timezone)?,
    duration: format!("{} minutos", appointment.duration_minutes),
    price,
    location: (!appointment.location.is_empty()).then_some(appointment.location),
    status: appointment_status(&appointment.status).to_owned(),
    payment: payment_status(&appointment.payment).to_owned(),
    calendar_sync: calendar_status(&appointment.calendar_sync).to_owned(),
    receipt_deadline: appointment
      .receipt_deadline
      .map(|time| local_time(time, timezone))
      .transpose()?,
    meet_url: appointment.meet_url.filter(|url| !url.is_empty()),
    attendance_confirmed_at: appointment
      .attendance_confirmed_at
      .map(|time| local_time(time, timezone))
      .transpose()?,
    can_cancel: active_upcoming,
    can_reschedule: active_upcoming,
    can_resolve_refund,
    receipts,
    history,
  }))
}

fn receipt_status(outcome: Option<&str>, reviewed: bool, timely: bool, late: bool) -> &'static str {
  if reviewed {
    match outcome {
      Some("verified") => "Pago verificado por el doctor",
      Some("rejected") => "Comprobante rechazado",
      Some("new_receipt") => "Se solicitó un nuevo comprobante",
      Some("cancelled") => "Cita cancelada tras la revisión",
      Some("late_verified_recovery") => "Pago tardío verificado; se eligió recuperar la cita",
      Some("late_verified_refund") => "Pago tardío verificado; se requiere gestionar el reembolso",
      Some("late_clarification") => "Se solicitó aclarar el comprobante tardío",
      _ => "Decisión registrada",
    }
  } else if late || !timely {
    "Comprobante tardío en revisión"
  } else {
    "Comprobante pendiente de revisión"
  }
}

fn history_action(kind: &str) -> &'static str {
  match kind {
    "appointment_requested" => "Solicitud de cita registrada",
    "video_payment_requested" => "Solicitud de videollamada con pago registrada",
    "appointment_created" => "Cita registrada",
    "rescheduled" => "Cita reprogramada",
    "late_receipt_recovery_rescheduled" => {
      "Cita reprogramada durante la recuperación de pago tardío"
    }
    "reschedule_requested" => "El doctor pidió reprogramar la cita",
    "cancelled" => "Cita cancelada",
    "expired" => "Cita vencida",
    "calendar_availability_recovered" => "Disponibilidad de calendario recuperada",
    "calendar_cancelled" => "Cita retirada del calendario",
    "calendar_sync_failed" => "Falló la sincronización del calendario",
    "calendar_synced" => "Cita sincronizada con Google Calendar",
    "meet_ready" => "Enlace de videollamada preparado",
    "receipt_received" => "Comprobante recibido",
    "receipt_late_received" => "Comprobante tardío recibido",
    "receipt_associated" => "Comprobante asociado a esta cita",
    "payment_verified" => "Pago verificado por el doctor",
    "payment_rejected" => "Comprobante rechazado",
    "receipt_new_receipt_requested" => "Se solicitó un nuevo comprobante",
    "late_receipt_verified_recovery" => "Se eligió recuperar la cita con el pago tardío",
    "late_receipt_verified_refund" => "Se requiere gestionar el reembolso del pago tardío",
    "late_receipt_clarification_requested" => "Se solicitó aclarar el comprobante tardío",
    "refund_resolved" => "Reembolso registrado como resuelto por el doctor",
    _ => "Cambio registrado",
  }
}

async fn detail(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Path(id): Path<String>,
) -> Response {
  let Ok(id) = Uuid::parse_str(&id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  match load_detail(&state, id, state.clock.now_millis()).await {
    Ok(Some(view)) => Html(views::appointment_detail(&session.csrf, &view)).into_response(),
    Ok(None) => StatusCode::NOT_FOUND.into_response(),
    Err(_) => (
      StatusCode::SERVICE_UNAVAILABLE,
      Html(views::load_error(&session.csrf, Some(&id.to_string()))),
    )
      .into_response(),
  }
}

enum AppointmentAction {
  Cancel,
  Reschedule,
  ResolveRefund,
}

async fn cancel(State(state): State<AppState>, Path(id): Path<String>) -> Response {
  apply_change(state, id, AppointmentAction::Cancel).await
}

async fn request_reschedule(State(state): State<AppState>, Path(id): Path<String>) -> Response {
  apply_change(state, id, AppointmentAction::Reschedule).await
}

async fn resolve_refund(State(state): State<AppState>, Path(id): Path<String>) -> Response {
  apply_change(state, id, AppointmentAction::ResolveRefund).await
}

async fn apply_change(state: AppState, raw_id: String, action: AppointmentAction) -> Response {
  let Ok(id) = Uuid::parse_str(&raw_id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let Some(origin) = state.cfg.public_origin.as_deref() else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  let tx = match state.db.conn.begin().await {
    Ok(tx) => tx,
    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
  };
  if whatsapp::bound_transaction(&tx).await.is_err() {
    let _ = tx.rollback().await;
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  }
  let now = state.clock.now_millis();
  let result = match action {
    AppointmentAction::Cancel => store::cancel(&tx, id, "Doctor", origin, now).await,
    AppointmentAction::Reschedule => booking::request_reschedule(&tx, id, origin, now).await,
    AppointmentAction::ResolveRefund => store::resolve_refund(&tx, id, now).await,
  };
  match result {
    Ok(true) => {
      if tx.commit().await.is_err() {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
      } else {
        Redirect::to(&format!("/appointments/{id}")).into_response()
      }
    }
    Ok(false) => {
      let exists = tx
        .query_one(sql(
          "SELECT id FROM appointments WHERE id=$1",
          vec![id.into()],
        ))
        .await;
      let _ = tx.rollback().await;
      match exists {
        Ok(Some(_)) => Redirect::to(&format!("/appointments/{id}")).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
      }
    }
    Err(_) => {
      let _ = tx.rollback().await;
      StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
  }
}
