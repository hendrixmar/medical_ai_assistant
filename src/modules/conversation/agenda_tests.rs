use super::*;
use crate::{
  app::tests::with_database,
  modules::{
    doctor::{
      tests::{fixture, NOW},
      Clock,
    },
    whatsapp::config::WahaConfig,
  },
};
use axum::{
  body::{to_bytes, Body},
  extract::State,
  http::{Request, StatusCode},
  response::Response,
  Router,
};
use chrono::TimeZone;
use hmac::{Hmac, Mac};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use sha2::Sha512;
use std::sync::Arc;
use tokio::{sync::Mutex, task::JoinHandle};
use tower::ServiceExt;
use uuid::Uuid;

const WEBHOOK_KEY: &str = "synthetic-agenda-HMAC-key-with-at-least-32-bytes";
const DOCTOR: &str = "525512345678@c.us";
const PRACTICE: &str = "525555555555@c.us";
const QUESTION: &str = "¿qué tengo el martes 19 de enero?";

type ClaudeRequests = Arc<Mutex<Vec<Value>>>;

async fn fake_claude(
  State(requests): State<ClaudeRequests>,
  request: Request<Body>,
) -> Response<Body> {
  let body = to_bytes(request.into_body(), 16 * 1024).await.unwrap();
  let value = serde_json::from_slice(&body).unwrap();
  requests.lock().await.push(value);
  Response::builder()
    .status(StatusCode::OK)
    .header("content-type", "application/json")
    .body(Body::from(
      json!({"content":[{"type":"text","text":r#"{"intent":"agenda","date_phrase":"martes 19 de enero","time_phrase":null,"uncertain":false}"#}]}).to_string(),
    ))
    .unwrap()
}

async fn start_claude() -> (String, ClaudeRequests, JoinHandle<()>) {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
  let requests = Arc::new(Mutex::new(Vec::new()));
  let app = Router::new()
    .fallback(fake_claude)
    .with_state(requests.clone());
  let task = tokio::spawn(async move {
    axum::serve(listener, app).await.unwrap();
  });
  (endpoint, requests, task)
}

fn incoming(id: &str, sender: &str, body: &str) -> Value {
  json!({
    "id": format!("event_{id}"),
    "event": "message",
    "session": "default",
    "engine": "GOWS",
    "me": {"id": PRACTICE},
    "payload": {
      "id": format!("false_{sender}_{id}"),
      "from": sender,
      "fromMe": false,
      "timestamp": NOW,
      "body": body,
      "hasMedia": false,
      "_data": {"Message": {"conversation": body}}
    }
  })
}

async fn process(
  db: &crate::database::Db,
  app: &Router,
  clock: &dyn Clock,
  cfg: &crate::common::config::Config,
  id: &str,
  sender: &str,
  body: &str,
) {
  let raw = serde_json::to_vec(&incoming(id, sender, body)).unwrap();
  let mut mac = Hmac::<Sha512>::new_from_slice(WEBHOOK_KEY.as_bytes()).unwrap();
  mac.update(&raw);
  let signature: String = mac
    .finalize()
    .into_bytes()
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect();
  let response = app
    .clone()
    .oneshot(
      Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/waha")
        .header("x-webhook-hmac", signature)
        .body(Body::from(raw))
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  assert!(matches!(
    crate::modules::whatsapp::jobs::run_once(db, clock)
      .await
      .unwrap(),
    crate::modules::whatsapp::jobs::Outcome::Prepared(_)
  ));
  assert_eq!(
    crate::modules::conversation::jobs::run_once(db, clock, cfg, None)
      .await
      .unwrap(),
    crate::modules::conversation::jobs::Outcome::Processed
  );
}

#[test]
fn parser_routing_is_limited_to_read_only_agenda_questions() {
  assert!(!needs_parser("agenda"));
  assert!(!needs_parser("agenda mañana"));
  assert!(!needs_parser("¿qué tengo mañana?"));
  assert!(!needs_parser("agenda esta semana"));
  assert!(!needs_parser("agenda 2027-01-15"));
  assert!(!needs_parser("¿qué tengo el 2027-01-15?"));
  assert!(needs_parser("¿qué tengo el martes 20 de octubre?"));
  assert!(!needs_parser("cancela la cita"));
  assert!(!needs_parser(
    "¿qué tengo el martes 20 de octubre? cancela la cita"
  ));
  assert!(!needs_parser("¿qué tengo el 123456?"));
  assert!(!needs_parser(&"x".repeat(641)));
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn agenda_uses_practice_local_day_and_reports_persisted_states_read_only() {
  with_database(|db| async move {
    let (_, _, _, _) = fixture(&db).await;
    let now = NOW * 1000;
    db.conn
      .execute(sql(
        "UPDATE practice SET timezone=$1 WHERE id=1",
        vec!["America/Tijuana".to_owned().into()],
      ))
      .await
      .unwrap();
    let contact = Uuid::now_v7();
    db.conn
      .execute(sql(
        "INSERT INTO contacts(id,created_at) VALUES ($1,$2)",
        vec![contact.into(), now.into()],
      ))
      .await
      .unwrap();
    let in_day = Uuid::now_v7();
    let next_day = Uuid::now_v7();
    for (id, starts_at, status, payment, calendar) in [
      (
        in_day,
        now + 3_600_000,
        "awaiting_payment",
        "under_review",
        "failed",
      ),
      (
        next_day,
        now + 86_400_000,
        "confirmed",
        "verified",
        "synced",
      ),
    ] {
      db.conn
        .execute(sql(
          "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,revision,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,45,100000,'video',$6,$7,'America/Tijuana','Consultorio de prueba',0,0,$8,$9,$10,1,$11,$11)",
          vec![
            id.into(),
            contact.into(),
            "Paciente de prueba".to_owned().into(),
            Uuid::now_v7().into(),
            "Consulta general".to_owned().into(),
            starts_at.into(),
            (starts_at + 2_700_000).into(),
            status.to_owned().into(),
            payment.to_owned().into(),
            calendar.to_owned().into(),
            now.into(),
          ],
        ))
        .await
        .unwrap();
    }

    let today_answer = answer(&db.conn, now, Question::Text("agenda hoy"))
      .await
      .unwrap()
      .unwrap();
    assert!(today_answer.contains("viernes 15/01/2027 (America/Tijuana)"));
    assert!(today_answer.contains("Paciente de prueba"));
    assert!(today_answer.contains("pendiente de pago"));
    assert!(today_answer.contains("comprobante en revisión"));
    assert!(today_answer.contains("sincronización fallida"));
    assert!(!today_answer.contains("16/01/2027"));
    assert!(!today_answer.contains("No hay citas registradas"));
    let selected_date = answer(
      &db.conn,
      now,
      Question::Date(chrono::NaiveDate::from_ymd_opt(2027, 1, 15).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(selected_date.contains("Paciente de prueba"));
    let unchanged = db
      .conn
      .query_one(sql(
        "SELECT (SELECT count(*) FROM appointments) AS appointments,(SELECT count(*) FROM appointment_history) AS history,(SELECT count(*) FROM outbox_messages) AS outbox",
        vec![],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(unchanged.try_get::<i64>("", "appointments").unwrap(), 2);
    assert_eq!(unchanged.try_get::<i64>("", "history").unwrap(), 0);
    assert_eq!(unchanged.try_get::<i64>("", "outbox").unwrap(), 0);

    db.conn
      .execute_unprepared("UPDATE practice SET timezone=NULL WHERE id=1")
      .await
      .unwrap();
    let no_timezone = answer(&db.conn, now, Question::Text("agenda hoy"))
      .await
      .unwrap()
      .unwrap();
    assert!(no_timezone.contains("configura explícitamente la zona horaria"));
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn verified_doctor_agenda_is_database_rendered_read_only_and_private_from_contacts() {
  with_database(|db| async move {
    let (mut cfg, clock, initial_app, _) = fixture(&db).await;
    drop(initial_app);
    let (endpoint, requests, provider_task) = start_claude().await;
    let config = Arc::get_mut(&mut cfg).unwrap();
    config.waha = WahaConfig::parse(
      Some("http://127.0.0.1:1"),
      Some("synthetic-api-key"),
      Some(WEBHOOK_KEY),
    )
    .unwrap();
    config.claude_api_key = Some("synthetic-claude-key".into());
    config.claude_model = Some("claude-test-model".into());
    config.claude_test_endpoint = Some(endpoint);
    let app = crate::app::router_with_clock(cfg.clone(), db.clone(), clock.clone());
    let now = NOW * 1000;

    process(&db, &app, clock.as_ref(), &cfg, "PROOF", DOCTOR, "Hola").await;
    let proof_inbox = format!("false_{DOCTOR}_PROOF");
    let doctor: Uuid = db
      .conn
      .query_one(sql(
        "SELECT contact_id FROM contact_aliases WHERE jid=$1",
        vec![DOCTOR.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "contact_id")
      .unwrap();
    db.conn.execute(sql("UPDATE practice SET timezone=$1 WHERE id=1", vec!["America/Tijuana".to_owned().into()]))
      .await
      .unwrap();
    db.conn.execute(sql("UPDATE doctor_number_state SET verified_phone=$1,original_target=$2,contact_id=$3,inbox_message_id=$4,verified_at=$5,practice_phone=$6 WHERE id=1", vec![
      DOCTOR.into(),
      DOCTOR.into(),
      doctor.into(),
      proof_inbox.clone().into(),
      now.into(),
      PRACTICE.into(),
    ]))
      .await
      .unwrap();

    let patient_contact = Uuid::now_v7();
    db.conn.execute(sql("INSERT INTO contacts(id,created_at) VALUES ($1,$2)", vec![patient_contact.into(), now.into()]))
      .await
      .unwrap();
    let appointment = Uuid::now_v7();
    let local_start = chrono::NaiveDate::from_ymd_opt(2027, 1, 19)
      .unwrap()
      .and_hms_opt(10, 0, 0)
      .unwrap();
    let starts_at = chrono_tz::America::Tijuana
      .from_local_datetime(&local_start)
      .single()
      .unwrap()
      .timestamp_millis();
    db.conn.execute(sql("INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,revision,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,45,100000,'video',$6,$7,'America/Tijuana','Consultorio de prueba',0,0,'awaiting_payment','under_review','failed',1,$8,$8)", vec![
      appointment.into(),
      patient_contact.into(),
      "Paciente de prueba".to_owned().into(),
      Uuid::now_v7().into(),
      "Consulta general".to_owned().into(),
      starts_at.into(),
      (starts_at + 2_700_000).into(),
      now.into(),
    ]))
      .await
      .unwrap();

    process(&db, &app, clock.as_ref(), &cfg, "AGENDA", DOCTOR, QUESTION).await;
    let answer: String = db
      .conn
      .query_one(sql(
        "SELECT rendered_text FROM outbox_messages WHERE contact_id=$1 AND purpose='doctor_notification' ORDER BY created_at DESC LIMIT 1",
        vec![doctor.into()],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "rendered_text")
      .unwrap();
    for fact in [
      "Agenda para martes 19/01/2027 (America/Tijuana)",
      "10:00",
      "Paciente de prueba",
      "Consulta general",
      "formato: video",
      "estado: pendiente de pago",
      "comprobante en revisión",
      "sincronización fallida",
    ] {
      assert!(answer.contains(fact), "missing database fact: {fact}");
    }
    assert!(!answer.contains("Paciente inventado"));

    let rebound = "525599900001@c.us";
    db.conn.execute(sql("INSERT INTO contact_aliases(jid,contact_id,inbox_message_id) VALUES ($1,$2,$3)", vec![rebound.into(), doctor.into(), proof_inbox.into()]))
      .await
      .unwrap();
    process(&db, &app, clock.as_ref(), &cfg, "REBIND", rebound, QUESTION).await;
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      "CONTACT",
      "525599900002@c.us",
      QUESTION,
    )
    .await;
    for target in [rebound, "525599900002@c.us"] {
      let response: String = db
        .conn
        .query_one(sql(
          "SELECT rendered_text FROM outbox_messages WHERE original_target=$1 ORDER BY created_at DESC LIMIT 1",
          vec![target.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "rendered_text")
        .unwrap();
      for fact in [
        "Paciente de prueba",
        "Consulta general",
        "Agenda para martes",
        "formato: video",
      ] {
        assert!(!response.contains(fact));
      }
    }

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
      let sanitized = request["messages"][0]["content"].as_str().unwrap();
      assert!(sanitized.contains("martes 19 de enero"));
      assert!(!sanitized.contains("Paciente de prueba"));
      assert!(!sanitized.contains("Consulta general"));
    }
    drop(requests);

    let state = db
      .conn
      .query_one(sql(
        "SELECT (SELECT count(*) FROM appointments) AS appointments,(SELECT count(*) FROM appointment_history) AS history,status,payment,calendar_sync,format,revision,starts_at,cancelled_at FROM appointments WHERE id=$1",
        vec![appointment.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(state.try_get::<i64>("", "appointments").unwrap(), 1);
    assert_eq!(state.try_get::<i64>("", "history").unwrap(), 0);
    assert_eq!(
      state.try_get::<String>("", "status").unwrap(),
      "awaiting_payment"
    );
    assert_eq!(state.try_get::<String>("", "payment").unwrap(), "under_review");
    assert_eq!(state.try_get::<String>("", "calendar_sync").unwrap(), "failed");
    assert_eq!(state.try_get::<String>("", "format").unwrap(), "video");
    assert_eq!(state.try_get::<i64>("", "revision").unwrap(), 1);
    assert_eq!(state.try_get::<i64>("", "starts_at").unwrap(), starts_at);
    assert!(state.try_get::<Option<i64>>("", "cancelled_at").unwrap().is_none());
    provider_task.abort();
  })
  .await;
}
