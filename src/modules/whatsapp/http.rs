use super::{
  inbox,
  views::{self, ConnectionState, ConnectionView},
};
use crate::app::AppState;
use crate::modules::doctor::DoctorSession;
use axum::{
  body::{to_bytes, Body},
  extract::{Request, State},
  http::StatusCode,
  response::{Html, IntoResponse, Redirect, Response},
  routing::{get, post},
  Extension, Form, Json, Router,
};
use serde::Deserialize;

pub fn dashboard_router() -> Router<AppState> {
  Router::new()
    .route("/settings/whatsapp", get(connection))
    .route("/settings/whatsapp/start", post(start))
    .route("/settings/whatsapp/request-code", post(request_code))
    .route("/settings/whatsapp/qr", get(qr))
}
fn page(
  session: &DoctorSession,
  state: ConnectionState,
  phone: Option<&str>,
  notice: Option<&str>,
  pairing_code: Option<&str>,
  pairing_phone: Option<&str>,
) -> Response {
  Html(views::connection(ConnectionView {
    csrf_token: &session.csrf,
    state,
    phone,
    notice,
    pairing_code,
    pairing_phone,
  }))
  .into_response()
}
async fn connection(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
) -> Response {
  let Some(client) = &state.waha else {
    return page(
      &session,
      ConnectionState::Unconfigured,
      None,
      None,
      None,
      None,
    );
  };
  match client.status().await {
    Ok(status) => page(
      &session,
      status.state,
      status.phone.as_deref(),
      None,
      None,
      None,
    ),
    Err(_) => page(
      &session,
      ConnectionState::Unavailable,
      None,
      None,
      None,
      None,
    ),
  }
}
async fn start(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
) -> Response {
  let Some(client) = &state.waha else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  match client.status().await {
    Ok(status) if status.state == ConnectionState::Stopped => {}
    Ok(status) => {
      return (
        StatusCode::CONFLICT,
        page(
          &session,
          status.state,
          status.phone.as_deref(),
          Some("Actualiza el estado antes de iniciar la conexión."),
          None,
          None,
        ),
      )
        .into_response()
    }
    Err(_) => {
      return (
        StatusCode::SERVICE_UNAVAILABLE,
        page(
          &session,
          ConnectionState::Unavailable,
          None,
          None,
          None,
          None,
        ),
      )
        .into_response()
    }
  }
  match client.start().await {
    Ok(()) => Redirect::to("/settings/whatsapp").into_response(),
    Err(_) => (
      StatusCode::SERVICE_UNAVAILABLE,
      page(
        &session,
        ConnectionState::Unavailable,
        None,
        Some("No se pudo iniciar la conexión. Actualiza el estado antes de volver a intentar."),
        None,
        None,
      ),
    )
      .into_response(),
  }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RequestCodeForm {
  phone: String,
  #[allow(dead_code)]
  csrf_token: String,
}

async fn request_code(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  form: Result<Form<RequestCodeForm>, axum::extract::rejection::FormRejection>,
) -> Response {
  let raw = form.map(|f| f.0.phone).unwrap_or_default();
  let stripped: String = raw.chars().filter(|c| *c != ' ' && *c != '-').collect();
  let digits = stripped.strip_prefix('+').unwrap_or(&stripped);
  let valid = (10..=15).contains(&digits.len())
    && digits.bytes().all(|b| b.is_ascii_digit())
    && !digits.starts_with('0');
  if !valid {
    return (
      StatusCode::UNPROCESSABLE_ENTITY,
      page(
        &session,
        ConnectionState::Qr,
        None,
        Some("Escribe el número con clave de país, solo dígitos (por ejemplo 521234567890)."),
        None,
        Some(&raw),
      ),
    )
      .into_response();
  }
  let Some(client) = &state.waha else {
    return (
      StatusCode::SERVICE_UNAVAILABLE,
      page(
        &session,
        ConnectionState::Unavailable,
        None,
        None,
        None,
        None,
      ),
    )
      .into_response();
  };
  let status = match client.status().await {
    Ok(status) => status,
    Err(_) => {
      return (
        StatusCode::SERVICE_UNAVAILABLE,
        page(
          &session,
          ConnectionState::Unavailable,
          None,
          None,
          None,
          None,
        ),
      )
        .into_response()
    }
  };
  match status.state {
    ConnectionState::Qr => {}
    ConnectionState::Stopped => {
      return (
        StatusCode::CONFLICT,
        page(
          &session,
          status.state,
          status.phone.as_deref(),
          Some("Primero inicia la conexión con el botón Conectar WhatsApp."),
          None,
          None,
        ),
      )
        .into_response()
    }
    _ => {
      return (
        StatusCode::CONFLICT,
        page(
          &session,
          status.state,
          status.phone.as_deref(),
          Some("Actualiza el estado antes de pedir un código de vinculación."),
          None,
          None,
        ),
      )
        .into_response()
    }
  }
  match client.request_code(digits).await {
    Ok(code) => page(
      &session,
      ConnectionState::Qr,
      status.phone.as_deref(),
      None,
      Some(&code),
      None,
    ),
    Err(_) => (
      StatusCode::SERVICE_UNAVAILABLE,
      page(
        &session,
        ConnectionState::Unavailable,
        None,
        Some("No se pudo pedir el código de vinculación. Actualiza el estado antes de volver a intentar."),
        None,
        None,
      ),
    )
      .into_response(),
  }
}
async fn qr(State(state): State<AppState>) -> Response {
  let Some(client) = &state.waha else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  match client.status().await {
    Ok(status) if status.state == ConnectionState::Qr => {}
    Ok(_) => return StatusCode::CONFLICT.into_response(),
    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
  }
  match client.qr().await {
    Ok(png) => ([("content-type", "image/png")], png).into_response(),
    Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
  }
}

pub fn webhook_router() -> Router<AppState> {
  Router::new().route("/api/v1/webhooks/waha", post(webhook))
}
async fn webhook(State(state): State<AppState>, request: Request<Body>) -> Response {
  let Some(config) = &state.cfg.waha else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  let (parts, body) = request.into_parts();
  let raw = match to_bytes(body, inbox::MAX_BODY).await {
    Ok(raw) => raw,
    Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
  };
  if !inbox::authenticate(&parts.headers, &raw, &config.hmac_key) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let envelope: serde_json::Value = match serde_json::from_slice(&raw) {
    Ok(value) => value,
    Err(_) => return StatusCode::BAD_REQUEST.into_response(),
  };
  if envelope["event"].as_str() == Some("message.ack") {
    use crate::modules::conversation::acknowledgements;
    return match acknowledgements::parse(&envelope) {
      Err(()) => StatusCode::BAD_REQUEST.into_response(),
      Ok(None) => Json(serde_json::json!({"outcome":"ignored"})).into_response(),
      Ok(Some(ack)) => {
        match acknowledgements::accept(&state.db, ack, state.clock.now_millis()).await {
          Ok(()) => Json(serde_json::json!({"outcome":"accepted"})).into_response(),
          Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
      }
    };
  }
  let now = state.clock.now_millis();
  match inbox::incoming(&raw) {
    Ok(Some(message)) => {
      let id = message.id.clone();
      let inserted = match inbox::accept(&state.db, message, raw.to_vec(), now).await {
        Ok(inserted) => inserted,
        Err(_) => {
          tracing::warn!("WAHA intake database unavailable");
          return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
      };
      if inbox::copy_pending_media(
        &state.db,
        state.waha.as_ref(),
        &id,
        now,
        state.cfg.public_origin.as_deref(),
      )
      .await
      .is_err()
      {
        tracing::warn!("WAHA Receipt media intake unavailable");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
      }
      Json(serde_json::json!({"outcome": if inserted {"accepted"} else {"duplicate"}}))
        .into_response()
    }
    Ok(None) => {
      use super::takeover::Outcome;
      match super::takeover::accept_phone_event(
        &state.db,
        &raw,
        state.cfg.waha_takeover_enabled,
        now,
      )
      .await
      {
        Ok(Some(Outcome::Accepted)) => {
          Json(serde_json::json!({"outcome":"accepted"})).into_response()
        }
        Ok(Some(Outcome::Duplicate)) => {
          Json(serde_json::json!({"outcome":"duplicate"})).into_response()
        }
        Ok(Some(Outcome::Ignored)) | Ok(None) => {
          Json(serde_json::json!({"outcome":"ignored"})).into_response()
        }
        Err(_) => {
          tracing::warn!("WAHA Takeover persistence unavailable");
          StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
      }
    }
    Err(()) => StatusCode::BAD_REQUEST.into_response(),
  }
}
