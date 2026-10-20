use super::{
  booking, store,
  views::{self, DetailView, HistoryEntry, ListItem, ListView, Period, ReceiptLink},
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
  Extension, Router,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};
use serde::Deserialize;
use uuid::Uuid;

const PAGE_SIZE: i64 = 25;

pub fn protected_router() -> Router<AppState> {
  Router::new()
    .route("/appointments", get(list))
    .route("/appointments/{id}", get(detail))
    .route("/appointments/{id}/cancel", post(cancel))
    .route("/appointments/{id}/reschedule", post(request_reschedule))
    .route("/appointments/{id}/refund/resolve", post(resolve_refund))
}

#[derive(Default, Deserialize)]
struct ListQuery {
  period: Option<String>,
  #[serde(default)]
  page: u32,
}

fn period(value: Option<&str>) -> Option<Period> {
  match value.unwrap_or("upcoming") {
    "upcoming" => Some(Period::Upcoming),
    "past" => Some(Period::Past),
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
  let Some(period) = period(query.period.as_deref()) else {
    return StatusCode::BAD_REQUEST.into_response();
  };
  let result = load_list(&state, period, query.page, state.clock.now_millis()).await;
  let load_error = result.is_err();
  let (items, next) = result.unwrap_or_default();
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
    })),
  )
    .into_response()
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
