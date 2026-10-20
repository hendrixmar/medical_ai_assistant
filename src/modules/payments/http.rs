use super::{
  actions::{self, LateChoice, RejectionChoice},
  intake::{self, SelectOutcome, SelectionActor},
  views::{self, AppointmentSummary, Candidate, ReceiptReview},
};
use crate::{
  app::AppState,
  modules::{
    appointments::{model::Appointment, store as appointment_store},
    doctor::DoctorSession,
    whatsapp::{self, inbox::sql, media},
  },
};
use axum::{
  body::Body,
  extract::{Form, Path, State},
  http::{header, HeaderValue, StatusCode},
  response::{Html, IntoResponse, Redirect, Response},
  routing::{get, post},
  Extension, Router,
};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use uuid::Uuid;

pub fn protected_router() -> Router<AppState> {
  Router::new()
    .route("/dashboard/payments/receipts/{id}", get(receipt))
    .route("/dashboard/payments/receipts/{id}/file", get(receipt_file))
    .route(
      "/dashboard/payments/receipts/{id}/association",
      post(associate),
    )
    .route("/dashboard/payments/receipts/{id}/verify", post(verify))
    .route("/dashboard/payments/receipts/{id}/reject", post(reject))
    .route(
      "/dashboard/payments/receipts/{id}/late-outcome",
      post(late_outcome),
    )
    .route("/dashboard/payments/receipts/{id}/cancel", post(cancel))
}

