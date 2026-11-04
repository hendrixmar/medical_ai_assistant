use super::views::{self, Connection, SettingsView};
use crate::{
  app::AppState,
  modules::{
    doctor::DoctorSession,
    whatsapp::{self, inbox::sql},
  },
};
use axum::{
  extract::State,
  http::StatusCode,
  response::{Html, IntoResponse, Redirect, Response},
  routing::{get, post},
  Extension, Router,
};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};

#[derive(Clone)]
struct SavedConnection {
  account_id: Option<String>,
  charges_enabled: bool,
  details_submitted: bool,
  notice: Option<String>,
}

pub fn router() -> Router<AppState> {
  Router::new()
    .route("/settings/stripe", get(settings))
    .route("/settings/stripe/connect", post(connect))
    .route("/settings/stripe/return", get(onboarding_return))
    .route("/settings/stripe/refresh", get(onboarding_refresh))
    .route("/settings/stripe/disconnect", post(disconnect))
}

async fn settings(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
) -> Response {
  let saved = match load_connection(&state).await {
    Ok(saved) => saved,
    Err(_) => return unavailable(),
  };
  let (saved, refresh_notice) = match refresh_connection(&state, saved).await {
    Ok(value) => value,
    Err(_) => return unavailable(),
  };
  page(&session.csrf, state.stripe.is_some(), saved, refresh_notice)
}

async fn connect(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
) -> Response {
  let Some(client) = state.stripe.as_ref() else {
    return unavailable();
  };
  let saved = match load_connection(&state).await {
    Ok(saved) => saved,
    Err(_) => return unavailable(),
  };
  let account_id = match saved.and_then(|saved| saved.account_id) {
    Some(account_id) => account_id,
    None => {
      let account = match client.create_account().await {
        Ok(account) => account,
        Err(_) => {
          let _ = save_notice(&state, "No se pudo iniciar la conexión con Stripe.").await;
          return error_page(&state, &session.csrf).await;
        }
      };
      if save_account(&state, &account.id).await.is_err() {
        return unavailable();
      }
      account.id
    }
  };
  match client.create_account_link(&account_id).await {
    Ok(url) => Redirect::to(&url).into_response(),
    Err(_) => {
      let _ = save_notice(&state, "No se pudo abrir el registro de Stripe.").await;
      error_page(&state, &session.csrf).await
    }
  }
}

async fn onboarding_return(State(state): State<AppState>) -> Response {
  match load_connection(&state).await {
    Ok(saved) => match refresh_connection(&state, saved).await {
      Ok(_) => Redirect::to("/settings/stripe").into_response(),
      Err(_) => unavailable(),
    },
    Err(_) => unavailable(),
  }
}

async fn onboarding_refresh(State(state): State<AppState>) -> Response {
  let saved = match load_connection(&state).await {
    Ok(saved) => saved,
    Err(_) => return unavailable(),
  };
  let (saved, _) = match refresh_connection(&state, saved).await {
    Ok(value) => value,
    Err(_) => return unavailable(),
  };
  if let (Some(client), Some(saved)) = (state.stripe.as_ref(), saved) {
    if let Some(account_id) = saved.account_id.as_deref() {
      if !saved.charges_enabled {
        if let Ok(url) = client.create_account_link(account_id).await {
          return Redirect::to(&url).into_response();
        }
      }
    }
  }
  Redirect::to("/settings/stripe").into_response()
}

