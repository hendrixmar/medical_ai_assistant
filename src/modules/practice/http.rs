use super::{
  model::{self, Fields},
  store,
  views::{self, SettingsView, Step},
};
use crate::{app::AppState, modules::doctor::DoctorSession};
use axum::{
  extract::{Query, State},
  http::StatusCode,
  response::{Html, IntoResponse, Redirect, Response},
  routing::{get, post},
  Extension, Form, Router,
};
use serde::Deserialize;

pub fn router() -> Router<AppState> {
  Router::new()
    .route("/settings", get(settings))
    .route("/settings/details", post(details))
    .route("/settings/services", post(service))
    .route("/settings/schedule", post(schedule))
    .route("/settings/payments", post(payments))
    .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
}

#[derive(Default, Deserialize)]
struct Navigation {
  step: Option<String>,
  saved: Option<String>,
  service: Option<String>,
}

fn unavailable() -> Response {
  (
    StatusCode::SERVICE_UNAVAILABLE,
    "No pudimos leer o guardar la configuración. Inténtalo de nuevo; no se confirmó ningún cambio.",
  )
    .into_response()
}

async fn render(
  state: &AppState,
  session: &DoctorSession,
  step: Option<Step>,
  posted: Option<Fields>,
  errors: model::Errors,
  saved: bool,
  service_id: Option<&str>,
) -> Response {
  let Ok(practice) = store::load(&state.db).await else {
    return unavailable();
  };
  let Ok(services) = store::services(&state.db).await else {
    return unavailable();
  };
  let mut complete = practice.complete();
  complete[1] = !services.is_empty();
  let step = step.unwrap_or_else(|| {
    [
      Step::Details,
      Step::Services,
      Step::Schedule,
      Step::Payments,
    ]
    .into_iter()
    .zip(complete)
    .find(|(_, done)| !done)
    .map(|(step, _)| step)
    .unwrap_or(Step::Details)
  });
  let mut fields = practice.fields();
  let editing = match service_id {
    Some(id) if step == Step::Services => {
      let Ok(id) = id.parse::<uuid::Uuid>() else {
        return (StatusCode::NOT_FOUND, "Servicio no encontrado.").into_response();
      };
      let Some(service) = services.iter().find(|s| s.id == id) else {
        return (StatusCode::NOT_FOUND, "Servicio no encontrado.").into_response();
      };
      Some(service)
    }
    _ => None,
  };
  fields.insert("service_id".into(), uuid::Uuid::now_v7().to_string());
  if let Some(service) = editing {
    fields.extend(service.fields());
  }
  let editing_service = editing.is_some()
    || posted.as_ref().is_some_and(|fields| {
      services
        .iter()
        .any(|s| s.id.to_string() == model::value(fields, "service_id"))
    });
  // An omitted submitted field is blank, not the previously saved decision.
  if let Some(posted) = posted {
    fields = posted;
  }
  let service_views: Vec<_> = services
    .iter()
    .map(|s| views::ServiceView {
      id: s.id.to_string(),
      name: s.name.clone(),
      duration_minutes: s.duration_minutes.to_string(),
      price_mxn: s.price_mxn(),
    })
    .collect();
  let mut timezone_options: Vec<_> = chrono_tz::TZ_VARIANTS
    .iter()
    .map(|tz| (tz.name().to_owned(), tz.name().to_owned()))
    .collect();
  timezone_options.sort_by_key(|(name, _)| {
    (
      !matches!(
        name.as_str(),
        "America/Mexico_City"
          | "America/Tijuana"
          | "America/Cancun"
          | "America/Hermosillo"
          | "America/Mazatlan"
          | "America/Ciudad_Juarez"
      ),
      name.clone(),
    )
  });
  let example = model::value(&fields, "timezone")
    .parse::<chrono_tz::Tz>()
    .ok()
    .and_then(|tz| {
      chrono::DateTime::from_timestamp(state.clock.now(), 0).map(|now| {
        format!(
          "{} · {}",
          now.with_timezone(&tz).format("%Y-%m-%d %H:%M:%S %:z"),
          tz.name()
        )
      })
    });
  let status = if errors.is_empty() {
    StatusCode::OK
  } else {
    StatusCode::UNPROCESSABLE_ENTITY
  };
  (
    status,
    Html(views::settings(SettingsView {
      csrf_token: &session.csrf,
      step,
      fields: &fields,
      services: &service_views,
      editing_service,
      errors: &errors,
      message: saved.then_some("Configuración guardada."),
      complete,
      timezone_options: &timezone_options,
      timezone_example: example.as_deref(),
    })),
  )
    .into_response()
}

async fn settings(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Query(nav): Query<Navigation>,
) -> Response {
  let step = match nav.step.as_deref() {
    Some("details") => Some(Step::Details),
    Some("services") => Some(Step::Services),
    Some("schedule") => Some(Step::Schedule),
    Some("payments") => Some(Step::Payments),
    _ => None,
  };
  render(
    &state,
    &session,
    step,
    None,
    vec![],
    nav.saved.as_deref() == Some("1"),
    nav.service.as_deref(),
  )
  .await
}

async fn payments(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Form(fields): Form<Fields>,
) -> Response {
  match super::payments::Payments::parse(&fields) {
    Ok(payments) => match store::payments(&state.db, payments).await {
      Ok(()) => Redirect::to("/settings?saved=1").into_response(),
      Err(_) => unavailable(),
    },
    Err(errors) => {
      render(
        &state,
        &session,
        Some(Step::Payments),
        Some(fields),
        errors,
        false,
        None,
      )
      .await
    }
  }
}

async fn schedule(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Form(fields): Form<Fields>,
) -> Response {
  match super::schedule::Schedule::parse(&fields) {
    Ok(schedule) => match store::schedule(&state.db, schedule).await {
      Ok(()) => Redirect::to("/settings?step=payments&saved=1").into_response(),
      Err(_) => unavailable(),
    },
    Err(errors) => {
      render(
        &state,
        &session,
        Some(Step::Schedule),
        Some(fields),
        errors,
        false,
        None,
      )
      .await
    }
  }
}

async fn service(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Form(fields): Form<Fields>,
) -> Response {
  match model::Service::parse(&fields) {
    Ok(service) => match store::service(&state.db, service).await {
      Ok(()) => Redirect::to("/settings?step=services&saved=1").into_response(),
      Err(_) => unavailable(),
    },
    Err(errors) => {
      render(
        &state,
        &session,
        Some(Step::Services),
        Some(fields),
        errors,
        false,
        None,
      )
      .await
    }
  }
}

async fn details(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Form(fields): Form<Fields>,
) -> Response {
  match model::Details::parse(&fields) {
    Ok(details) => match store::details(&state.db, details).await {
      Ok(()) => Redirect::to("/settings?step=services&saved=1").into_response(),
      Err(_) => unavailable(),
    },
    Err(errors) => {
      render(
        &state,
        &session,
        Some(Step::Details),
        Some(fields),
        errors,
        false,
        None,
      )
      .await
    }
  }
}