#[derive(FromQueryResult)]
struct ReceiptRow {
  id: Uuid,
  appointment_id: Option<Uuid>,
  mime: String,
  sent_at: i64,
  arrived_at: i64,
  timely: Option<bool>,
  late: Option<bool>,
  bytes_available: bool,
  reviewed_at: Option<i64>,
  review_outcome: Option<String>,
  clarification_requested_at: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AssociationForm {
  appointment_id: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RejectForm {
  choice: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LateForm {
  outcome: String,
}

#[derive(Clone, Copy)]
enum DomainAction {
  Verify,
  Reject(RejectionChoice),
  Late(LateChoice),
  Cancel,
}

async fn receipt(
  State(state): State<AppState>,
  Extension(session): Extension<DoctorSession>,
  Path(raw_id): Path<String>,
) -> Response {
  let Ok(id) = Uuid::parse_str(&raw_id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  match load_review(&state, id).await {
    Ok(Some(view)) => Html(views::receipt(&session.csrf, &view)).into_response(),
    Ok(None) => StatusCode::NOT_FOUND.into_response(),
    Err(_) => service_unavailable(),
  }
}

async fn receipt_file(
  State(state): State<AppState>,
  Extension(_session): Extension<DoctorSession>,
  Path(raw_id): Path<String>,
) -> Response {
  let Ok(id) = Uuid::parse_str(&raw_id) else {
    return file_unavailable(StatusCode::NOT_FOUND);
  };
  let row = match state
    .db
    .conn
    .query_one(sql(
      "SELECT mime,bytes,deleted_at FROM receipts WHERE id=$1",
      vec![id.into()],
    ))
    .await
  {
    Ok(row) => row,
    Err(_) => return file_unavailable(StatusCode::SERVICE_UNAVAILABLE),
  };
  let Some(row) = row else {
    return file_unavailable(StatusCode::NOT_FOUND);
  };
  let mime: String = match row.try_get("", "mime") {
    Ok(value) => value,
    Err(_) => return file_unavailable(StatusCode::SERVICE_UNAVAILABLE),
  };
  let deleted_at = match row.try_get::<Option<i64>>("", "deleted_at") {
    Ok(value) => value,
    Err(_) => return file_unavailable(StatusCode::SERVICE_UNAVAILABLE),
  };
  let bytes = match row.try_get::<Option<Vec<u8>>>("", "bytes") {
    Ok(Some(bytes)) if deleted_at.is_none() => bytes,
    Ok(_) => return file_unavailable(StatusCode::GONE),
    Err(_) => return file_unavailable(StatusCode::SERVICE_UNAVAILABLE),
  };
  let canonical = match media::canonical_mime(&mime, &bytes) {
    Ok(mime) => mime,
    Err(_) => return file_unavailable(StatusCode::GONE),
  };
  let mut response = Response::new(Body::from(bytes));
  *response.status_mut() = StatusCode::OK;
  response
    .headers_mut()
    .insert(header::CONTENT_TYPE, HeaderValue::from_static(canonical));
  response.headers_mut().insert(
    header::CONTENT_DISPOSITION,
    HeaderValue::from_static("attachment; filename=\"comprobante\""),
  );
  response.headers_mut().insert(
    "x-content-type-options",
    HeaderValue::from_static("nosniff"),
  );
  response
    .headers_mut()
    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
  response
}

async fn associate(
  State(state): State<AppState>,
  Path(raw_id): Path<String>,
  Form(form): Form<AssociationForm>,
) -> Response {
  let Ok(receipt_id) = Uuid::parse_str(&raw_id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let Ok(appointment_id) = Uuid::parse_str(&form.appointment_id) else {
    return StatusCode::BAD_REQUEST.into_response();
  };
  let Some(origin) = state.cfg.public_origin.as_deref() else {
    return service_unavailable();
  };
  let tx = match begin(&state).await {
    Ok(tx) => tx,
    Err(response) => return response,
  };
  let contact = match tx
    .query_one(sql(
      "SELECT contact_id FROM receipts WHERE id=$1",
      vec![receipt_id.into()],
    ))
    .await
  {
    Ok(Some(row)) => match row.try_get::<Uuid>("", "contact_id") {
      Ok(contact) => contact,
      Err(_) => {
        let _ = tx.rollback().await;
        return service_unavailable();
      }
    },
    Ok(None) => return finish(tx, receipt_id).await,
    Err(_) => {
      let _ = tx.rollback().await;
      return service_unavailable();
    }
  };
  let result = intake::select_receipt(
    &tx,
    contact,
    receipt_id,
    appointment_id,
    SelectionActor::Doctor,
    origin,
    state.clock.now_millis(),
  )
  .await;
  match result {
    Ok(SelectOutcome::Attached { .. } | SelectOutcome::AlreadySelected | SelectOutcome::Stale) => {
      finish(tx, receipt_id).await
    }
    Err(_) => {
      let _ = tx.rollback().await;
      service_unavailable()
    }
  }
}

async fn verify(State(state): State<AppState>, Path(raw_id): Path<String>) -> Response {
  let Ok(id) = Uuid::parse_str(&raw_id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  run_action(&state, id, DomainAction::Verify).await
}
async fn reject(
  State(state): State<AppState>,
  Path(raw_id): Path<String>,
  Form(form): Form<RejectForm>,
) -> Response {
  let Ok(id) = Uuid::parse_str(&raw_id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let choice = match form.choice.as_str() {
    "receipt" => RejectionChoice::NewReceipt,
    "cancel" => RejectionChoice::Cancel,
    _ => return StatusCode::BAD_REQUEST.into_response(),
  };
  run_action(&state, id, DomainAction::Reject(choice)).await
}

async fn late_outcome(
  State(state): State<AppState>,
  Path(raw_id): Path<String>,
  Form(form): Form<LateForm>,
) -> Response {
  let Ok(id) = Uuid::parse_str(&raw_id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let choice = match form.outcome.as_str() {
    "verified_recovery" => LateChoice::VerifiedRecovery,
    "verified_refund" => LateChoice::VerifiedRefund,
    "clarification" => LateChoice::Clarification,
    _ => return StatusCode::BAD_REQUEST.into_response(),
  };
  run_action(&state, id, DomainAction::Late(choice)).await
}

async fn cancel(State(state): State<AppState>, Path(raw_id): Path<String>) -> Response {
  let Ok(id) = Uuid::parse_str(&raw_id) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  run_action(&state, id, DomainAction::Cancel).await
}

async fn run_action(state: &AppState, receipt: Uuid, action: DomainAction) -> Response {
  let Some(origin) = state.cfg.public_origin.as_deref() else {
    return service_unavailable();
  };
  let tx = match begin(state).await {
    Ok(tx) => tx,
    Err(response) => return response,
  };
  let now = state.clock.now_millis();
  let result = match action {
    DomainAction::Verify => actions::verify(&tx, receipt, origin, now).await,
    DomainAction::Reject(choice) => actions::reject(&tx, receipt, choice, origin, now).await,
    DomainAction::Late(choice) => actions::late_outcome(&tx, receipt, choice, origin, now).await,
    DomainAction::Cancel => actions::cancel(&tx, receipt, origin, now).await,
  };
  match result {
    Ok(_) => finish(tx, receipt).await,
    Err(_) => {
      let _ = tx.rollback().await;
      service_unavailable()
    }
  }
}

async fn begin(state: &AppState) -> Result<sea_orm::DatabaseTransaction, Response> {
  let tx = state
    .db
    .conn
    .begin()
    .await
    .map_err(|_| service_unavailable())?;
  if whatsapp::bound_transaction(&tx).await.is_err() {
    let _ = tx.rollback().await;
    return Err(service_unavailable());
  }
  Ok(tx)
}

async fn finish(tx: sea_orm::DatabaseTransaction, receipt: Uuid) -> Response {
  if tx.commit().await.is_err() {
    return service_unavailable();
  }
  Redirect::to(&format!("/dashboard/payments/receipts/{receipt}")).into_response()
}

async fn load_review(state: &AppState, id: Uuid) -> Result<Option<ReceiptReview>, DbErr> {
  let now = state.clock.now_millis();
  let Some(row) = state
    .db
    .conn
    .query_one(sql(
      "SELECT id,appointment_id,mime,sent_at,arrived_at,timely,late,(bytes IS NOT NULL AND deleted_at IS NULL) AS bytes_available,reviewed_at,review_outcome,clarification_requested_at FROM receipts WHERE id=$1",
      vec![id.into()],
    ))
    .await?
  else {
    return Ok(None);
  };
  let receipt = ReceiptRow::from_query_result(&row, "")?;
  let appointment = match receipt.appointment_id {
    Some(appointment_id) => {
      let Some(appointment) = appointment_store::get(&state.db.conn, appointment_id).await? else {
        return Ok(None);
      };
      let hold_row = state
        .db
        .conn
        .query_one(sql(
          "SELECT COALESCE(bool_or(expires_at IS NULL OR expires_at>$2),false) AS live_hold FROM appointment_holds WHERE appointment_id=$1 AND active",
          vec![appointment_id.into(), now.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("Appointment Hold unavailable".into()))?;
      let live_hold: bool = hold_row.try_get("", "live_hold")?;
      Some(appointment_summary(
        appointment,
        live_hold,
        receipt.reviewed_at.is_none() && receipt.bytes_available,
        receipt.late,
        now,
      ))
    }
    None => None,
  };
  let mut candidates = Vec::new();
  if receipt.appointment_id.is_none() {
    let rows = state
      .db
      .conn
      .query_all(sql(
        "SELECT a.id,a.patient_name,a.service_name,a.starts_at,a.timezone FROM receipt_candidates c JOIN appointments a ON a.id=c.appointment_id WHERE c.receipt_id=$1 ORDER BY a.starts_at,a.id",
        vec![id.into()],
      ))
      .await?;
    for row in rows {
      candidates.push(Candidate {
        appointment_id: row.try_get("", "id")?,
        patient_name: row.try_get("", "patient_name")?,
        service_name: row.try_get("", "service_name")?,
        starts_at: row.try_get("", "starts_at")?,
        timezone: row.try_get("", "timezone")?,
      });
    }
  }
  Ok(Some(ReceiptReview {
    id: receipt.id,
    mime: receipt.mime,
    sent_at: receipt.sent_at,
    arrived_at: receipt.arrived_at,
    timely: receipt.timely,
    late: receipt.late,
    bytes_available: receipt.bytes_available,
    reviewed_at: receipt.reviewed_at,
    review_outcome: receipt.review_outcome,
    clarification_requested_at: receipt.clarification_requested_at,
    appointment,
    candidates,
  }))
}

fn appointment_summary(
  appointment: Appointment,
  live_hold: bool,
  has_evidence: bool,
  late: Option<bool>,
  now: i64,
) -> AppointmentSummary {
  let can_review = has_evidence
    && live_hold
    && late != Some(true)
    && appointment.status == "awaiting_payment"
    && matches!(
      appointment.payment.as_str(),
      "awaiting_receipt" | "under_review"
    );
  let can_late_review = has_evidence
    && appointment.status == "expired"
    && matches!(
      appointment.payment.as_str(),
      "awaiting_receipt" | "under_review" | "rejected"
    );
  let can_cancel = appointment.starts_at > now
    && matches!(
      appointment.status.as_str(),
      "requested" | "awaiting_payment" | "confirmed"
    );
  AppointmentSummary {
    id: appointment.id,
    patient_name: appointment.patient_name,
    service_name: appointment.service_name,
    starts_at: appointment.starts_at,
    timezone: appointment.timezone,
    status: appointment.status,
    payment: appointment.payment,
    can_verify: can_review,
    can_late_review,
    can_cancel,
  }
}

fn file_unavailable(status: StatusCode) -> Response {
  let mut response = (
    status,
    "El comprobante no está disponible. Solicite que el Contact lo envíe otra vez.",
  )
    .into_response();
  response.headers_mut().insert(
    header::CONTENT_TYPE,
    HeaderValue::from_static("text/plain; charset=utf-8"),
  );
  response
    .headers_mut()
    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
  response.headers_mut().insert(
    "x-content-type-options",
    HeaderValue::from_static("nosniff"),
  );
  response
}

fn service_unavailable() -> Response {
  (
    StatusCode::SERVICE_UNAVAILABLE,
    "El servicio no está disponible en este momento.",
  )
    .into_response()
}
#[cfg(test)]
mod tests {
  use super::*;
  use crate::{
    app::tests::with_database,
    modules::doctor::tests::{authenticated_get, field, fixture, post, signed_in, text, NOW},
  };
  use sea_orm::ConnectionTrait;

  async fn seed_receipt(
    db: &crate::database::Db,
    now: i64,
    deadline: i64,
    expiry: i64,
    sent_at: i64,
    arrived_at: i64,
  ) -> (Uuid, Uuid, Vec<u8>) {
    let contact = Uuid::now_v7();
    let appointment = Uuid::now_v7();
    let receipt = Uuid::now_v7();
    let inbox = format!("receipt-test-{contact}");
    let target = "15550001111@c.us";
    let bytes = b"%PDF-1.7\nsynthetic receipt".to_vec();
    db.conn
      .execute(sql(
        "INSERT INTO contacts(id,created_at) VALUES ($1,$2)",
        vec![contact.into(), now.into()],
      ))
      .await
      .unwrap();
    db.conn
      .execute(sql(
        "INSERT INTO inbox_messages(message_id,original_chat_identity,server_timestamp,arrived_at,raw_envelope,preparation_state,message_kind,prepared_at,contact_id) VALUES ($1,$2,$3,to_timestamp($4::double precision/1000.0),$5,'ready_for_conversation','media',to_timestamp($4::double precision/1000.0),$6)",
        vec![
          inbox.clone().into(),
          target.into(),
          sent_at.div_euclid(1000).into(),
          arrived_at.into(),
          b"{}".to_vec().into(),
          contact.into(),
        ],
      ))
      .await
      .unwrap();
    db.conn
      .execute(sql(
        "INSERT INTO contact_aliases(jid,contact_id,inbox_message_id) VALUES ($1,$2,$3)",
        vec![target.into(), contact.into(), inbox.clone().into()],
      ))
      .await
      .unwrap();
    let starts_at = now + 7_200_000;
    db.conn
      .execute(sql(
        "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,revision,created_at,updated_at) VALUES ($1,$2,'Paciente de prueba',$3,'Consulta',45,100000,'video',$4,$5,'America/Tijuana','',0,0,'awaiting_payment','awaiting_receipt','not_yet','Banco de prueba','Beneficiario de prueba','012345678901234567','REF123',$6,1,$7,$7)",
        vec![
          appointment.into(),
          contact.into(),
          Uuid::now_v7().into(),
          starts_at.into(),
          (starts_at + 2_700_000).into(),
          deadline.into(),
          now.into(),
        ],
      ))
      .await
      .unwrap();
    db.conn
      .execute(sql(
        "INSERT INTO appointment_holds(id,appointment_id,starts_at,ends_at,active,expires_at) VALUES ($1,$2,$3,$4,true,$5)",
        vec![
          Uuid::now_v7().into(),
          appointment.into(),
          (starts_at - 900_000).into(),
          (starts_at + 3_600_000).into(),
          expiry.into(),
        ],
      ))
      .await
      .unwrap();
    db.conn
      .execute(sql(
        "INSERT INTO appointment_jobs(id,appointment_id,kind,due_at,dedupe_key) VALUES ($1,$2,'expiry',$3,$4)",
        vec![
          Uuid::now_v7().into(),
          appointment.into(),
          expiry.into(),
          format!("{appointment}:1:expiry:deadline").into(),
        ],
      ))
      .await
      .unwrap();
    db.conn
      .execute(sql(
        "INSERT INTO receipts(id,contact_id,inbox_message_id,appointment_id,mime,bytes,sent_at,arrived_at,timely,late,created_at) VALUES ($1,$2,$3,$4,'application/pdf',$5,$6,$7,$8,$9,$10)",
        vec![
          receipt.into(),
          contact.into(),
          inbox.into(),
          appointment.into(),
          bytes.clone().into(),
          sent_at.into(),
          arrived_at.into(),
          (sent_at <= deadline).into(),
          (arrived_at >= expiry).into(),
          now.into(),
        ],
      ))
      .await
      .unwrap();
    (receipt, appointment, bytes)
  }

  #[tokio::test]
  #[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
  async fn authenticated_review_keeps_after_deadline_receipt_secure_and_idempotent_before_expiry() {
    with_database(|db| async move {
      let (_, _, app, totp) = fixture(&db).await;
      let now = (NOW + 30) * 1000;
      let deadline = now - 60_000;
      let expiry = now + 60_000;
      let (receipt_id, appointment_id, bytes) =
        seed_receipt(&db, now, deadline, expiry, deadline + 1, now - 1_000).await;
      let session = signed_in(&app, &totp, NOW + 30).await;
      let path = format!("/dashboard/payments/receipts/{receipt_id}");
      let file_path = format!("{path}/file");

      let file = authenticated_get(&app, &file_path, &session).await;
      assert_eq!(file.status(), StatusCode::OK);
      assert_eq!(file.headers()[header::CONTENT_TYPE], "application/pdf");
      assert_eq!(file.headers()[header::CONTENT_DISPOSITION], "attachment; filename=\"comprobante\"");
      assert_eq!(file.headers()[header::CACHE_CONTROL], "no-store");
      assert_eq!(file.headers()["x-content-type-options"], "nosniff");
      assert_eq!(axum::body::to_bytes(file.into_body(),whatsapp::client::MAX_MEDIA_BYTES).await.unwrap().to_vec(),bytes);

      let page = authenticated_get(&app, &path, &session).await;
      let page = text(page).await;
      assert!(page.contains("Enviado después del plazo según WhatsApp"));
      assert!(page.contains("Descargar comprobante"));
      assert!(page.contains("Confirmar pago recibido"));
      let dashboard = authenticated_get(&app, "/dashboard", &session).await;
      let csrf = field(&text(dashboard).await, "csrf_token");

      let denied = post(
        &app,
        &format!("{path}/verify"),
        Some(&session),
        Some("https://attacker.example"),
        &[("csrf_token", &csrf)],
      )
      .await;
      assert_eq!(denied.status(), StatusCode::FORBIDDEN);
      let invalid_csrf = post(
        &app,
        &format!("{path}/verify"),
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", "incorrect")],
      )
      .await;
      assert_eq!(invalid_csrf.status(), StatusCode::FORBIDDEN);

      for _ in 0..2 {
        let response = post(
          &app,
          &format!("{path}/verify"),
          Some(&session),
          Some("https://practice.test"),
          &[("csrf_token", &csrf)],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
      }
      let row = db
        .conn
        .query_one(sql(
          "SELECT a.status,a.payment,a.calendar_sync,a.event_id,a.revision,h.expires_at,r.reviewed_at,r.review_outcome,(SELECT count(*) FROM appointment_history WHERE appointment_id=a.id AND kind='payment_verified') AS verified_history,(SELECT count(*) FROM outbox_messages WHERE dedupe_key=$2) AS verified_messages,(SELECT count(*) FROM appointment_jobs WHERE appointment_id=a.id AND kind='calendar_sync' AND finished_at IS NULL) AS calendar_jobs FROM appointments a JOIN appointment_holds h ON h.appointment_id=a.id JOIN receipts r ON r.appointment_id=a.id WHERE a.id=$1",
          vec![appointment_id.into(), format!("receipt:{receipt_id}:verified").into()],
        ))
        .await
        .unwrap()
        .unwrap();
      assert_eq!(row.try_get::<String>("", "status").unwrap(), "confirmed");
      assert_eq!(row.try_get::<String>("", "payment").unwrap(), "verified");
      assert_eq!(row.try_get::<String>("", "calendar_sync").unwrap(), "pending");
      assert_eq!(row.try_get::<i64>("", "revision").unwrap(), 2);
      assert_eq!(row.try_get::<Option<i64>>("", "expires_at").unwrap(), None);
      assert!(row.try_get::<String>("", "event_id").unwrap().len() == 32);
      assert!(row.try_get::<Option<i64>>("", "reviewed_at").unwrap().is_some());
      assert_eq!(row.try_get::<String>("", "review_outcome").unwrap(), "verified");
      assert_eq!(row.try_get::<i64>("", "verified_history").unwrap(), 1);
      assert_eq!(row.try_get::<i64>("", "verified_messages").unwrap(), 1);
      assert_eq!(row.try_get::<i64>("", "calendar_jobs").unwrap(), 1);
    })
    .await;
  }

  #[tokio::test]
  #[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
  async fn expiry_winning_prevents_receipt_verification_from_resurrecting_the_appointment() {
    with_database(|db| async move {
      let (_, _, app, totp) = fixture(&db).await;
      let now = (NOW + 30) * 1000;
      let deadline = now - 300_000;
      let expiry = now;
      let (receipt_id, appointment_id, _) =
        seed_receipt(&db, now, deadline, expiry, deadline + 1, expiry - 1_000).await;
      let session = signed_in(&app, &totp, NOW + 30).await;
      let dashboard = authenticated_get(&app, "/dashboard", &session).await;
      let csrf = field(&text(dashboard).await, "csrf_token");
      let response = post(
        &app,
        &format!("/dashboard/payments/receipts/{receipt_id}/verify"),
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", &csrf)],
      )
      .await;
      assert_eq!(response.status(), StatusCode::SEE_OTHER);
      let row = db
        .conn
        .query_one(sql(
          "SELECT a.status,a.payment,h.active,r.reviewed_at,r.late,(SELECT count(*) FROM appointment_history WHERE appointment_id=a.id AND kind='payment_verified') AS verified_history FROM appointments a JOIN appointment_holds h ON h.appointment_id=a.id JOIN receipts r ON r.appointment_id=a.id WHERE a.id=$1",
          vec![appointment_id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
      assert_eq!(row.try_get::<String>("", "status").unwrap(), "expired");
      assert_eq!(row.try_get::<String>("", "payment").unwrap(), "awaiting_receipt");
      assert!(!row.try_get::<bool>("", "active").unwrap());
      assert_eq!(row.try_get::<Option<i64>>("", "reviewed_at").unwrap(), None);
      assert_eq!(row.try_get::<Option<bool>>("", "late").unwrap(), Some(true));
      assert_eq!(row.try_get::<i64>("", "verified_history").unwrap(), 0);
    })
    .await;
  }
}
