use super::{
  store,
  views::{self, CheckView, ConnectionView, GoogleView, PermissionView},
  Error,
};
use crate::{app::AppState, modules::doctor::DoctorSession};
use axum::{
  extract::{Path, RawQuery, State},
  http::{Method, StatusCode},
  response::{Html, IntoResponse, Redirect, Response},
  routing::{get, post},
  Extension, Router,
};

const PATH: &str = "/settings/google";
pub fn router() -> Router<AppState> {
  Router::new()
    .route(PATH, get(show))
    .route("/settings/google/connect", post(connect))
    .route("/settings/google/callback", get(callback))
    .route("/settings/google/callback/", get(callback))
    .route("/settings/google/calendars", post(select))
    .route("/settings/google/probe", post(probe))
    .route("/settings/google/probes/{id}/cleanup", post(cleanup_retry))
    .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
}
async fn connect(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  body: String,
) -> Response {
  if !fields(&body, 8192, &[]).is_ok_and(|fields| fields.len() == 1 && fields[0].0 == "csrf_token")
  {
    return failure(Error::Invalid, &session);
  }
  let (Some(config), Some(client)) = (&state.cfg.google, &state.google) else {
    return failure(Error::Unavailable, &session);
  };
  match store::issue(&state.db, state.clock.as_ref(), config, &session).await {
    Ok((value, verifier)) => {
      Redirect::to(&client.authorization(config, &value, &verifier)).into_response()
    }
    Err(error) => failure(error, &session),
  }
}
fn failure(error: Error, session: &DoctorSession) -> Response {
  let (status,notice) = match error {
    Error::Limited => (StatusCode::TOO_MANY_REQUESTS,"Se alcanzó el límite de solicitudes o hay una comprobación en curso. Actualiza el estado antes de volver a intentar."),
    Error::Invalid => (StatusCode::CONFLICT,"El formulario ya no coincide con la configuración actual o contiene datos no válidos. No se guardaron cambios. Actualiza el estado."),
    _ => (StatusCode::SERVICE_UNAVAILABLE,"No se pudo completar la operación. No se afirma que la conexión esté disponible ni que la limpieza haya terminado. Actualiza el estado."),
  };
  (
    status,
    Html(views::google(GoogleView {
      csrf_token: &session.csrf,
      connection: ConnectionView::Unavailable,
      generation: 0,
      selection_version: 0,
      current_probe_id: None,
      calendars: &[],
      selected_booking: None,
      selected_blocking: &[],
      saved_booking_label: None,
      calendars_available: false,
      permission: PermissionView::Unverified,
      check: CheckView::None,
      cleanups: &[],
      connect_block: Some("Actualiza el estado antes de iniciar otra acción."),
      selection_block: Some("Actualiza el estado."),
      probe_block: Some("Actualiza el estado."),
      notice: Some(notice),
      load_error: true,
      cleanup_previous_page: None,
      cleanup_next_page: None,
    })),
  )
    .into_response()
}
async fn callback(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  RawQuery(query): RawQuery,
  method: Method,
) -> Response {
  if method != Method::GET {
    return Redirect::to(PATH).into_response();
  }
  let _ = tokio::time::timeout(
    std::time::Duration::from_secs(12),
    complete_callback(&state, &session, query.as_deref().unwrap_or_default()),
  )
  .await;
  Redirect::to(PATH).into_response()
}
async fn complete_callback(
  state: &AppState,
  session: &DoctorSession,
  query: &str,
) -> Result<(), Error> {
  let fields = fields(query, 8192, &[])?;
  let value = single(&fields, "state")?;
  if value.len() != 43 {
    return Err(Error::Invalid);
  }
  let code = fields
    .iter()
    .find(|(k, _)| k == "code")
    .map(|(_, v)| v.as_str());
  let error = fields
    .iter()
    .find(|(k, _)| k == "error")
    .map(|(_, v)| v.as_str());
  if code.is_some() == error.is_some()
    || code
      .or(error)
      .is_none_or(|s| s.is_empty() || s.len() > 4096)
  {
    return Err(Error::Invalid);
  }
  let config = state.cfg.google.as_ref().ok_or(Error::Unavailable)?;
  let client = state.google.as_ref().ok_or(Error::Unavailable)?;
  let attempt = store::consume(
    &state.db,
    state.clock.as_ref(),
    config,
    session,
    value,
    error.is_some(),
  )
  .await?;
  let Some(code) = code else {
    return Ok(());
  };
  match client
    .exchange(config, code, &attempt.verifier, state.clock.now_millis())
    .await
  {
    Ok(tokens) => {
      let result = store::install(
        &state.db,
        state.clock.as_ref(),
        config,
        session,
        &attempt,
        &tokens,
      )
      .await;
      if result.is_err() {
        store::failed(&state.db, &attempt).await?;
      }
      result
    }
    Err(_) => store::failed(&state.db, &attempt).await,
  }
}
/// Strict application/x-www-form-urlencoded, including duplicate decoded names.
/// Only the explicitly repeatable calendar field may occur more than once.
pub(super) fn fields(
  raw: &str,
  limit: usize,
  repeated: &[&str],
) -> Result<Vec<(String, String)>, Error> {
  if raw.len() > limit {
    return Err(Error::Invalid);
  }
  fn decode(value: &str) -> Result<String, Error> {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
      match bytes[i] {
        b'%' => {
          if i + 2 >= bytes.len() {
            return Err(Error::Invalid);
          }
          let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).map_err(|_| Error::Invalid)?;
          out.push(u8::from_str_radix(hex, 16).map_err(|_| Error::Invalid)?);
          i += 3;
        }
        b'+' => {
          out.push(b' ');
          i += 1;
        }
        byte => {
          out.push(byte);
          i += 1;
        }
      }
    }
    let value = String::from_utf8(out).map_err(|_| Error::Invalid)?;
    if value.chars().any(char::is_control) {
      return Err(Error::Invalid);
    }
    Ok(value)
  }
  let mut result = Vec::new();
  let mut names = std::collections::HashSet::new();
  if raw.is_empty() {
    return Ok(result);
  }
  for pair in raw.split('&') {
    let (key, value) = pair.split_once('=').ok_or(Error::Invalid)?;
    let key = decode(key)?;
    let value = decode(value)?;
    if key.is_empty() || (!repeated.contains(&key.as_str()) && !names.insert(key.clone())) {
      return Err(Error::Invalid);
    }
    result.push((key, value));
  }
  Ok(result)
}
fn display_time(time: i64) -> String {
  chrono::DateTime::from_timestamp_millis(time)
    .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
    .unwrap_or_else(|| "Fecha no disponible".into())
}
fn reason(value: &str) -> &'static str {
  match value {
    "markers_mismatch"=>"El evento no coincide con los marcadores de esta prueba. No se eliminó ningún evento.",
    "missing_etag"=>"No se obtuvo una versión segura del evento para eliminarlo.",
    "connection_changed"|"authorization_changed"=>"Cambió la conexión o la selección. La limpieza conserva su calendario original.",
    "cleanup_permission"=>"La conexión no pudo demostrar acceso al calendario original para completar la limpieza.",
    "unconfigured"=>"La limpieza está pausada porque Google no está configurado o su cliente no está disponible. El objetivo original se conserva.",
    "credentials_unavailable"=>"Las credenciales no están disponibles. El objetivo original se conserva.",
    "conference_failed"=>"Google Meet no se pudo comprobar. Esto no afirma que todas las operaciones presenciales sean imposibles.",
    "insert_rejected"|"permission_unverified"=>"Google no aceptó la creación de la prueba. Los permisos sobre eventos siguen sin comprobar.",
    "deleted"=>"El evento de prueba se eliminó, pero no se comprobó Google Meet. Esto no garantiza la destrucción inmediata del espacio Meet.",
    _=>"No se pudo confirmar la limpieza dentro del límite de intentos. Una respuesta de ausencia también puede significar falta de acceso; revisa el objetivo original.",
  }
}
fn single<'a>(fields: &'a [(String, String)], name: &str) -> Result<&'a str, Error> {
  fields
    .iter()
    .find(|(k, _)| k == name)
    .map(|(_, v)| v.as_str())
    .ok_or(Error::Invalid)
}
async fn select(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  body: String,
) -> Response {
  let result = tokio::time::timeout(std::time::Duration::from_secs(12), async {
    let form = fields(&body, 64 * 1024, &["blocking_calendar"])?;
    if form.iter().any(|(k, _)| {
      ![
        "csrf_token",
        "generation",
        "selection_version",
        "booking_calendar",
        "blocking_calendar",
      ]
      .contains(&k.as_str())
    }) {
      return Err(Error::Invalid);
    }
    let generation = single(&form, "generation")?
      .parse()
      .map_err(|_| Error::Invalid)?;
    let version = single(&form, "selection_version")?
      .parse()
      .map_err(|_| Error::Invalid)?;
    let booking = single(&form, "booking_calendar")?;
    let blocking = form
      .iter()
      .filter(|(k, _)| k == "blocking_calendar")
      .map(|(_, v)| v.clone())
      .collect();
    super::calendars::save(&state, &session, generation, version, booking, blocking).await
  })
  .await
  .unwrap_or(Err(Error::Unavailable));
  match result {
    Ok(()) => Redirect::to(PATH).into_response(),
    Err(error) => failure(error, &session),
  }
}
async fn probe(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  body: String,
) -> Response {
  let result = async {
    let form = fields(&body, 8192, &[])?;
    if form.len() != 4 {
      return Err(Error::Invalid);
    }
    let generation = single(&form, "generation")?
      .parse()
      .map_err(|_| Error::Invalid)?;
    let version = single(&form, "selection_version")?
      .parse()
      .map_err(|_| Error::Invalid)?;
    let expected = single(&form, "expected_probe_id")?;
    let expected = if expected.is_empty() {
      None
    } else {
      Some(expected.parse().map_err(|_| Error::Invalid)?)
    };
    super::probes::start(&state, &session, generation, version, expected).await
  }
  .await;
  match result {
    Ok(()) => Redirect::to(PATH).into_response(),
    Err(error) => failure(error, &session),
  }
}
async fn cleanup_retry(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Path(id): Path<String>,
  body: String,
) -> Response {
  let result = async {
    let form = fields(&body, 8192, &[])?;
    if form.len() != 2 {
      return Err(Error::Invalid);
    }
    let generation = single(&form, "generation")?
      .parse()
      .map_err(|_| Error::Invalid)?;
    let id = id.parse().map_err(|_| Error::Invalid)?;
    super::probes::retry(&state, &session, id, generation).await
  }
  .await;
  match result {
    Ok(()) => Redirect::to(PATH).into_response(),
    Err(error) => failure(error, &session),
  }
}
async fn show(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  RawQuery(query): RawQuery,
) -> Response {
  let nav = fields(query.as_deref().unwrap_or_default(), 1024, &[]);
  let page_number = match nav.and_then(|f| {
    f.iter()
      .find(|(k, _)| k == "cleanup_page")
      .map(|(_, v)| v.parse::<u32>().map_err(|_| Error::Invalid))
      .unwrap_or(Ok(0))
  }) {
    Ok(page) => page,
    Err(error) => return failure(error, &session),
  };
  match tokio::time::timeout(
    std::time::Duration::from_secs(12),
    page(&state, &session, page_number),
  )
  .await
  {
    Ok(response) => response,
    Err(_) => failure(Error::Unavailable, &session),
  }
}
async fn page(state: &AppState, session: &DoctorSession, page_number: u32) -> Response {
  use super::store::sql;
  use sea_orm::ConnectionTrait;
  let result = store::load(&state.db).await;
  let row = result.as_ref().ok();
  let paused = state.cfg.google.is_none() || state.google.is_none();
  let mut connection = match (&state.cfg.google, row) {
    _ if paused => ConnectionView::Unconfigured,
    (None, _) => ConnectionView::Unconfigured,
    (_, None) => ConnectionView::Unavailable,
    (Some(config), Some(row)) => match row.try_get::<String>("", "status").ok().as_deref() {
      Some("disconnected") => ConnectionView::Disconnected,
      Some("reauthorize") => ConnectionView::ReauthorizationRequired,
      Some("connected") if store::decrypt(row, config).is_ok() => ConnectionView::Connected,
      _ => ConnectionView::Unavailable,
    },
  };
  let generation = row
    .and_then(|r| r.try_get::<i64>("", "generation").ok())
    .unwrap_or_default();
  let selection_version = row
    .and_then(|r| r.try_get::<i64>("", "selection_version").ok())
    .unwrap_or_default();
  let listed = if matches!(connection, ConnectionView::Connected) {
    Some(super::calendars::list(state).await)
  } else {
    None
  };
  let refreshed = if listed.is_some() {
    Some(store::load(&state.db).await)
  } else {
    None
  };
  let stale = refreshed.as_ref().is_some_and(|result| match result {
    Ok(current) => {
      current.try_get::<i64>("", "generation").ok() != Some(generation)
        || current.try_get::<i64>("", "selection_version").ok() != Some(selection_version)
        || current
          .try_get::<Option<uuid::Uuid>>("", "current_probe_id")
          .ok()
          != row.and_then(|r| r.try_get::<Option<uuid::Uuid>>("", "current_probe_id").ok())
    }
    Err(_) => true,
  });
  let available = !stale
    && listed.as_ref().is_some_and(|r| {
      r.as_ref()
        .is_ok_and(|(access, _)| access.generation == generation)
    });
  if let Some(Err(error)) = &listed {
    connection = if matches!(error, Error::Reauthorize) {
      ConnectionView::ReauthorizationRequired
    } else {
      ConnectionView::Unavailable
    };
  }
  let choices: Vec<_> = listed
    .as_ref()
    .and_then(|r| r.as_ref().ok())
    .map(|(_, items)| {
      items
        .iter()
        .map(|c| views::CalendarChoice {
          id: c.id.clone(),
          label: c.summary.clone(),
          booking_candidate: c.candidate(),
        })
        .collect()
    })
    .unwrap_or_default();
  let booking = row.and_then(|r| {
    r.try_get::<Option<String>>("", "booking_calendar")
      .ok()
      .flatten()
  });
  let label = row.and_then(|r| {
    r.try_get::<Option<String>>("", "booking_label")
      .ok()
      .flatten()
  });
  let blocking: Vec<String> = row
    .and_then(|r| {
      r.try_get::<serde_json::Value>("", "blocking_calendars")
        .ok()
    })
    .and_then(|v| serde_json::from_value(v).ok())
    .unwrap_or_default();
  let decryption_failed = state
    .cfg
    .google
    .as_ref()
    .zip(row)
    .is_some_and(|(config, r)| {
      r.try_get::<String>("", "status").ok().as_deref() == Some("connected")
        && store::decrypt(r, config).is_err()
    });
  let current = row.and_then(|r| {
    r.try_get::<Option<uuid::Uuid>>("", "current_probe_id")
      .ok()
      .flatten()
  });
  let probe = if let Some(id) = current {
    state
      .db
      .conn
      .query_one(sql(
        "SELECT * FROM google_probes WHERE id=$1",
        vec![id.into()],
      ))
      .await
  } else {
    Ok(None)
  };
  let cleanup_rows=state.db.conn.query_all(sql("SELECT * FROM google_probes WHERE cleanup_outcome='unresolved' OR ($2 AND cleanup_outcome='pending' AND create_committed_at IS NOT NULL) ORDER BY started_at,id LIMIT 26 OFFSET $1",vec![(i64::from(page_number)*25).into(),paused.into()])).await;
  let counts=state.db.conn.query_one(sql("SELECT (SELECT count(*) FROM google_auth_attempts WHERE issued_at>$1) AS authorizations,(SELECT count(*) FROM google_probes WHERE started_at>$1) AS starts,(SELECT count(*) FROM google_probes WHERE phase NOT IN ('complete','attention')) AS active",vec![(state.clock.now_millis()-3_600_000).into()])).await;
  let has_next = cleanup_rows.as_ref().is_ok_and(|r| r.len() > 25);
  let cleanups: Result<Vec<_>, sea_orm::DbErr> = cleanup_rows
    .as_ref()
    .map(|rows| {
      rows
        .iter()
        .take(25)
        .map(|r| {
          Ok(views::CleanupItem {
            id: r.try_get::<uuid::Uuid>("", "id")?.to_string(),
            calendar_label: format!(
              "{} · Calendario: {} · Evento: {}",
              r.try_get::<String>("", "calendar_label")?,
              r.try_get::<String>("", "calendar_id")?,
              r.try_get::<String>("", "event_id")?
            ),
            occurred_at: display_time(r.try_get("", "started_at")?),
            reason: if paused {
              reason("unconfigured")
            } else {
              reason(&r.try_get::<String>("", "reason")?)
            }
            .into(),
            can_retry: !paused
              && r.try_get::<String>("", "phase")? == "attention"
              && r
                .try_get::<Option<i64>>("", "last_cleanup_request")?
                .is_none_or(|t| t + 60_000 <= state.clock.now_millis()),
          })
        })
        .collect()
    })
    .unwrap_or_else(|_| Ok(vec![]));
  let permission = row
    .and_then(|r| {
      r.try_get::<Option<i64>>("", "permission_checked_at")
        .ok()
        .flatten()
    })
    .map(|t| PermissionView::Verified {
      checked_at: display_time(t),
    })
    .unwrap_or(PermissionView::Unverified);
  let capability = row.and_then(|r| {
    r.try_get::<Option<i64>>("", "capability_checked_at")
      .ok()
      .flatten()
  });
  let check = if let Some(t) = capability {
    CheckView::Passed {
      checked_at: display_time(t),
    }
  } else if let Ok(Some(p)) = &probe {
    match p.try_get::<String>("", "phase").ok().as_deref() {
      Some("create_committed" | "observing" | "cleanup") if paused => CheckView::CleanupPending {
        reason: reason("unconfigured").into(),
      },
      Some("prepared") => CheckView::Queued,
      Some("complete") => CheckView::Failed {
        reason: reason(&p.try_get::<String>("", "reason").unwrap_or_default()).into(),
      },
      Some("attention") => CheckView::CleanupPending {
        reason: reason(&p.try_get::<String>("", "reason").unwrap_or_default()).into(),
      },
      _ => CheckView::Running,
    }
  } else {
    CheckView::None
  };
  let counts_row = counts.as_ref().ok().and_then(|r| r.as_ref());
  let connect_block = counts_row
    .and_then(|r| r.try_get::<i64>("", "authorizations").ok())
    .filter(|n| *n >= 5)
    .map(|_| {
      "Se alcanzó el límite de cinco conexiones por hora. Espera antes de volver a intentar."
    });
  let probe_block = if paused {
    Some(reason("unconfigured"))
  } else if booking.is_none() {
    Some("Primero guarda la selección de calendarios.")
  } else if counts_row
    .and_then(|r| r.try_get::<i64>("", "active").ok())
    .is_some_and(|n| n > 0)
  {
    Some("Hay una comprobación en curso. Actualiza el estado antes de iniciar otra.")
  } else if counts_row
    .and_then(|r| r.try_get::<i64>("", "starts").ok())
    .is_some_and(|n| n >= 10)
  {
    Some("Se alcanzó el límite de diez comprobaciones por hora.")
  } else {
    None
  };
  let notice = row.and_then(|r| r.try_get::<Option<String>>("", "notice").ok().flatten());
  let notice=match notice.as_deref() {Some("denied")=>Some("No autorizaste la nueva conexión. La conexión anterior no fue reemplazada."),Some("authorization_failed"|"authorization_incomplete")=>Some("No se completó la nueva autorización con los permisos exactos y credenciales sin conexión. La conexión anterior no fue reemplazada."),Some("connected_reselect")=>Some("Conexión autorizada. Selecciona de nuevo los calendarios; esta autorización puede pertenecer a otra cuenta."),Some("selection_saved")=>Some("Selección guardada. Los permisos sobre eventos y Google Meet todavía requieren una comprobación."),Some("refresh_uncertain")=>Some("No se pudo confirmar la renovación de credenciales. Vuelve a conectar Google; no se afirma que la autorización haya sido revocada."),_=>None};
  let load_error = result.is_err()
    || stale
    || decryption_failed
    || cleanup_rows.is_err()
    || cleanups.is_err()
    || probe.is_err()
    || counts.is_err();
  let current = current.map(|id| id.to_string());
  (
    if load_error {
      StatusCode::SERVICE_UNAVAILABLE
    } else {
      StatusCode::OK
    },
    Html(views::google(GoogleView {
      csrf_token: &session.csrf,
      connection,
      generation,
      selection_version,
      current_probe_id: current.as_deref(),
      calendars: &choices,
      selected_booking: booking.as_deref(),
      selected_blocking: &blocking,
      saved_booking_label: label.as_deref(),
      calendars_available: available,
      permission,
      check,
      cleanups: &cleanups.unwrap_or_default(),
      connect_block,
      selection_block: None,
      probe_block,
      notice,
      load_error,
      cleanup_previous_page: page_number.checked_sub(1),
      cleanup_next_page: if has_next {
        page_number.checked_add(1)
      } else {
        None
      },
    })),
  )
    .into_response()
}
