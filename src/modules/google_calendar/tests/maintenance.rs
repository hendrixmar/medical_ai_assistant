use super::fixture::Fixture;
use super::*;
use crate::{database::Db, modules::google_calendar::maintenance};

fn sql(query: &str, values: Vec<sea_orm::Value>) -> Statement {
  Statement::from_sql_and_values(DbBackend::Postgres, query, values)
}

async fn snapshot(db: &Db, query: &str) -> String {
  db.conn
    .query_one(sql(query, vec![]))
    .await
    .unwrap()
    .unwrap()
    .try_get::<String>("", "snapshot")
    .unwrap()
}

const APPOINTMENT_SNAPSHOT: &str = "SELECT id::text||':'||status||':'||payment||':'||coalesce(event_id,'')||':'||coalesce(booking_calendar,'') AS snapshot FROM appointments";
const PROBE_SNAPSHOT: &str = "SELECT id::text||':'||phase||':'||cleanup_outcome||':'||calendar_id AS snapshot FROM google_probes";

async fn seed_appointment_and_probe(db: &Db) {
  let contact = uuid::Uuid::now_v7();
  let appointment = uuid::Uuid::now_v7();
  db.conn
    .execute(sql(
      "INSERT INTO contacts(id,created_at) VALUES($1,1)",
      vec![contact.into()],
    ))
    .await
    .unwrap();
  db.conn.execute(sql("INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,booking_calendar,google_generation,google_selection_version,event_id,created_at,updated_at) VALUES($1,$2,'Paciente de prueba',$3,'Consulta',30,10000,'video',2000000000,2000001800,'America/Mexico_City','',0,0,'confirmed','verified','pending','secondary@example.invalid',1,1,'0123456789abcdef0123456789abcdef',1,1)", vec![appointment.into(), contact.into(), uuid::Uuid::now_v7().into()])).await.unwrap();
  let marker: String = db
    .conn
    .query_one(sql(
      "SELECT deployment_marker AS snapshot FROM google_connection WHERE id=1",
      vec![],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "snapshot")
    .unwrap();
  db.conn.execute(sql("INSERT INTO google_probes(id,generation,selection_version,calendar_id,calendar_label,event_id,conference_request_id,deployment_marker,probe_marker,started_at,event_start,due_at,phase,cleanup_outcome) VALUES($1,1,1,'secondary@example.invalid','Secundario','0123456789abcdef0123456789abcdef','synthetic-cleanup-request',$2,'synthetic-probe-marker',1,1,1,'attention','unresolved')", vec![uuid::Uuid::now_v7().into(), marker.into()])).await.unwrap();
}

fn google_cfg_with_key(
  base: &crate::common::config::Config,
  key: [u8; 32],
) -> crate::common::config::Config {
  use crate::common::config::{Configuration, Environment};
  let mut google = super::super::config::GoogleConfig::parse(
    Some("client"),
    Some("SYNTHETIC_SECRET"),
    Some(&base64::engine::general_purpose::STANDARD.encode(key)),
    Some("https://practice.test"),
  )
  .unwrap()
  .unwrap();
  google.test_endpoints = base.google.as_ref().and_then(|g| g.test_endpoints.clone());
  Arc::new(Configuration {
    env: Environment::Production,
    listen_address: "127.0.0.1:0".parse().unwrap(),
    app_port: 0,
    swagger_endpoint: "/docs".into(),
    db_dsn: base.db_dsn.clone(),
    db_pool_max_size: 2,
    db_timeout: 5,
    db_run_migrations: true,
    bcrypt_cost: 4,
    public_origin: Some("https://practice.test".into()),
    doctor_totp_key: Some([42; 32]),
    waha: None,
    google: Some(google),
    claude_api_key: None,
    claude_model: None,
    claude_test_endpoint: None,
    waha_takeover_enabled: false,
    waha_outbound_enabled: false,
    waha_contact_replies_per_hour: 20,
  })
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn google_rotation_reencrypts_and_supersedes_pending_attempts() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    assert_eq!(f.select().await.status(), 303);
    seed_appointment_and_probe(&db).await;
    let _state = f.state().await; // leaves one pending OAuth attempt
    let before = snapshot(&db, "SELECT generation::text||':'||token_version::text||':'||coalesce(booking_calendar,'')||':'||blocking_calendars::text||':'||status AS snapshot FROM google_connection WHERE id=1").await;
    let appointment_before = snapshot(&db, APPOINTMENT_SNAPSHOT).await;
    let probe_before = snapshot(&db, PROBE_SNAPSHOT).await;
    let ciphertext_before = snapshot(&db, "SELECT encode(credential_ciphertext,'hex') AS snapshot FROM google_connection WHERE id=1").await;

    // Wrong current key rolls back: credentials and pending attempt stay.
    let wrong_cfg = google_cfg_with_key(&f.cfg, [99; 32]);
    assert!(maintenance::rotate_key(&db, &wrong_cfg, &[72; 32]).await.is_err());
    assert_eq!(snapshot(&db, "SELECT encode(credential_ciphertext,'hex') AS snapshot FROM google_connection WHERE id=1").await, ciphertext_before);
    assert_eq!(snapshot(&db, "SELECT count(*)::text AS snapshot FROM google_auth_attempts WHERE outcome='pending'").await, "1");

    // New key equal to old / Doctor key is rejected.
    assert!(maintenance::rotate_key(&db, &f.cfg, &[71; 32]).await.is_err());
    assert!(maintenance::rotate_key(&db, &f.cfg, &[42; 32]).await.is_err());

    assert_eq!(maintenance::rotate_key(&db, &f.cfg, &[72; 32]).await.unwrap(), 1);
    assert_eq!(snapshot(&db, "SELECT generation::text||':'||token_version::text||':'||coalesce(booking_calendar,'')||':'||blocking_calendars::text||':'||status AS snapshot FROM google_connection WHERE id=1").await, before);
    assert_eq!(snapshot(&db, APPOINTMENT_SNAPSHOT).await, appointment_before);
    assert_eq!(snapshot(&db, PROBE_SNAPSHOT).await, probe_before);
    assert_eq!(snapshot(&db, "SELECT count(*)::text AS snapshot FROM google_auth_attempts WHERE outcome='pending'").await, "0");
    assert_eq!(snapshot(&db, "SELECT count(*)::text AS snapshot FROM google_auth_attempts WHERE outcome='superseded' AND verifier_ciphertext IS NULL").await, "1");

    // New key decrypts, old key does not (boolean only; bytes never printed).
    let row = f.row(&db).await;
    let generation: i64 = row.try_get("", "generation").unwrap();
    let ct: Vec<u8> = row.try_get("", "credential_ciphertext").unwrap();
    let nonce: Vec<u8> = row.try_get("", "credential_nonce").unwrap();
    let plain = super::super::crypto::decrypt(&[72; 32], "credentials", "1", generation, &ct, &nonce).unwrap();
    assert!(String::from_utf8_lossy(&plain).contains(ACCESS));
    assert!(super::super::crypto::decrypt(&[71; 32], "credentials", "1", generation, &ct, &nonce).is_err());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn google_reconnect_only_discards_tokens_and_reconnect_reselects() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    assert_eq!(f.select().await.status(), 303);
    seed_appointment_and_probe(&db).await;
    let _state = f.state().await;
    let meta_before = snapshot(&db, "SELECT generation::text||':'||token_version::text||':'||coalesce(booking_calendar,'')||':'||blocking_calendars::text AS snapshot FROM google_connection WHERE id=1").await;
    let appointment_before = snapshot(&db, APPOINTMENT_SNAPSHOT).await;
    let probe_before = snapshot(&db, PROBE_SNAPSHOT).await;
    let calls_before = f.provider.calls.lock().await.len();

    // No old-key decrypt and no provider call.
    maintenance::reconnect_only(&db).await.unwrap();
    assert_eq!(f.provider.calls.lock().await.len(), calls_before);
    let row = f.row(&db).await;
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "reauthorize");
    assert_eq!(row.try_get::<String>("", "notice").unwrap(), "reauthorize");
    assert!(row.try_get::<Option<Vec<u8>>>("", "credential_ciphertext").unwrap().is_none());
    assert!(row.try_get::<Option<Vec<u8>>>("", "credential_nonce").unwrap().is_none());
    assert!(row.try_get::<Option<i64>>("", "credential_expires_at").unwrap().is_none());
    assert_eq!(snapshot(&db, "SELECT generation::text||':'||token_version::text||':'||coalesce(booking_calendar,'')||':'||blocking_calendars::text AS snapshot FROM google_connection WHERE id=1").await, meta_before);
    assert_eq!(snapshot(&db, APPOINTMENT_SNAPSHOT).await, appointment_before);
    assert_eq!(snapshot(&db, PROBE_SNAPSHOT).await, probe_before);
    assert_eq!(snapshot(&db, "SELECT count(*)::text AS snapshot FROM google_auth_attempts WHERE outcome='pending'").await, "0");

    // Honest reconnect page: reauthorization required, never a claimed revocation.
    let page = text(authenticated_get(&f.app, "/settings/google", &f.session).await).await;
    assert!(page.contains("Necesitas volver a conectar Google"));
    assert!(!page.contains("revocaste"));

    // Install a fresh grant under the new key, then explicitly reselect.
    let cfg_new = google_cfg_with_key(&f.cfg, [73; 32]);
    let app = crate::app::router_with_clock(cfg_new, db.clone(), f.clock.clone());
    let csrf = field(&text(authenticated_get(&app, "/settings", &f.session).await).await, "csrf_token");
    let response = post(&app, "/settings/google/connect", Some(&f.session), Some("https://practice.test"), &[("csrf_token", &csrf)]).await;
    assert_eq!(response.status(), 303);
    let state = reqwest::Url::parse(response.headers()["location"].to_str().unwrap())
      .unwrap()
      .query_pairs()
      .find(|(k, _)| k == "state")
      .unwrap()
      .1
      .into_owned();
    let response = authenticated_get(&app, &format!("/settings/google/callback?state={state}&code=SYNTHETIC_CODE"), &f.session).await;
    assert_eq!(response.headers()["location"], "/settings/google");
    let row = f.row(&db).await;
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "connected");
    assert!(row.try_get::<Option<String>>("", "booking_calendar").unwrap().is_none(), "install requires reselection");
    let generation: i64 = row.try_get("", "generation").unwrap();
    let selection_version: i64 = row.try_get("", "selection_version").unwrap();
    let ct: Vec<u8> = row.try_get("", "credential_ciphertext").unwrap();
    let nonce: Vec<u8> = row.try_get("", "credential_nonce").unwrap();
    assert!(super::super::crypto::decrypt(&[73; 32], "credentials", "1", generation, &ct, &nonce).is_ok());
    assert!(super::super::crypto::decrypt(&[71; 32], "credentials", "1", generation, &ct, &nonce).is_err());
    let generation_s = generation.to_string();
    let version_s = selection_version.to_string();
    let response = post(
      &app,
      "/settings/google/calendars",
      Some(&f.session),
      Some("https://practice.test"),
      &[
        ("csrf_token", &csrf),
        ("generation", &generation_s),
        ("selection_version", &version_s),
        ("booking_calendar", "secondary@example.invalid"),
      ],
    )
    .await;
    assert_eq!(response.status(), 303);
    let row = f.row(&db).await;
    assert_eq!(row.try_get::<Option<String>>("", "booking_calendar").unwrap().as_deref(), Some("secondary@example.invalid"));
    assert_eq!(snapshot(&db, APPOINTMENT_SNAPSHOT).await, appointment_before);
    assert_eq!(snapshot(&db, PROBE_SNAPSHOT).await, probe_before);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn google_rotation_without_credentials_supersedes_attempts() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    let _state = f.state().await;
    assert_eq!(
      maintenance::rotate_key(&db, &f.cfg, &[72; 32])
        .await
        .unwrap(),
      0
    );
    assert_eq!(
      snapshot(
        &db,
        "SELECT count(*)::text AS snapshot FROM google_auth_attempts WHERE outcome='pending'"
      )
      .await,
      "0"
    );
  })
  .await;
}
