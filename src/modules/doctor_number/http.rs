use super::{
  store,
  views::{self, ChallengeView, DoctorNumberView},
  Error,
};
use crate::{
  app::AppState,
  modules::{doctor::DoctorSession, whatsapp::views::ConnectionState},
};
use axum::{
  extract::State,
  http::StatusCode,
  response::{Html, IntoResponse, Redirect, Response},
  routing::{get, post},
  Extension, Router,
};

const PATH: &str = "/settings/doctor-number";
pub fn router() -> Router<AppState> {
  Router::new()
    .route(PATH, get(show))
    .route("/settings/doctor-number/challenge", post(issue))
}
async fn practice(state: &AppState) -> Option<String> {
  let status = state.waha.as_ref()?.status().await.ok()?;
  (status.state == ConnectionState::Working)
    .then_some(status.phone)
    .flatten()
}
async fn show(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
) -> Response {
  let phone = practice(&state).await;
  page(&state, &session, phone.as_deref(), StatusCode::OK, None).await
}
async fn page(
  state: &AppState,
  session: &DoctorSession,
  phone: Option<&str>,
  status: StatusCode,
  notice: Option<&str>,
) -> Response {
  let result = match state.cfg.doctor_totp_key.as_ref() {
    Some(key) => store::load(&state.db, state.clock.as_ref(), key).await,
    None => Err(Error::Unavailable),
  };
  let data = result.as_ref().ok();
  let retry = data.and_then(|d| d.retry).map(|s|format!("Espera {s} segundos antes de generar otro código. El límite también se conserva al reiniciar."));
  let block = if phone.is_none() {
    Some("Se necesita una conexión de WhatsApp activa con el número del consultorio resuelto. Actualiza el estado antes de volver a intentar.")
  } else {
    retry.as_deref()
  };
  let challenge = match data {
    Some(d) if d.state == "active" => match (&d.code, &d.practice_phone, &d.expires_at) {
      (Some(code), Some(practice_phone), Some(expires_at)) => ChallengeView::Active {
        code,
        practice_phone,
        expires_at,
      },
      _ => ChallengeView::None,
    },
    Some(d) if d.state == "expired" => ChallengeView::Expired,
    Some(d) if d.state == "exhausted" => ChallengeView::Exhausted,
    Some(d) if d.state == "used" => ChallengeView::Used,
    _ => ChallengeView::None,
  };
  (
    if result.is_err() {
      StatusCode::SERVICE_UNAVAILABLE
    } else {
      status
    },
    Html(views::doctor_number(DoctorNumberView {
      csrf_token: &session.csrf,
      verified_phone: data.and_then(|d| d.verified_phone.as_deref()),
      verified_at: data.and_then(|d| d.verified_at.as_deref()),
      practice_phone: phone,
      challenge,
      issue_block: block,
      notice,
      load_error: result.is_err(),
    })),
  )
    .into_response()
}
async fn issue(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
) -> Response {
  // Shared authenticated status GET precedes every SQL transaction.
  let phone = practice(&state).await;
  let Some(phone) = phone else {
    return page(
      &state,
      &session,
      None,
      StatusCode::SERVICE_UNAVAILABLE,
      Some("No se generó ningún código. La conexión no pudo confirmar el número del consultorio."),
    )
    .await;
  };
  let Some(key) = state.cfg.doctor_totp_key.as_ref() else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  let jid = format!("{}@c.us", phone.trim_start_matches('+'));
  match store::issue(&state.db, state.clock.as_ref(), key, &jid).await {
    Ok(()) => Redirect::to(PATH).into_response(),
    Err(Error::Limited(seconds)) => {
      let mut response = page(
        &state,
        &session,
        Some(&phone),
        StatusCode::TOO_MANY_REQUESTS,
        Some("No se generó otro código: se alcanzó el límite de solicitudes."),
      )
      .await;
      response
        .headers_mut()
        .insert("retry-after", seconds.to_string().parse().unwrap());
      response
    }
    Err(_) => {
      page(
        &state,
        &session,
        Some(&phone),
        StatusCode::SERVICE_UNAVAILABLE,
        Some("No se pudo generar el código. Actualiza el estado antes de intentar de nuevo."),
      )
      .await
    }
  }
}
