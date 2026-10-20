use super::*;
use crate::modules::conversation::sender;
use axum::response::{IntoResponse, Response};

pub(super) async fn fake(State(w): State<Arc<Wire>>, request: axum::extract::Request) -> Response {
  assert_eq!(request.headers()["x-api-key"], "synthetic-api-key");
  let path = request.uri().path().to_owned();
  let raw = axum::body::to_bytes(request.into_body(), 1024 * 1024)
    .await
    .unwrap();
  let body = if raw.is_empty() {
    Value::Null
  } else {
    serde_json::from_slice(&raw).unwrap()
  };
  w.outgoing
    .lock()
    .unwrap()
    .push((path.clone(), body.clone()));
  match path.as_str() {
    "/api/default/new-message-id" => Json(json!({"id":"DOCTORREPLY"})).into_response(),
    "/api/sendSeen" => axum::http::StatusCode::NO_CONTENT.into_response(),
    "/api/startTyping" | "/api/stopTyping" => Json(json!({"result":true})).into_response(),
    "/api/sendText" => {
      Json(json!({"id":format!("true_{}_DOCTORREPLY",body["chatId"].as_str().unwrap())}))
        .into_response()
    }
    _ => axum::http::StatusCode::NOT_FOUND.into_response(),
  }
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn verification_reply_stays_off_until_opt_in_then_uses_existing_pacing_once() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(1);
    let mut input = message("WIRE", "99001@lid", &code, clock.now_millis());
    input["payload"]["_data"]["Info"] = json!({"SenderAlt":"525512345678@s.whatsapp.net"});
    process(&db, &app, clock.as_ref(), &cfg, &input).await;
    let client = crate::modules::whatsapp::client::Client::new(cfg.waha.as_ref().unwrap()).unwrap();
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Idle
    );
    assert!(p.wire.outgoing.lock().unwrap().is_empty());
    drop(app);
    let mut cfg = cfg;
    Arc::get_mut(&mut cfg).unwrap().waha_outbound_enabled = true;
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Typing
    );
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Idle
    );
    let row = db
      .conn
      .query_one(sql(
        "SELECT typing_delay_ms,typing_due_at FROM outbox_messages",
      ))
      .await
      .unwrap()
      .unwrap();
    let delay: i32 = row.try_get("", "typing_delay_ms").unwrap();
    assert!((2000..=6000).contains(&delay));
    assert_eq!(
      row.try_get::<i64>("", "typing_due_at").unwrap(),
      clock.now_millis() + i64::from(delay)
    );
    clock.advance(i64::from(delay));
    assert_eq!(
      sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
        .await
        .unwrap(),
      sender::Outcome::Dispatched
    );
    for _ in 0..3 {
      assert_eq!(
        sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
          .await
          .unwrap(),
        sender::Outcome::Idle
      );
    }
    let calls = p.wire.outgoing.lock().unwrap().clone();
    assert_eq!(
      calls
        .iter()
        .map(|(path, _)| path.as_str())
        .collect::<Vec<_>>(),
      vec![
        "/api/default/new-message-id",
        "/api/sendSeen",
        "/api/startTyping",
        "/api/stopTyping",
        "/api/sendText"
      ]
    );
    let text = calls[4].1["text"].as_str().unwrap();
    assert!(!text.contains(&code));
    assert!(text.contains("https://practice.test/dashboard"));
    assert!(!text.contains("1 Agendar"));
    assert_eq!(calls[4].1["chatId"], "99001@lid");
    assert_eq!(
      count(&db, "contacts WHERE privacy_first_sent_at IS NOT NULL").await,
      1
    );
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE state='sent' AND dispatch_committed_at IS NOT NULL"
      )
      .await,
      1
    );
  })
  .await;
}
