use super::*;
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use sha2::Sha512;

#[test]
fn published_research_vector_uses_literal_key_and_raw_utf8_bytes() {
  let raw = include_bytes!("phone-message.json");
  let mut headers = axum::http::HeaderMap::new();
  headers.insert("x-webhook-hmac", "3805f429fdd8c7d5938ee1e59a9c0cb0f253226a341efab0a61599834234eaaa22ce4058a9c5462ea911a1dd6f320130b127c2e05c4634aed271892e46be11b6".parse().unwrap());
  assert!(super::super::inbox::authenticate(
    &headers,
    raw,
    "fixture-only-not-a-real-secret"
  ));
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn signatures_validate_raw_bytes_before_parse_and_effects() {
  with_database(|db| async move {
    let (app, _) = setup(&db).await;
    let raw = serde_json::to_vec(&envelope("signed")).unwrap();
    let valid = signature(&raw, KEY);
    for headers in [
      vec![],
      vec![("x-webhook-hmac", "")],
      vec![("x-webhook-hmac", "abc")],
      vec![("x-webhook-hmac", "not-hex")],
      vec![("x-webhook-hmac", &valid), ("x-webhook-hmac", &valid)],
      vec![
        ("x-webhook-hmac", &valid),
        ("x-webhook-hmac-algorithm", "sha256"),
      ],
      vec![
        ("x-webhook-hmac", &valid),
        ("x-webhook-hmac-algorithm", "sha512"),
        ("x-webhook-hmac-algorithm", "sha512"),
      ],
    ] {
      assert_eq!(send(&app, &raw, &headers).await.status(), 401);
    }
    for invalid in [
      "0".repeat(128),
      "G".repeat(128),
      valid.to_uppercase(),
      format!("sha512={valid}"),
    ] {
      assert_eq!(
        send(&app, &raw, &[("x-webhook-hmac", &invalid)])
          .await
          .status(),
        401
      );
    }
    let mut tampered = raw.clone();
    tampered.push(b' ');
    assert_eq!(
      send(&app, &tampered, &[("x-webhook-hmac", &valid)])
        .await
        .status(),
      401
    );
    assert_eq!(
      send(&app, b"not JSON", &[("x-webhook-hmac", &valid)])
        .await
        .status(),
      401
    );
    assert_eq!(
      send(
        &app,
        b"not JSON",
        &[("x-webhook-hmac", &signature(b"not JSON", KEY))]
      )
      .await
      .status(),
      400
    );
    assert_eq!(count(&db, "inbox_messages").await, 0);
    assert_eq!(
      send(
        &app,
        &tampered,
        &[
          ("x-webhook-hmac", &signature(&tampered, KEY)),
          ("x-webhook-hmac-algorithm", "sha512")
        ]
      )
      .await
      .status(),
      200
    );
    assert_eq!(send(&app, &vec![b' '; 2097153], &[]).await.status(), 413);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn only_direct_incoming_messages_create_work_and_lid_is_not_a_phone() {
  with_database(|db| async move {
    let (app, _) = setup(&db).await;
    for (path, value) in [("/event", json!("call.received")), ("/event", json!("session.status")),
      ("/session", json!("foreign")), ("/engine", json!("WEBJS")), ("/payload/fromMe", json!(true)),
      ("/payload/from", json!("123@g.us")), ("/payload/from", json!("status@broadcast"))] {
      let mut message = envelope("ignored"); *message.pointer_mut(path).unwrap() = value;
      let raw = serde_json::to_vec(&message).unwrap();
      assert_eq!(send(&app, &raw, &[]).await.status(), 401);
      let response = deliver(&app, &message).await; assert_eq!(response.status(), 200);
      assert!(crate::modules::doctor::tests::text(response).await.contains("ignored"));
    }
    assert_eq!(count(&db, "processing_jobs").await, 0);
    for (path, value) in [("/payload/id", json!(null)), ("/payload/timestamp", json!(1.5)),
      ("/payload/timestamp", json!("1791158400")), ("/payload/fromMe", json!(null)),
      ("/payload/from", json!("bad@lid")), ("/payload/body", json!(123))] {
      let mut message = envelope("invalid"); *message.pointer_mut(path).unwrap() = value;
      assert_eq!(deliver(&app, &message).await.status(), 400);
    }
    for (id, alt) in [("lid-mapped", Some("15550100102@s.whatsapp.net")), ("lid-only", None)] {
      let mut message = envelope(id); message["payload"]["from"] = json!("990000000001@lid");
      if let Some(alt) = alt { message["payload"]["_data"] = json!({"Info":{"SenderAlt":alt}}); }
      assert_eq!(deliver(&app, &message).await.status(), 200);
    }
    let rows = db.conn.query_all(sql("SELECT original_chat_identity, resolved_phone_identity FROM inbox_messages ORDER BY message_id")).await.unwrap();
    assert_eq!(rows[0].try_get::<String>("", "original_chat_identity").unwrap(), "990000000001@lid");
    assert_eq!(rows[0].try_get::<Option<String>>("", "resolved_phone_identity").unwrap(), Some("15550100102@c.us".into()));
    assert_eq!(rows[1].try_get::<Option<String>>("", "resolved_phone_identity").unwrap(), None);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn signed_field_boundaries_reject_without_effects_and_exact_limits_prepare() {
  with_database(|db| async move {
    let (app, clock) = setup(&db).await;
    for (path, value) in [
      ("/payload/id", json!("")),
      ("/payload/id", json!("x".repeat(513))),
      ("/payload/id", json!("é".repeat(257))),
      ("/payload/id", json!("id\u{0000}")),
      ("/payload/id", json!("id\n")),
      ("/payload/body", json!("x".repeat(1024 * 1024 + 1))),
      ("/payload/body", json!("é".repeat(512 * 1024 + 1))),
      ("/payload/body", json!("body\u{0000}")),
      ("/payload/timestamp", json!(-1)),
      ("/payload/timestamp", json!(253402300800_i64)),
      ("/payload/timestamp", json!(u64::MAX)),
      ("/payload/hasMedia", json!(null)),
    ] {
      let mut message = envelope("invalid-boundary");
      *message.pointer_mut(path).unwrap() = value;
      assert_eq!(deliver(&app, &message).await.status(), 400, "{path}");
      assert_eq!(count(&db, "inbox_messages").await, 0);
      assert_eq!(count(&db, "processing_jobs").await, 0);
    }
    for (field, limit) in [("url", 8192), ("mimetype", 256), ("filename", 1024)] {
      for value in [json!("x".repeat(limit + 1)), json!("nul\u{0000}"), json!(42)] {
        let mut message = envelope("invalid-media-boundary");
        message["payload"]["media"] = json!({});
        message["payload"]["media"][field] = value;
        assert_eq!(deliver(&app, &message).await.status(), 400, "{field}");
        assert_eq!(count(&db, "inbox_messages").await, 0);
        assert_eq!(count(&db, "processing_jobs").await, 0);
      }
    }
    let mut exact = vec![envelope(&"x".repeat(512)), envelope(&"é".repeat(256))];
    let mut body = envelope("body-at-limit");
    body["payload"]["body"] = json!("x".repeat(1024 * 1024));
    exact.push(body);
    for timestamp in [0, 253402300799_i64] {
      let mut message = envelope(&format!("timestamp-{timestamp}"));
      message["payload"]["timestamp"] = json!(timestamp);
      exact.push(message);
    }
    let mut media = envelope("media-at-limits");
    media["payload"]["hasMedia"] = json!(true);
    media["payload"]["media"] = json!({"url":"x".repeat(8192), "mimetype":"x".repeat(256), "filename":"x".repeat(1024)});
    exact.push(media);
    for (index, message) in exact.iter().enumerate() {
      assert_eq!(deliver(&app, message).await.status(), 200);
      assert_eq!(count(&db, "inbox_messages").await, index as i64 + 1);
      assert_eq!(count(&db, "processing_jobs WHERE kind='prepare_inbox'").await, index as i64 + 1);
      assert_eq!(super::super::jobs::run_once(&db, clock.as_ref()).await.unwrap(),
        super::super::jobs::Outcome::Prepared(message["payload"]["id"].as_str().unwrap().to_owned()));
    }
    assert_eq!(db.conn.query_one(sql("SELECT count(*) AS count FROM inbox_messages WHERE preparation_state='ready_for_conversation'"))
      .await.unwrap().unwrap().try_get::<i64>("", "count").unwrap(), 6);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn failed_job_insert_rolls_back_intake_and_unavailable_db_never_acknowledges() {
  with_database(|db| async move {
    let (app, _) = setup(&db).await;
    db.conn.execute_unprepared("CREATE FUNCTION reject_job() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic fault'; END $$; CREATE TRIGGER reject_job BEFORE INSERT ON processing_jobs FOR EACH ROW EXECUTE FUNCTION reject_job();").await.unwrap();
    assert_eq!(deliver(&app, &envelope("rollback")).await.status(), 503);
    assert_eq!(count(&db, "inbox_messages").await, 0);
    assert_eq!(count(&db, "processing_jobs").await, 0);
    db.conn.execute_unprepared("DROP TRIGGER reject_job ON processing_jobs").await.unwrap();
    let message = envelope("rollback");
    let (a,b) = tokio::join!(deliver(&app, &message), deliver(&app, &message));
    assert_eq!(a.status(), 200); assert_eq!(b.status(), 200);
    assert_eq!(count(&db, "inbox_messages").await, 1);
    assert_eq!(count(&db, "processing_jobs").await, 1);
    db.conn.execute_unprepared("DROP TABLE processing_jobs").await.unwrap();
    assert_eq!(deliver(&app, &envelope("no-job-table")).await.status(), 503);
    assert_eq!(count(&db, "inbox_messages").await, 1);
    db.conn.close_by_ref().await.unwrap();
    assert_eq!(deliver(&app,&envelope("closed-pool")).await.status(),503);
    assert_eq!(send(&app,b"not-json",&[]).await.status(),401);
    let mut ignored=envelope("ignored"); ignored["event"]=json!("call.received");
    assert_eq!(deliver(&app,&ignored).await.status(),200);
  }).await;
}

pub(super) const KEY: &str = "synthetic-only-HMAC-key-at-least-32-characters";
pub(super) fn sql(query: &str) -> Statement {
  Statement::from_string(DbBackend::Postgres, query)
}
pub(super) fn envelope(id: &str) -> Value {
  json!({"event":"message", "session":"default", "engine":"GOWS", "timestamp":1791158405000_i64,
    "payload":{"id":id,"from":"15550100101@c.us","fromMe":false,"timestamp":1791158400,
    "body":" Hola, cita sintética \n", "hasMedia":false}})
}
pub(super) fn signature(raw: &[u8], key: &str) -> String {
  let mut mac = Hmac::<Sha512>::new_from_slice(key.as_bytes()).unwrap();
  mac.update(raw);
  mac
    .finalize()
    .into_bytes()
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}
pub(super) async fn setup(
  db: &crate::database::Db,
) -> (
  axum::Router,
  std::sync::Arc<crate::modules::doctor::tests::TestClock>,
) {
  let (mut cfg, clock, app, _) = fixture(db).await;
  drop(app);
  std::sync::Arc::get_mut(&mut cfg).unwrap().waha = super::super::config::WahaConfig::parse(
    Some("http://127.0.0.1:1"),
    Some("fixture-only-api-key"),
    Some(KEY),
  )
  .unwrap();
  (
    crate::app::router_with_clock(cfg, db.clone(), clock.clone()),
    clock,
  )
}
pub(super) async fn send(
  app: &axum::Router,
  raw: &[u8],
  headers: &[(&str, &str)],
) -> axum::response::Response {
  let mut request = Request::builder()
    .method("POST")
    .uri("/api/v1/webhooks/waha");
  for (key, value) in headers {
    request = request.header(*key, *value);
  }
  app
    .clone()
    .oneshot(request.body(Body::from(raw.to_vec())).unwrap())
    .await
    .unwrap()
}
pub(super) async fn deliver(app: &axum::Router, message: &Value) -> axum::response::Response {
  let raw = serde_json::to_vec(message).unwrap();
  send(app, &raw, &[("x-webhook-hmac", &signature(&raw, KEY))]).await
}
pub(super) async fn count(db: &crate::database::Db, table: &str) -> i64 {
  db.conn
    .query_one(sql(&format!("SELECT count(*) AS count FROM {table}")))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "count")
    .unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn fractional_arrival_is_immutable_and_preparation_is_eligible_in_same_second() {
  struct Millis;
  impl crate::modules::doctor::Clock for Millis {
    fn now(&self) -> i64 {
      1_800_000_030
    }
    fn now_millis(&self) -> i64 {
      1_800_000_030_123
    }
  }
  with_database(|db| async move {
    let (mut cfg, _, app, _) = fixture(&db).await;
    drop(app);
    std::sync::Arc::get_mut(&mut cfg).unwrap().waha = super::super::config::WahaConfig::parse(
      Some("http://127.0.0.1:1"),
      Some("fixture-key"),
      Some(KEY),
    )
    .unwrap();
    let app = crate::app::router_with_clock(cfg, db.clone(), std::sync::Arc::new(Millis));
    let input = envelope("fractional");
    assert_eq!(deliver(&app, &input).await.status(), 200);
    assert_eq!(deliver(&app, &input).await.status(), 200);
    let row = db
      .conn
      .query_one(sql("SELECT arrived_at FROM inbox_messages"))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      row
        .try_get::<chrono::DateTime<chrono::Utc>>("", "arrived_at")
        .unwrap()
        .timestamp_millis(),
      1_800_000_030_123
    );
    assert_eq!(
      super::super::jobs::run_once(&db, &Millis).await.unwrap(),
      super::super::jobs::Outcome::Prepared("fractional".into())
    );
    assert_eq!(count(&db, "inbox_messages").await, 1);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn signed_message_commits_original_bytes_timestamps_and_one_job() {
  with_database(|db| async move {
    let (app, _) = setup(&db).await;
    let message = envelope("payload-ID");
    let raw = serde_json::to_vec(&message).unwrap();
    let response = deliver(&app, &message).await;
    assert_eq!(response.status(), 200);
    let row = db
      .conn
      .query_one(sql("SELECT * FROM inbox_messages"))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      row.try_get::<String>("", "message_id").unwrap(),
      "payload-ID"
    );
    assert_eq!(row.try_get::<Vec<u8>>("", "raw_envelope").unwrap(), raw);
    assert_eq!(
      row.try_get::<i64>("", "server_timestamp").unwrap(),
      1791158400
    );
    assert_eq!(
      row
        .try_get::<chrono::DateTime<chrono::Utc>>("", "arrived_at")
        .unwrap()
        .timestamp(),
      crate::modules::doctor::tests::NOW + 30
    );
    assert_eq!(
      row
        .try_get::<String>("", "resolved_phone_identity")
        .unwrap(),
      "15550100101@c.us"
    );
    assert_eq!(
      row.try_get::<String>("", "preparation_state").unwrap(),
      "accepted"
    );
    assert_eq!(count(&db, "processing_jobs").await, 1);
    let mut duplicate = message;
    duplicate["id"] = json!("different-envelope-ID");
    duplicate["payload"]["body"] = json!("do not overwrite");
    let (a, b) = tokio::join!(deliver(&app, &duplicate), deliver(&app, &duplicate));
    assert_eq!(a.status(), 200);
    assert_eq!(b.status(), 200);
    assert_eq!(count(&db, "inbox_messages").await, 1);
    assert_eq!(count(&db, "processing_jobs").await, 1);
    assert_eq!(
      db.conn
        .query_one(sql("SELECT raw_envelope FROM inbox_messages"))
        .await
        .unwrap()
        .unwrap()
        .try_get::<Vec<u8>>("", "raw_envelope")
        .unwrap(),
      raw
    );
  })
  .await;
}
