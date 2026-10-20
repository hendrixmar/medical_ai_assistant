use super::{
  history, store,
  views::{
    self, AttentionItem, AttentionKind, ContactInboxView, DashboardAppointment, DashboardView,
    GoogleConnectionStatus,
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
  Extension, Router,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};
use uuid::Uuid;

const PAGE_SIZE: i64 = 25;

pub fn protected_router() -> Router<AppState> {
  Router::new()
    .route("/dashboard", get(dashboard))
    .route("/contacts", get(contact_list))
    .route("/contacts/{id}", get(contact_history))
    .route("/contacts/{id}/takeover/end", post(end_takeover))
    .route("/followups/{id}/resolve", post(resolve))
}

#[derive(Default, serde::Deserialize)]
struct Navigation {
  #[serde(default)]
  page: u32,
}

struct Item {
  id: String,
  contact: String,
  occurred_at: String,
  kind: AttentionKind,
  contact_id: Option<String>,
  appointment_id: Option<String>,
  receipt_id: Option<String>,
}

struct Attention {
  items: Vec<Item>,
  queued: u64,
  deferred: u64,
  next: bool,
}

struct UpcomingAppointment {
  id: String,
  patient_name: String,
  service_name: String,
  starts_at: String,
  status: &'static str,
  payment: &'static str,
  calendar_sync: &'static str,
}

fn attention_kind(kind: &str) -> AttentionKind {
  match kind {
    "followup" => AttentionKind::Followup,
    "receipt_followup" => AttentionKind::ReceiptFollowup,
    "followup_request" => AttentionKind::Other,
    "failed" => AttentionKind::SendFailed,
    "uncertain" => AttentionKind::SendUncertain,
    "error" => AttentionKind::SendErrorReported,
    "conflict" => AttentionKind::IdentityConflict,
    "receipt_review" => AttentionKind::ReceiptReview,
    "receipt_copy_failed" => AttentionKind::ReceiptCopyFailed,
    "late_receipt" => AttentionKind::LateReceipt,
    "receipt_selection" => AttentionKind::ReceiptSelection,
    "receipt_clarification" => AttentionKind::ReceiptClarification,
    "refund_due" => AttentionKind::RefundDue,
    "calendar_conflict" => AttentionKind::CalendarConflict,
    "calendar_sync_failed" => AttentionKind::CalendarSyncFailed,
    "calendar_availability_unavailable" => AttentionKind::CalendarAvailabilityUnavailable,
    "calendar_check_unavailable" => AttentionKind::CalendarCheckUnavailable,
    "google_reauthorization_required" => AttentionKind::GoogleReauthorizationRequired,
    "availability_conflict" => AttentionKind::AvailabilityConflict,
    "whatsapp_disconnected" => AttentionKind::WhatsAppDisconnected,
    "whatsapp_recovery_incomplete" => AttentionKind::WhatsAppRecoveryIncomplete,
    "whatsapp_identity_missing" => AttentionKind::WhatsAppIdentityMissing,
    "google_disconnected" => AttentionKind::GoogleDisconnected,
    "retention_policy_review" => AttentionKind::RetentionPolicyReview,
    _ => AttentionKind::Other,
  }
}

fn receipt_kind(kind: &str) -> bool {
  matches!(
    kind,
    "receipt_review" | "late_receipt" | "receipt_selection" | "receipt_clarification"
  )
}

fn receipt_from_dedupe(key: Option<&str>) -> Option<Uuid> {
  key?
    .strip_prefix("receipt:")?
    .split_once(':')
    .and_then(|(id, _)| Uuid::parse_str(id).ok())
}

async fn load(state: &AppState, page: u32) -> Result<Attention, DbErr> {
  let timezone = state
    .db
    .conn
    .query_one(sql("SELECT timezone FROM practice WHERE id=1", vec![]))
    .await?
    .and_then(|row| row.try_get::<Option<String>>("", "timezone").ok().flatten())
    .and_then(|value| value.parse::<chrono_tz::Tz>().ok())
    .unwrap_or(chrono_tz::UTC);
  let rows = state
    .db
    .conn
    .query_all(sql(
      "WITH attention(id,contact_id,created_at,priority,kind,appointment_id,dedupe_key) AS (
        SELECT f.id,f.contact_id,f.created_at,0,
          CASE WHEN receipt_followup.appointment_id IS NULL THEN 'followup' ELSE 'receipt_followup' END::text,
          receipt_followup.appointment_id,NULL::text
        FROM followup_requests f
        LEFT JOIN LATERAL (
          SELECT ai.appointment_id
          FROM attention_items ai
          JOIN receipts r ON r.inbox_message_id=f.inbox_message_id AND r.contact_id=f.contact_id AND r.appointment_id=ai.appointment_id
          WHERE ai.contact_id=f.contact_id AND ai.kind='receipt_followup' AND ai.resolved_at IS NULL
          ORDER BY ai.created_at DESC,ai.id
          LIMIT 1
        ) receipt_followup ON true
        WHERE f.resolved_at IS NULL
        UNION ALL
        SELECT id,contact_id,created_at,2,
          CASE WHEN ack_error THEN 'error' ELSE state END::text,
          appointment_id,NULL::text
        FROM outbox_messages
        WHERE state='failed' OR ack_error OR (state='uncertain' AND dispatch_deadline<=$2)
        UNION ALL
        SELECT id,id,created_at,2,'conflict'::text,NULL::uuid,NULL::text
        FROM contacts WHERE identity_conflict
        UNION ALL
        SELECT ai.id,ai.contact_id,ai.created_at,
          CASE WHEN ai.kind IN ('receipt_review','late_receipt','receipt_selection','receipt_clarification','receipt_copy_failed') THEN 0
               WHEN ai.kind IN ('refund_due','calendar_conflict','calendar_sync_failed','calendar_check_unavailable','google_reauthorization_required','whatsapp_disconnected','whatsapp_recovery_incomplete','whatsapp_identity_missing') THEN 1
               ELSE 2 END,
          ai.kind,ai.appointment_id,ai.dedupe_key
        FROM attention_items ai
        WHERE ai.resolved_at IS NULL AND ai.kind<>'receipt_followup'
          AND NOT (ai.kind='followup_request' AND EXISTS (
            SELECT FROM followup_requests f WHERE f.contact_id=ai.contact_id AND f.resolved_at IS NULL
          ))
      )
      SELECT a.id,c.id AS contact_id,a.created_at,a.kind,a.appointment_id,a.dedupe_key,
        (SELECT min(jid) FROM contact_aliases WHERE contact_id=c.id AND jid LIKE '%@c.us') AS phone,
        c.identity_conflict
      FROM attention a
      LEFT JOIN appointments ap ON ap.id=a.appointment_id
      LEFT JOIN contacts c ON c.id=COALESCE(a.contact_id,ap.contact_id)
      ORDER BY a.priority,a.created_at,a.id LIMIT 26 OFFSET $1",
      vec![
        (i64::from(page) * PAGE_SIZE).into(),
        state.clock.now_millis().into(),
      ],
    ))
    .await?;
  let next = rows.len() > PAGE_SIZE as usize;
  let mut items = Vec::with_capacity(rows.len().min(PAGE_SIZE as usize));
  for row in rows.into_iter().take(PAGE_SIZE as usize) {
    let time: i64 = row.try_get("", "created_at")?;
    let occurred_at = DateTime::<Utc>::from_timestamp_millis(time)
      .ok_or_else(|| DbErr::Custom("Invalid stored time".into()))?
      .with_timezone(&timezone)
      .format("%Y-%m-%d %H:%M %Z")
      .to_string();
    let contact_id = row.try_get::<Option<Uuid>>("", "contact_id")?;
    let identity_conflict = row
      .try_get::<Option<bool>>("", "identity_conflict")?
      .unwrap_or(false);
    let contact = if contact_id.is_none() {
      "Sin contacto asociado".to_owned()
    } else if identity_conflict {
      "Contacto con identidad en conflicto".to_owned()
    } else {
      row
        .try_get::<Option<String>>("", "phone")?
        .map(|phone| format!("+{}", phone.trim_end_matches("@c.us")))
        .unwrap_or_else(|| "Contacto sin número resuelto".to_owned())
    };
    let kind_text = row.try_get::<String>("", "kind")?;
    let appointment_id = row.try_get::<Option<Uuid>>("", "appointment_id")?;
    let receipt_id = if receipt_kind(&kind_text) {
      let candidate =
        receipt_from_dedupe(row.try_get::<Option<String>>("", "dedupe_key")?.as_deref());
      if let Some(candidate) = candidate {
        state
          .db
          .conn
          .query_one(sql(
            "SELECT id FROM receipts WHERE id=$1 AND ($2::uuid IS NULL OR contact_id=$2) AND ($3::uuid IS NULL OR appointment_id=$3) AND ($2::uuid IS NOT NULL OR $3::uuid IS NOT NULL)",
            vec![candidate.into(), contact_id.into(), appointment_id.into()],
          ))
          .await?
          .map(|row| row.try_get::<Uuid>("", "id").map(|id| id.to_string()))
          .transpose()?
      } else {
        None
      }
    } else {
      None
    };
    items.push(Item {
      id: row.try_get::<Uuid>("", "id")?.to_string(),
      contact,
      occurred_at,
      kind: attention_kind(&kind_text),
      contact_id: contact_id.map(|id| id.to_string()),
      appointment_id: appointment_id.map(|id| id.to_string()),
      receipt_id,
    });
  }
  let counts = state
    .db
    .conn
    .query_one(sql(
      "SELECT count(*) FILTER (WHERE state IN ('queued','typing')) AS queued,count(*) FILTER (WHERE state IN ('queued','typing') AND rate_deferred_until>$1) AS deferred FROM outbox_messages",
      vec![state.clock.now_millis().into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Queue unavailable".into()))?;
  Ok(Attention {
    items,
    queued: counts.try_get::<i64>("", "queued")? as u64,
    deferred: counts.try_get::<i64>("", "deferred")? as u64,
    next,
  })
}

fn appointment_status(value: &str) -> &'static str {
  match value {
    "requested" => "Solicitud sin confirmar",
    "awaiting_payment" => "Pendiente de comprobante",
    "confirmed" => "Confirmada",
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

async fn load_upcoming(state: &AppState) -> Result<Vec<UpcomingAppointment>, DbErr> {
  let rows = state
    .db
    .conn
    .query_all(sql(
      "SELECT id,patient_name,service_name,starts_at,timezone,status,payment,calendar_sync
       FROM appointments
       WHERE starts_at >= $1 AND status NOT IN ('cancelled','expired')
       ORDER BY starts_at,id LIMIT 10",
      vec![state.clock.now_millis().into()],
    ))
    .await?;
  rows
    .into_iter()
    .map(|row| {
      let starts_at: i64 = row.try_get("", "starts_at")?;
      let timezone: String = row.try_get("", "timezone")?;
      let zone = timezone
        .parse::<chrono_tz::Tz>()
        .map_err(|_| DbErr::Custom("Invalid Appointment timezone".into()))?;
      let starts_at = DateTime::<Utc>::from_timestamp_millis(starts_at)
        .ok_or_else(|| DbErr::Custom("Invalid Appointment timestamp".into()))?
        .with_timezone(&zone)
        .format("%Y-%m-%d %H:%M %Z")
        .to_string();
      Ok(UpcomingAppointment {
        id: row.try_get::<Uuid>("", "id")?.to_string(),
        patient_name: row.try_get("", "patient_name")?,
        service_name: row.try_get("", "service_name")?,
        starts_at,
        status: appointment_status(&row.try_get::<String>("", "status")?),
        payment: payment_status(&row.try_get::<String>("", "payment")?),
        calendar_sync: calendar_status(&row.try_get::<String>("", "calendar_sync")?),
      })
    })
    .collect()
}

async fn dashboard(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Query(nav): Query<Navigation>,
) -> Response {
  let attention_result = load(&state, nav.page).await;
  let attention_failed = attention_result.is_err();
  let attention = attention_result.unwrap_or(Attention {
    items: vec![],
    queued: 0,
    deferred: 0,
    next: false,
  });
  let appointments_result = load_upcoming(&state).await;
  let appointment_failed = appointments_result.is_err();
  let appointments = appointments_result.unwrap_or_default();
  let google_cleanup = state
    .db
    .conn
    .query_one(sql(
      "SELECT count(*) AS count FROM google_probes WHERE cleanup_outcome='unresolved' OR ($1 AND cleanup_outcome='pending' AND create_committed_at IS NOT NULL)",
      vec![(state.cfg.google.is_none() || state.google.is_none()).into()],
    ))
    .await
    .and_then(|row| row.ok_or_else(|| DbErr::Custom("Google attention unavailable".into())))
    .and_then(|row| row.try_get::<i64>("", "count"));
  let google_connection = state
    .db
    .conn
    .query_one(sql(
      "SELECT status FROM google_connection WHERE id=1",
      vec![],
    ))
    .await
    .and_then(|row| row.ok_or_else(|| DbErr::Custom("Google connection unavailable".into())))
    .and_then(|row| row.try_get::<String>("", "status"));
  let google_status = if state.cfg.google.is_none() {
    GoogleConnectionStatus::NotConfigured
  } else if state.google.is_none() {
    GoogleConnectionStatus::Unavailable
  } else {
    match google_connection.as_deref() {
      Ok("connected") => GoogleConnectionStatus::Connected,
      Ok("disconnected") => GoogleConnectionStatus::Disconnected,
      Ok("reauthorize") => GoogleConnectionStatus::ReauthorizationRequired,
      _ => GoogleConnectionStatus::Unavailable,
    }
  };
  let google_load_error =
    google_cleanup.is_err() || (state.cfg.google.is_some() && google_connection.is_err());
  let items: Vec<_> = attention
    .items
    .iter()
    .map(|item| AttentionItem {
      id: &item.id,
      contact: &item.contact,
      occurred_at: &item.occurred_at,
      kind: item.kind,
      contact_id: item.contact_id.as_deref(),
      appointment_id: item.appointment_id.as_deref(),
      receipt_id: item.receipt_id.as_deref(),
    })
    .collect();
  let appointment_views: Vec<_> = appointments
    .iter()
    .map(|appointment| DashboardAppointment {
      id: &appointment.id,
      patient_name: &appointment.patient_name,
      service_name: &appointment.service_name,
      starts_at: &appointment.starts_at,
      status: appointment.status,
      payment: appointment.payment,
      calendar_sync: appointment.calendar_sync,
    })
    .collect();
  (
    if attention_failed || appointment_failed {
      StatusCode::SERVICE_UNAVAILABLE
    } else {
      StatusCode::OK
    },
    Html(views::dashboard(DashboardView {
      csrf_token: &session.csrf,
      items: &items,
      notice: None,
      load_error: attention_failed,
      appointment_load_error: appointment_failed,
      appointments: &appointment_views,
      outbound_enabled: state.cfg.waha_outbound_enabled,
      queued_count: attention.queued,
      rate_deferred_count: attention.deferred,
      google_cleanup_count: google_cleanup.as_ref().copied().unwrap_or_default() as u64,
      google_status,
      google_load_error,
      previous_page: nav.page.checked_sub(1),
      next_page: if attention.next {
        nav.page.checked_add(1)
      } else {
        None
      },
    })),
  )
    .into_response()
}

async fn contact_list(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Query(nav): Query<Navigation>,
) -> Response {
  let result = history::inbox(&state.db, nav.page, state.clock.now_millis()).await;
  let load_error = result.is_err();
  let data = result.unwrap_or(history::InboxPage {
    contacts: vec![],
    next_page: false,
  });
  (
    if load_error {
      StatusCode::SERVICE_UNAVAILABLE
    } else {
      StatusCode::OK
    },
    Html(views::contact_inbox(ContactInboxView {
      csrf_token: &session.csrf,
      page: nav.page,
      contacts: &data.contacts,
      load_error,
      next_page: data.next_page,
    })),
  )
    .into_response()
}

async fn contact_history(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Path(id): Path<String>,
  Query(nav): Query<Navigation>,
) -> Response {
  let Ok(id) = Uuid::parse_str(&id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  match history::contact(&state.db, id, nav.page, state.clock.now_millis()).await {
    Ok(Some(contact)) => Html(views::contact_history(
      &session.csrf,
      &contact,
      state.cfg.waha_takeover_enabled,
    ))
    .into_response(),
    Ok(None) => StatusCode::NOT_FOUND.into_response(),
    Err(_) => (
      StatusCode::SERVICE_UNAVAILABLE,
      Html(views::contact_history_error(&session.csrf, id, nav.page)),
    )
      .into_response(),
  }
}

async fn end_takeover(State(state): State<AppState>, Path(id): Path<String>) -> Response {
  let Ok(contact) = Uuid::parse_str(&id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let tx = match state.db.conn.begin().await {
    Ok(tx) => tx,
    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
  };
  if whatsapp::bound_transaction(&tx).await.is_err() {
    let _ = tx.rollback().await;
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  }
  let exists = tx
    .query_one(sql(
      "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
      vec![contact.into()],
    ))
    .await;
  match exists {
    Ok(Some(_)) => {}
    Ok(None) => {
      let _ = tx.rollback().await;
      return StatusCode::NOT_FOUND.into_response();
    }
    Err(_) => {
      let _ = tx.rollback().await;
      return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
  }
  if whatsapp::takeover::end(&tx, contact, state.clock.now_millis())
    .await
    .is_err()
    || tx.commit().await.is_err()
  {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  }
  Redirect::to(&format!("/contacts/{contact}")).into_response()
}

async fn resolve(State(state): State<AppState>, Path(id): Path<String>) -> Response {
  let Ok(id) = Uuid::parse_str(&id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let Some(origin) = state.cfg.public_origin.as_deref() else {
    return StatusCode::SERVICE_UNAVAILABLE.into_response();
  };
  match store::resolve(&state.db, id, origin, state.clock.now_millis()).await {
    Ok(true) => Redirect::to("/dashboard").into_response(),
    Ok(false) => StatusCode::NOT_FOUND.into_response(),
    Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
  }
}
