use super::{fixture::Fixture, *};
use crate::modules::google_calendar::worker;

pub(super) async fn start(f: &Fixture, expected: &str) -> axum::response::Response {
  f.post(
    "/settings/google/probe",
    &[
      ("generation", "1"),
      ("selection_version", "2"),
      ("expected_probe_id", expected),
    ],
  )
  .await
}
pub(super) async fn ticks(f: &Fixture, db: &crate::database::Db, n: usize) {
  for _ in 0..n {
    worker::run_once(db, f.clock.as_ref(), &f.cfg, Some(&f.client))
      .await
      .unwrap();
    f.clock.advance(11);
  }
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn persistent_delete_401_consumes_one_correction_across_claims_and_requires_reconnect() {
  for refresh_status in [200, 503] {
    with_database(move |db| async move {
      let f = Fixture::new(&db).await;
      f.connect().await;
      f.select().await;
      start(&f, "").await;
      ticks(&f, &db, 2).await;
      *f.provider.delete_status.lock().await = 401;
      *f.provider.token_status.lock().await = refresh_status;
      let pool = crate::app::tests::reconnect(&db, 2).await;
      ticks(&f, &pool, 8).await;
      let row = f.row(&pool).await;
      assert_eq!(
        row.try_get::<String>("", "status").unwrap(),
        "reauthorize",
        "persistent DELETE 401 must require reconnect"
      );
      assert!(row
        .try_get::<Option<i64>>("", "capability_checked_at")
        .unwrap()
        .is_none());
      let id = row
        .try_get::<uuid::Uuid>("", "current_probe_id")
        .unwrap()
        .to_string();
      let calls = f.provider.calls.lock().await;
      assert_eq!(
        calls.iter().filter(|(_, p, _)| p == "/token").count(),
        2,
        "one authorization plus one corrective refresh, including transient failure"
      );
      assert_eq!(calls.iter().filter(|(m, _, _)| m == "DELETE").count(), 2);
      let event_methods: Vec<_> = calls
        .iter()
        .filter(|(_, p, _)| p.contains("/events"))
        .map(|(m, _, _)| m.as_str())
        .collect();
      assert_eq!(
        event_methods,
        ["POST", "GET", "GET", "DELETE", "GET", "DELETE"]
      );
      drop(calls);
      let body = text(authenticated_get(&f.app, "/settings/google", &f.session).await).await;
      assert!(body.contains(&id));
      assert!(body.contains("Calendario: secondary@example.invalid"));
      assert!(f.provider.event.lock().await.is_some());
      // A new explicit cleanup-only request has its own single correction.
      *f.provider.token_status.lock().await = 200;
      f.connect().await;
      assert_eq!(
        f.post(
          &format!("/settings/google/probes/{id}/cleanup"),
          &[("generation", "2")]
        )
        .await
        .status(),
        303
      );
      ticks(&f, &pool, 8).await;
      let row = f.row(&pool).await;
      assert_eq!(row.try_get::<String>("", "status").unwrap(), "reauthorize");
      assert!(row
        .try_get::<Option<i64>>("", "permission_checked_at")
        .unwrap()
        .is_none());
      assert_eq!(
        f.provider
          .calls
          .lock()
          .await
          .iter()
          .filter(|(_, p, _)| p == "/token")
          .count(),
        4
      );
      pool.conn.close().await.unwrap();
    })
    .await;
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn delete_401_coalesces_with_a_refresh_already_completed_by_another_request() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.provider.grant.lock().await["expires_in"] = serde_json::json!(60);
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    ticks(&f, &db, 2).await;
    *f.provider.delete_status.lock().await = 401;
    f.provider
      .hold_delete
      .store(true, std::sync::atomic::Ordering::SeqCst);
    let (pool, clock, cfg, client) = (db.clone(), f.clock.clone(), f.cfg.clone(), f.client.clone());
    let task =
      tokio::spawn(
        async move { worker::run_once(&pool, clock.as_ref(), &cfg, Some(&client)).await },
      );
    tokio::time::timeout(
      std::time::Duration::from_secs(2),
      f.provider.delete_entered.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    f.clock.advance(10);
    assert_eq!(
      authenticated_get(&f.app, "/settings/google", &f.session)
        .await
        .status(),
      200
    );
    assert_eq!(
      f.row(&db)
        .await
        .try_get::<i64>("", "token_version")
        .unwrap(),
      2
    );
    f.provider.delete_release.add_permits(1);
    task.await.unwrap().unwrap();
    assert_eq!(
      f.provider
        .calls
        .lock()
        .await
        .iter()
        .filter(|(_, p, _)| p == "/token")
        .count(),
      2,
      "old version 401 must reuse the completed refresh"
    );
    f.provider
      .hold_delete
      .store(false, std::sync::atomic::Ordering::SeqCst);
    f.clock.advance(2);
    ticks(&f, &db, 2).await;
    assert_eq!(
      f.row(&db).await.try_get::<String>("", "status").unwrap(),
      "reauthorize"
    );
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
async fn late_persistent_delete_401_cannot_invalidate_a_newer_grant() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    *f.provider.delete_status.lock().await = 401;
    ticks(&f, &db, 3).await;
    f.provider
      .hold_delete
      .store(true, std::sync::atomic::Ordering::SeqCst);
    let (pool, clock, cfg, client) = (db.clone(), f.clock.clone(), f.cfg.clone(), f.client.clone());
    let task =
      tokio::spawn(
        async move { worker::run_once(&pool, clock.as_ref(), &cfg, Some(&client)).await },
      );
    tokio::time::timeout(
      std::time::Duration::from_secs(2),
      f.provider.delete_entered.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    f.connect().await;
    let before = f.provider.calls.lock().await.len();
    f.provider.delete_release.add_permits(1);
    task.await.unwrap().unwrap();
    ticks(&f, &db, 2).await;
    let row = f.row(&db).await;
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "connected");
    assert_eq!(row.try_get::<i64>("", "generation").unwrap(), 2);
    assert_eq!(f.provider.calls.lock().await.len(), before);
    assert!(f.provider.event.lock().await.is_some());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn external_deletion_resolves_cleanup_without_demonstrating_event_operations() {
  for observed in [false, true] {
    for tombstone in [false, true] {
      with_database(move |db| async move {
        let f = Fixture::new(&db).await;
        f.connect().await;
        f.select().await;
        assert_eq!(start(&f, "").await.status(), 303);
        ticks(&f, &db, if observed { 2 } else { 1 }).await;
        if tombstone {
          let id = f.provider.event.lock().await.as_ref().unwrap()["id"].clone();
          *f.provider.event.lock().await = Some(serde_json::json!({"id":id,"status":"cancelled"}));
        } else {
          *f.provider.event_status.lock().await = 410;
        }
        let pool = crate::app::tests::reconnect(&db, 2).await;
        ticks(&f, &pool, 3).await;
        let probe = pool
          .conn
          .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT phase,cleanup_outcome,event_permission FROM google_probes",
          ))
          .await
          .unwrap()
          .unwrap();
        assert_eq!(probe.try_get::<String>("", "phase").unwrap(), "complete");
        assert_eq!(
          probe.try_get::<String>("", "cleanup_outcome").unwrap(),
          "deleted"
        );
        assert_eq!(
          f.provider
            .calls
            .lock()
            .await
            .iter()
            .filter(|(m, _, _)| m == "DELETE")
            .count(),
          0
        );
        let row = f.row(&pool).await;
        assert!(
          row
            .try_get::<Option<i64>>("", "permission_checked_at")
            .unwrap()
            .is_none(),
          "observing deletion is not demonstrated DELETE permission"
        );
        assert!(row
          .try_get::<Option<i64>>("", "capability_checked_at")
          .unwrap()
          .is_none());
        assert!(!probe.try_get::<bool>("", "event_permission").unwrap());
        pool.conn.close().await.unwrap();
      })
      .await;
    }
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn metadata_401_coordinates_one_refresh_without_ever_blindly_inserting() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    *f.provider.metadata_status.lock().await = 401;
    ticks(&f, &db, 2).await;
    assert_eq!(
      f.provider
        .metadata_calls
        .load(std::sync::atomic::Ordering::SeqCst),
      2
    );
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
    assert_eq!(
      f.row(&db).await.try_get::<String>("", "status").unwrap(),
      "reauthorize"
    );
    assert!(!f
      .provider
      .calls
      .lock()
      .await
      .iter()
      .any(|(m, p, _)| m == "POST" && p.contains("/events?")));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn durable_probe_creates_once_reads_actual_meet_and_conditionally_cleans_before_capability() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    assert_eq!(f.select().await.status(), 303);
    assert_eq!(start(&f, "").await.status(), 303);
    assert_eq!(
      start(&f, "").await.status(),
      409,
      "accepted form cannot start another event"
    );
    assert!(
      f.provider.event.lock().await.is_none(),
      "constructor and POST never create remotely"
    );
    assert!(f
      .row(&db)
      .await
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
    ticks(&f, &db, 6).await;
    let row = f.row(&db).await;
    assert!(row
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_some());
    assert!(row
      .try_get::<Option<i64>>("", "permission_checked_at")
      .unwrap()
      .is_some());
    assert!(f.provider.event.lock().await.is_none());
    let calls = f.provider.calls.lock().await;
    let inserts: Vec<_> = calls
      .iter()
      .filter(|(m, p, _)| m == "POST" && p.contains("/events?"))
      .collect();
    assert_eq!(inserts.len(), 1);
    let event = &inserts[0].2;
    assert!(inserts[0].1.contains("conferenceDataVersion=1"));
    assert!(inserts[0].1.contains("sendUpdates=none"));
    assert_eq!(event["visibility"], "private");
    assert_eq!(event["transparency"], "transparent");
    assert_eq!(event["reminders"]["useDefault"], false);
    assert!(event.get("attendees").is_none());
    assert!(event.get("recurrence").is_none());
    assert_eq!(event["id"].as_str().unwrap().len(), 32);
    assert_eq!(calls.iter().filter(|(m, _, _)| m == "DELETE").count(), 1);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn failed_conference_can_prove_event_permissions_but_not_video() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    *f.provider.conference.lock().await = "failure".into();
    assert_eq!(start(&f, "").await.status(), 303);
    ticks(&f, &db, 6).await;
    let row = f.row(&db).await;
    assert!(row
      .try_get::<Option<i64>>("", "permission_checked_at")
      .unwrap()
      .is_some());
    assert!(row
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
    assert!(f.provider.event.lock().await.is_none());
  })
  .await;
}