async fn disconnect(State(state): State<AppState>) -> Response {
  let Some(client) = state.stripe.as_ref() else {
    return unavailable();
  };
  let saved = match load_connection(&state).await {
    Ok(saved) => saved,
    Err(_) => return unavailable(),
  };
  let Some(account_id) = saved.and_then(|saved| saved.account_id) else {
    return Redirect::to("/settings/stripe").into_response();
  };
  let links = match state
    .db
    .conn
    .query_all(sql(
      "SELECT appointment_id,revision,account_id,link_id FROM stripe_payment_links WHERE account_id=$1 AND link_id IS NOT NULL AND deactivated_at IS NULL ORDER BY created_at LIMIT 2",
      vec![account_id.clone().into()],
    ))
    .await
  {
    Ok(links) => links,
    Err(_) => return unavailable(),
  };
  let now = state.clock.now_millis();
  let mut failed = false;
  // Limit provider calls here; the worker retries remaining links after local disconnect.
  for link in links {
    let appointment: uuid::Uuid = match link.try_get("", "appointment_id") {
      Ok(id) => id,
      Err(_) => return unavailable(),
    };
    let revision: i64 = match link.try_get("", "revision") {
      Ok(revision) => revision,
      Err(_) => return unavailable(),
    };
    let account: String = match link.try_get("", "account_id") {
      Ok(account) => account,
      Err(_) => return unavailable(),
    };
    let link_id: String = match link.try_get("", "link_id") {
      Ok(link_id) => link_id,
      Err(_) => return unavailable(),
    };
    if client
      .deactivate_payment_link(&account, &link_id)
      .await
      .is_ok()
    {
      if state
        .db
        .conn
        .execute(sql(
          "UPDATE stripe_payment_links SET deactivated_at=$3,next_check_at=NULL,outcome=CASE WHEN outcome='active' THEN 'deactivated' ELSE outcome END WHERE appointment_id=$1 AND revision=$2 AND deactivated_at IS NULL",
          vec![appointment.into(), revision.into(), now.into()],
        ))
        .await
        .is_err()
      {
        return unavailable();
      }
    } else {
      failed = true;
      if state
        .db
        .conn
        .execute(sql(
          "UPDATE stripe_payment_links SET next_check_at=$3 WHERE appointment_id=$1 AND revision=$2 AND deactivated_at IS NULL",
          vec![appointment.into(), revision.into(), now.saturating_add(60_000).into()],
        ))
        .await
        .is_err()
      {
        return unavailable();
      }
    }
  }
  let notice = if failed {
    "La conexión local se olvidó, pero algunos enlaces no pudieron desactivarse y se volverán a intentar. La cuenta de Stripe del doctor no se eliminó."
  } else {
    "La conexión local se olvidó. Los enlaces que sigan abiertos se volverán a desactivar; la cuenta de Stripe del doctor no se eliminó."
  };
  let tx = match state.db.conn.begin().await {
    Ok(tx) => tx,
    Err(_) => return unavailable(),
  };
  if whatsapp::bound_transaction(&tx).await.is_err()
    || tx
      .execute(sql(
        "UPDATE stripe_connection SET account_id=NULL,charges_enabled=false,details_submitted=false,checked_at=$2,notice=$3,updated_at=$2 WHERE id=1 AND account_id=$1",
        vec![account_id.clone().into(), now.into(), notice.into()],
      ))
      .await
      .is_err()
    || tx
      .execute(sql(
        "UPDATE stripe_payment_links SET outcome=CASE WHEN outcome='active' THEN 'deactivating' ELSE outcome END,next_check_at=CASE WHEN next_check_at IS NULL OR next_check_at<$2 THEN $2 ELSE next_check_at END WHERE account_id=$1 AND deactivated_at IS NULL",
        vec![account_id.into(), now.into()],
      ))
      .await
      .is_err()
    || tx.commit().await.is_err()
  {
    return unavailable();
  }
  Redirect::to("/settings/stripe").into_response()
}

async fn load_connection(state: &AppState) -> Result<Option<SavedConnection>, DbErr> {
  let Some(row) = state
    .db
    .conn
    .query_one(sql(
      "SELECT account_id,charges_enabled,details_submitted,notice FROM stripe_connection WHERE id=1",
      vec![],
    ))
    .await?
  else {
    return Ok(None);
  };
  Ok(Some(SavedConnection {
    account_id: row.try_get("", "account_id")?,
    charges_enabled: row.try_get("", "charges_enabled")?,
    details_submitted: row.try_get("", "details_submitted")?,
    notice: row.try_get("", "notice")?,
  }))
}

