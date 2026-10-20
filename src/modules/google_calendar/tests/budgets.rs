use super::{
  fixture::Fixture,
  probes::{start, ticks},
  *,
};
use crate::modules::{doctor::Clock, google_calendar::worker};
use sea_orm::TransactionTrait;
use serde_json::json;
use std::{
  sync::atomic::Ordering,
  time::{Duration, Instant},
};

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn insert_401_refresh_uses_live_committed_phase_not_cached_prepared_phase() {
  for exhausted in [false, true] {
    with_database(move |db| async move {
      let f = Fixture::new(&db).await;
      f.connect().await;
      f.select().await;
      start(&f, "").await;
      *f.provider.insert_status.lock().await = 401;
      f.provider.hold_insert.store(true, Ordering::SeqCst);
      let (pool, clock, cfg, client) =
        (db.clone(), f.clock.clone(), f.cfg.clone(), f.client.clone());
      let task =
        tokio::spawn(
          async move { worker::run_once(&pool, clock.as_ref(), &cfg, Some(&client)).await },
        );
      tokio::time::timeout(Duration::from_secs(2), f.provider.insert_entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
      if exhausted {
        db.conn
          .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE google_probes SET observation_deadline=$1",
            vec![(f.clock.now_millis() + 7_000).into()],
          ))
          .await
          .unwrap();
      }
      f.provider.insert_release.add_permits(1);
      task.await.unwrap().unwrap();
      let calls = f.provider.calls.lock().await;
      assert_eq!(
        calls.iter().filter(|(_, p, _)| p == "/token").count(),
        if exhausted { 1 } else { 2 }
      );
      assert_eq!(
        calls
          .iter()
          .filter(|(m, p, _)| m == "POST" && p.contains("/events?"))
          .count(),
        1
      );
    })
    .await;
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn expired_observation_transitions_locally_before_refreshing_for_cleanup() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.provider.grant.lock().await["expires_in"] = json!(60);
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    ticks(&f, &db, 1).await;
    f.clock.advance(120);
    let before = f.provider.calls.lock().await.len();
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, Some(&f.client))
      .await
      .unwrap();
    assert_eq!(
      f.provider.calls.lock().await.len(),
      before,
      "expired observation must transition without credential HTTP"
    );
    let row = db
      .conn
      .query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT phase,cleanup_deadline FROM google_probes",
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(row.try_get::<String>("", "phase").unwrap(), "cleanup");
    assert_eq!(
      row.try_get::<i64>("", "cleanup_deadline").unwrap(),
      f.clock.now_millis() + 300_000
    );
    ticks(&f, &db, 1).await;
    assert!(f.provider.event.lock().await.is_none());
    assert_eq!(
      f.provider
        .calls
        .lock()
        .await
        .iter()
        .filter(|(_, p, _)| p == "/token")
        .count(),
      2
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn sent_rotation_is_saved_even_when_the_probe_phase_expires_during_http() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.provider.grant.lock().await["expires_in"] = json!(60);
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    ticks(&f, &db, 2).await;
    f.clock.advance(10);
    db.conn
      .execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE google_probes SET cleanup_deadline=$1",
        vec![(f.clock.now_millis() + 9_000).into()],
      ))
      .await
      .unwrap();
    f.provider.grant.lock().await["refresh_token"] = json!("LATE_VALID_ROTATION");
    f.provider.hold_exchange.store(true, Ordering::SeqCst);
    let before = f.provider.calls.lock().await.len();
    let (pool, clock, cfg, client) = (db.clone(), f.clock.clone(), f.cfg.clone(), f.client.clone());
    let task =
      tokio::spawn(
        async move { worker::run_once(&pool, clock.as_ref(), &cfg, Some(&client)).await },
      );
    tokio::time::timeout(
      Duration::from_secs(2),
      f.provider.exchange_entered.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    f.clock.advance(10);
    f.provider.exchange_release.add_permits(1);
    task.await.unwrap().unwrap();
    assert_eq!(
      f.provider.calls.lock().await.len(),
      before + 1,
      "only the already sent token request"
    );
    assert_eq!(
      f.row(&db)
        .await
        .try_get::<i64>("", "token_version")
        .unwrap(),
      2
    );
    f.provider.hold_exchange.store(false, Ordering::SeqCst);
    f.clock.advance(61);
    authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert!(f
      .provider
      .calls
      .lock()
      .await
      .iter()
      .any(|(_, p, b)| p == "/token"
        && b
          .as_str()
          .is_some_and(|s| s.contains("refresh_token=LATE_VALID_ROTATION"))));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn five_second_transport_deadline_includes_streamed_body_not_just_headers() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.provider.slow_list_body.store(true, Ordering::SeqCst);
    let start = Instant::now();
    let response = authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert!(start.elapsed() >= Duration::from_millis(4800));
    assert!(start.elapsed() < Duration::from_secs(8));
    let body = text(response).await;
    assert!(!body.contains("action=\"/settings/google/calendars\""));
    assert!(body.contains("action=\"/logout\""));
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn paginated_interactive_route_stops_at_twelve_seconds_below_global_timeout() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    *f.provider.pages.lock().await = (0..10)
      .map(|page| json!({"items":[],"nextPageToken":(page+1).to_string()}))
      .collect();
    f.provider.list_delay_ms.store(1500, Ordering::SeqCst);
    let start = Instant::now();
    let response = authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert_eq!(response.status(), 503);
    assert!(start.elapsed() >= Duration::from_secs(11));
    assert!(start.elapsed() < Duration::from_secs(14));
    let body = text(response).await;
    assert!(body.contains("action=\"/logout\""));
    assert!(!body.contains("action=\"/settings/google/calendars\""));
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn callback_outer_envelope_survives_actual_global_timeout_waiting_for_database_pool() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    let state = f.state().await;
    let pool =
      crate::app::tests::reconnect_with_acquire_timeout(&db, 2, Duration::from_secs(30)).await;
    let app = crate::app::router_with_clock(f.cfg.clone(), pool.clone(), f.clock.clone());
    let a = pool.conn.begin().await.unwrap();
    let b = pool.conn.begin().await.unwrap();
    let start = Instant::now();
    let response = authenticated_get(
      &app,
      &format!("/settings/google/callback/?state={state}&code=CODE_SENTINEL"),
      &f.session,
    )
    .await;
    assert!(start.elapsed() >= Duration::from_secs(14));
    assert!(start.elapsed() < Duration::from_secs(18));
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/settings/google");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(text(response).await.is_empty());
    a.rollback().await.unwrap();
    b.rollback().await.unwrap();
    pool.conn.close().await.unwrap();
    assert!(f.provider.calls.lock().await.is_empty());
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn retry_after_cannot_escape_observation_or_cleanup_budget() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    ticks(&f, &db, 1).await;
    *f.provider.event_status.lock().await = 429;
    *f.provider.event_retry_after.lock().await = Some("20".into());
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, Some(&f.client))
      .await
      .unwrap();
    let before = f.provider.calls.lock().await.len();
    f.clock.advance(19);
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, Some(&f.client))
      .await
      .unwrap();
    assert_eq!(f.provider.calls.lock().await.len(), before);
    f.clock.advance(2);
    *f.provider.event_retry_after.lock().await = Some("999999".into());
    worker::run_once(&db, f.clock.as_ref(), &f.cfg, Some(&f.client))
      .await
      .unwrap();
    let row = db
      .conn
      .query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT phase,cleanup_outcome FROM google_probes",
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(row.try_get::<String>("", "phase").unwrap(), "attention");
    assert_eq!(
      row.try_get::<String>("", "cleanup_outcome").unwrap(),
      "unresolved"
    );
    let before = f.provider.calls.lock().await.len();
    f.clock.advance(3600);
    ticks(&f, &db, 4).await;
    assert_eq!(f.provider.calls.lock().await.len(), before);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn cleanup_call_budget_and_ten_probe_starts_persist_across_pools() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    *f.provider.delete_status.lock().await = 412;
    start(&f, "").await;
    ticks(&f, &db, 20).await;
    let row = db
      .conn
      .query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT phase,cleanup_calls FROM google_probes",
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(row.try_get::<String>("", "phase").unwrap(), "attention");
    assert_eq!(row.try_get::<i32>("", "cleanup_calls").unwrap(), 12);
    *f.provider.delete_status.lock().await = 200;
    for _ in 1..10 {
      let id = f
        .row(&db)
        .await
        .try_get::<uuid::Uuid>("", "current_probe_id")
        .unwrap()
        .to_string();
      assert_eq!(start(&f, &id).await.status(), 303);
      ticks(&f, &db, 4).await;
    }
    let pool = crate::app::tests::reconnect(&db, 2).await;
    let app = crate::app::router_with_clock(f.cfg.clone(), pool.clone(), f.clock.clone());
    let id = f
      .row(&db)
      .await
      .try_get::<uuid::Uuid>("", "current_probe_id")
      .unwrap()
      .to_string();
    assert_eq!(
      post(
        &app,
        "/settings/google/probe",
        Some(&f.session),
        Some("https://practice.test"),
        &[
          ("csrf_token", &f.csrf),
          ("generation", "1"),
          ("selection_version", "2"),
          ("expected_probe_id", &id)
        ]
      )
      .await
      .status(),
      429
    );
    assert!(f.clock.now() < (NOW + 3600));
    pool.conn.close().await.unwrap();
  })
  .await;
}
