use super::views::{self, Status};
use crate::{
  app::AppState,
  modules::{
    appointments::store::local_time,
    doctor::DoctorSession,
    whatsapp::{self, inbox::sql},
  },
};
use axum::{
  extract::{Path, State},
  http::StatusCode,
  response::{Html, IntoResponse, Redirect, Response},
  routing::{get, post},
  Extension, Router,
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
  Router::new()
    .route("/settings/mercado-pago", get(settings))
    .route(
      "/appointments/{id}/mercado-pago/{payment}/refund",
      post(refund),
    )
}

async fn settings(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
) -> Response {
  if state.mercado_pago.is_none() {
    return Html(views::settings(&session.csrf, Status::NotConfigured, None)).into_response();
  }
  let row = match state
    .db
    .conn
    .query_one(sql(
      "SELECT a.user_id,a.checked_at,a.notice,p.timezone FROM mercado_pago_account a LEFT JOIN practice p ON p.id=1 WHERE a.id=1",
      vec![],
    ))
    .await
  {
    Ok(row) => row,
    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
  };
  let Some(row) = row else {
    return Html(views::settings(&session.csrf, Status::Checking, None)).into_response();
  };
  let user_id: Option<i64> = row.try_get("", "user_id").unwrap_or(None);
  let notice: Option<String> = row.try_get("", "notice").unwrap_or(None);
  let checked_at: i64 = row.try_get("", "checked_at").unwrap_or_default();
  let timezone: String = row
    .try_get::<Option<String>>("", "timezone")
    .ok()
    .flatten()
    .unwrap_or_default();
  let status = match notice.as_deref() {
    Some("unauthorized") => Status::Unauthorized,
    Some(_) => Status::Unavailable,
    None if user_id.is_some() => Status::Ready,
    None => Status::Checking,
  };
  let checked = local_time(checked_at, &timezone);
  Html(views::settings(&session.csrf, status, Some(&checked))).into_response()
}

async fn refund(
  State(state): State<AppState>,
  Path((raw_id, raw_payment)): Path<(String, String)>,
) -> Response {
  let (Ok(id), Ok(payment)) = (Uuid::parse_str(&raw_id), raw_payment.parse::<i64>()) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  if state.mercado_pago.is_none() {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  }
  let Ok(tx) = state.db.conn.begin().await else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  if whatsapp::bound_transaction(&tx).await.is_err() {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  }
  let now = state.clock.now_millis();
  match super::store::request_refund(&tx, id, payment, now).await {
    Ok(true) if tx.commit().await.is_ok() => {
      Redirect::to(&format!("/appointments/{id}")).into_response()
    }
    // Not refundable (already requested, wrong Appointment, or not eligible): show the current state.
    Ok(false) => Redirect::to(&format!("/appointments/{id}")).into_response(),
    _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
  }
}