async fn save_account(state: &AppState, account_id: &str) -> Result<(), DbErr> {
  let now = state.clock.now_millis();
  state
    .db
    .conn
    .execute(sql(
      "INSERT INTO stripe_connection(id,account_id,charges_enabled,details_submitted,created_at,updated_at) VALUES (1,$1,false,false,$2,$2) ON CONFLICT (id) DO UPDATE SET account_id=EXCLUDED.account_id,charges_enabled=false,details_submitted=false,checked_at=NULL,notice=NULL,updated_at=EXCLUDED.updated_at WHERE stripe_connection.account_id IS NULL",
      vec![account_id.into(), now.into()],
    ))
    .await?;
  Ok(())
}

async fn save_notice(state: &AppState, notice: &str) -> Result<(), DbErr> {
  let now = state.clock.now_millis();
  state
    .db
    .conn
    .execute(sql(
      "INSERT INTO stripe_connection(id,charges_enabled,details_submitted,notice,created_at,updated_at) VALUES (1,false,false,$1,$2,$2) ON CONFLICT (id) DO UPDATE SET notice=EXCLUDED.notice,updated_at=EXCLUDED.updated_at",
      vec![notice.into(), now.into()],
    ))
    .await?;
  Ok(())
}

async fn refresh_connection(
  state: &AppState,
  saved: Option<SavedConnection>,
) -> Result<(Option<SavedConnection>, Option<&'static str>), DbErr> {
  let Some(client) = state.stripe.as_ref() else {
    return Ok((saved, None));
  };
  let Some(mut saved) = saved else {
    return Ok((None, None));
  };
  let Some(account_id) = saved.account_id.clone() else {
    return Ok((Some(saved), None));
  };
  let status = match client.retrieve_account(&account_id).await {
    Ok(status) => status,
    Err(_) => {
      return Ok((
        Some(saved),
        Some("No se pudo consultar el estado actual de Stripe. Vuelve a intentarlo."),
      ));
    }
  };
  let now = state.clock.now_millis();
  let updated = state
    .db
    .conn
    .execute(sql(
      "UPDATE stripe_connection SET charges_enabled=$2,details_submitted=$3,checked_at=$4,notice=NULL,updated_at=$4 WHERE id=1 AND account_id=$1",
      vec![
        account_id.into(),
        status.charges_enabled.into(),
        status.details_submitted.into(),
        now.into(),
      ],
    ))
    .await?;
  if updated.rows_affected() != 1 {
    return Ok((load_connection(state).await?, None));
  }
  saved.charges_enabled = status.charges_enabled;
  saved.details_submitted = status.details_submitted;
  saved.notice = None;
  Ok((Some(saved), None))
}

fn page(
  csrf_token: &str,
  configured: bool,
  saved: Option<SavedConnection>,
  refresh_notice: Option<&str>,
) -> Response {
  let connection = if !configured {
    Connection::NotConfigured
  } else {
    match saved.as_ref().and_then(|saved| saved.account_id.as_ref()) {
      None => Connection::Disconnected,
      Some(_) if saved.as_ref().is_some_and(|saved| saved.charges_enabled) => Connection::Ready,
      Some(_) => Connection::Incomplete,
    }
  };
  let details_submitted = saved
    .as_ref()
    .and_then(|saved| saved.account_id.as_ref())
    .map(|_| saved.as_ref().is_some_and(|saved| saved.details_submitted));
  Html(views::settings(SettingsView {
    csrf_token,
    connection,
    details_submitted,
    notice: refresh_notice.or_else(|| saved.as_ref().and_then(|saved| saved.notice.as_deref())),
  }))
  .into_response()
}

async fn error_page(state: &AppState, csrf_token: &str) -> Response {
  let saved = load_connection(state).await.ok().flatten();
  let mut response = page(csrf_token, state.stripe.is_some(), saved, None);
  *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
  response
}

fn unavailable() -> Response {
  StatusCode::SERVICE_UNAVAILABLE.into_response()
}
