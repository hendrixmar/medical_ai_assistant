use super::{
  fixture::Fixture,
  probes::{start, ticks},
  *,
};
use crate::modules::{doctor::Clock, google_calendar::worker};
use serde_json::json;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn unconfigured_restart_surfaces_only_committed_original_cleanup_targets() {
  for committed in [false, true] {
    for clear_pointer in [false, true] {
      with_database(move |db| async move {
        let mut f = Fixture::new(&db).await;
        f.connect().await;
        assert_eq!(f.select().await.status(), 303);
        assert_eq!(start(&f, "").await.status(), 303);
        if committed {
          ticks(&f, &db, 1).await;
        }
        let target = db
          .conn
          .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT event_id FROM google_probes",
          ))
          .await
          .unwrap()
          .unwrap()
          .try_get::<String>("", "event_id")
          .unwrap();
        let active_dashboard =
          text(authenticated_get(&f.app, "/dashboard", &f.session).await).await;
        assert!(!active_dashboard.contains("Eventos de prueba de Google con limpieza pendiente:"));
        if clear_pointer {
          *f.provider.pages.lock().await = vec![
            json!({"items":[{"id":"other@example.invalid","summary":"Otro","accessRole":"owner"}]}),
          ];
          assert_eq!(
            f.post(
              "/settings/google/calendars",
              &[
                ("generation", "1"),
                ("selection_version", "2"),
                ("booking_calendar", "other@example.invalid")
              ]
            )
            .await
            .status(),
            303
          );
        }
        let before = f.provider.calls.lock().await.len();
        f.app = axum::Router::new();
        Arc::get_mut(&mut f.cfg).unwrap().google = None;
        let pool = crate::app::tests::reconnect(&db, 2).await;
        f.app = crate::app::router_with_clock(f.cfg.clone(), pool.clone(), f.clock.clone());
        assert!(!worker::run_once(&pool, f.clock.as_ref(), &f.cfg, None)
          .await
          .unwrap());
        let body = text(authenticated_get(&f.app, "/settings/google", &f.session).await).await;
        let dashboard = text(authenticated_get(&f.app, "/dashboard", &f.session).await).await;
        assert_eq!(
          body.contains(&format!("Evento: {target}")),
          committed,
          "original target visibility"
        );
        assert_eq!(
          dashboard.contains("Eventos de prueba de Google con limpieza pendiente: 1."),
          committed
        );
        if committed {
          assert!(body.contains("Calendario: secondary@example.invalid"));
          assert!(body.contains("pausada"));
          if !clear_pointer {
            assert!(body.contains("Prueba con limpieza pendiente."));
          }
        }
        for path in ["connect", "calendars", "probe", "probes/"] {
          assert!(!body.contains(&format!("action=\"/settings/google/{path}")));
        }
        assert_eq!(f.provider.calls.lock().await.len(), before);
        pool.conn.close().await.unwrap();
      })
      .await;
    }
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn selection_change_preserves_original_target_cleanup_without_publishing_old_success() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    ticks(&f, &db, 1).await;
    *f.provider.pages.lock().await =
      vec![json!({"items":[{"id":"other@example.invalid","summary":"Otro","accessRole":"owner"}]})];
    assert_eq!(
      f.post(
        "/settings/google/calendars",
        &[
          ("generation", "1"),
          ("selection_version", "2"),
          ("booking_calendar", "other@example.invalid")
        ]
      )
      .await
      .status(),
      303
    );
    ticks(&f, &db, 5).await;
    let row = f.row(&db).await;
    assert_eq!(
      row.try_get::<String>("", "booking_calendar").unwrap(),
      "other@example.invalid"
    );
    assert!(row
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
    assert!(row
      .try_get::<Option<i64>>("", "permission_checked_at")
      .unwrap()
      .is_none());
    let calls = f.provider.calls.lock().await;
    let deletes: Vec<_> = calls.iter().filter(|(m, _, _)| m == "DELETE").collect();
    assert_eq!(deletes.len(), 1);
    assert!(deletes[0].1.contains("secondary@example.invalid"));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn wildcard_or_malformed_etag_never_authorizes_unconditional_delete() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    ticks(&f, &db, 2).await;
    f.provider.event.lock().await.as_mut().unwrap()["etag"] = json!("*");
    ticks(&f, &db, 1).await;
    assert!(!f
      .provider
      .calls
      .lock()
      .await
      .iter()
      .any(|(m, _, _)| m == "DELETE"));
    assert!(f.provider.event.lock().await.is_some());
    assert!(f
      .row(&db)
      .await
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn killed_worker_after_actual_insert_recovers_only_by_read_and_retains_cleanup() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    f.provider
      .hold_insert
      .store(true, std::sync::atomic::Ordering::SeqCst);
    let (worker_db, clock, cfg, client) =
      (db.clone(), f.clock.clone(), f.cfg.clone(), f.client.clone());
    let task = tokio::spawn(async move {
      worker::run_once(&worker_db, clock.as_ref(), &cfg, Some(&client)).await
    });
    f.provider.insert_entered.acquire().await.unwrap().forget();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    f.provider.insert_release.add_permits(1);
    f.clock.advance(31);
    let pool = crate::app::tests::reconnect(&db, 2).await;
    ticks(&f, &pool, 6).await;
    assert!(f.provider.event.lock().await.is_none());
    assert_eq!(
      f.provider
        .calls
        .lock()
        .await
        .iter()
        .filter(|(m, p, _)| m == "POST" && p.contains("/events?"))
        .count(),
      1
    );
    assert!(
      f.row(&db)
        .await
        .try_get::<Option<i64>>("", "capability_checked_at")
        .unwrap()
        .is_none(),
      "lost creation response does not prove event creation permission"
    );
    pool.conn.close().await.unwrap();
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn late_insert_response_after_reconnect_cannot_publish_or_auto_cleanup() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    f.provider
      .hold_insert
      .store(true, std::sync::atomic::Ordering::SeqCst);
    let (worker_db, clock, cfg, client) =
      (db.clone(), f.clock.clone(), f.cfg.clone(), f.client.clone());
    let task = tokio::spawn(async move {
      worker::run_once(&worker_db, clock.as_ref(), &cfg, Some(&client)).await
    });
    f.provider.insert_entered.acquire().await.unwrap().forget();
    f.connect().await;
    f.provider.insert_release.add_permits(1);
    task.await.unwrap().unwrap();
    ticks(&f, &db, 4).await;
    assert!(f.provider.event.lock().await.is_some());
    assert!(!f
      .provider
      .calls
      .lock()
      .await
      .iter()
      .any(|(m, _, _)| m == "DELETE"));
    assert!(f
      .row(&db)
      .await
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
    assert!(f
      .row(&db)
      .await
      .try_get::<Option<uuid::Uuid>>("", "current_probe_id")
      .unwrap()
      .is_none());
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn ambiguous_404_releases_active_slot_and_later_success_coexists_with_retained_cleanup() {
  with_database(|db|async move {
    let f=Fixture::new(&db).await;f.connect().await;f.select().await;
    *f.provider.insert_status.lock().await=409;
    start(&f,"").await; ticks(&f,&db,32).await;
    let first=f.row(&db).await.try_get::<uuid::Uuid>("","current_probe_id").unwrap().to_string();
    let count=db.conn.query_one(Statement::from_string(DbBackend::Postgres,"SELECT count(*) AS n FROM google_probes WHERE phase='attention' AND cleanup_outcome='unresolved'")).await.unwrap().unwrap().try_get::<i64>("","n").unwrap();
    assert_eq!(count,1);
    *f.provider.insert_status.lock().await=200;
    assert_eq!(start(&f,&first).await.status(),303);ticks(&f,&db,6).await;
    assert!(f.row(&db).await.try_get::<Option<i64>>("","capability_checked_at").unwrap().is_some());
    let body=text(authenticated_get(&f.app,"/settings/google",&f.session).await).await;
    assert!(body.contains(&first));assert!(body.contains("limpieza"));
    let dashboard=text(authenticated_get(&f.app,"/dashboard",&f.session).await).await;
    assert!(dashboard.contains("/settings/google"));
    let calls=f.provider.calls.lock().await;
    assert_eq!(calls.iter().filter(|(m,p,_)|m=="POST" && p.contains("/events?")).count(),2);
    assert_eq!(calls.iter().filter(|(m,_,_)|m=="DELETE").count(),1);
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn collision_markers_are_never_deleted_and_cleanup_retry_is_bounded_and_cleanup_only() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    ticks(&f, &db, 1).await;
    let original = f.provider.event.lock().await.clone().unwrap();
    f.provider.event.lock().await.as_mut().unwrap()["extendedProperties"]["private"]["maProbe"] =
      json!("UNRELATED_EVENT");
    ticks(&f, &db, 3).await;
    assert!(!f
      .provider
      .calls
      .lock()
      .await
      .iter()
      .any(|(m, _, _)| m == "DELETE"));
    let id = f
      .row(&db)
      .await
      .try_get::<uuid::Uuid>("", "current_probe_id")
      .unwrap()
      .to_string();
    let path = format!("/settings/google/probes/{id}/cleanup");
    assert_eq!(f.post(&path, &[("generation", "1")]).await.status(), 303);
    ticks(&f, &db, 1).await;
    assert_eq!(f.post(&path, &[("generation", "1")]).await.status(), 429);
    f.clock.advance(61);
    *f.provider.event.lock().await = Some(original);
    assert_eq!(f.post(&path, &[("generation", "1")]).await.status(), 303);
    ticks(&f, &db, 4).await;
    assert!(f.provider.event.lock().await.is_none());
    assert!(f
      .row(&db)
      .await
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
    assert!(f
      .row(&db)
      .await
      .try_get::<Option<i64>>("", "permission_checked_at")
      .unwrap()
      .is_none());
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn reconnect_never_automatically_uses_new_grant_for_old_target_but_explicit_cleanup_can() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    ticks(&f, &db, 1).await;
    let id = f
      .row(&db)
      .await
      .try_get::<uuid::Uuid>("", "current_probe_id")
      .unwrap()
      .to_string();
    f.connect().await;
    let before = f.provider.calls.lock().await.len();
    ticks(&f, &db, 4).await;
    assert_eq!(f.provider.calls.lock().await.len(), before);
    let row = f.row(&db).await;
    assert!(row
      .try_get::<Option<String>>("", "booking_calendar")
      .unwrap()
      .is_none());
    assert!(row
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
    let response = f
      .post(
        &format!("/settings/google/probes/{id}/cleanup"),
        &[("generation", "2")],
      )
      .await;
    assert_eq!(response.status(), 303);
    ticks(&f, &db, 4).await;
    assert!(f.provider.event.lock().await.is_none());
    assert!(f
      .row(&db)
      .await
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn committed_create_recovery_and_two_workers_never_reinsert() {
  with_database(|db|async move {
    let f=Fixture::new(&db).await;f.connect().await;f.select().await;start(&f,"").await;
    // Persisted boundary after dispatch commitment, before any socket invocation.
    db.conn.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE google_probes SET phase='create_committed',create_committed_at=$1,observation_deadline=$2,reason='create_uncertain'",vec![f.clock.now_millis().into(),(f.clock.now_millis()+120_000).into()])).await.unwrap();
    let pool=crate::app::tests::reconnect(&db,2).await;
    let (a,b)=tokio::join!(worker::run_once(&db,f.clock.as_ref(),&f.cfg,Some(&f.client)),worker::run_once(&pool,f.clock.as_ref(),&f.cfg,Some(&f.client)));
    a.unwrap();b.unwrap();f.clock.advance(31);ticks(&f,&pool,30).await;
    assert!(!f.provider.calls.lock().await.iter().any(|(m,p,_)|m=="POST" && p.contains("/events?")));
    assert!(f.row(&db).await.try_get::<Option<i64>>("","capability_checked_at").unwrap().is_none());
    pool.conn.close().await.unwrap();
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn failed_create_commit_forbids_any_external_insert() {
  with_database(|db|async move {
    let f=Fixture::new(&db).await;f.connect().await;f.select().await;start(&f,"").await;
    // Test-schema-only deferred COMMIT failure, not an application failpoint.
    db.conn.execute_unprepared("CREATE FUNCTION reject_probe_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.phase='create_committed' THEN RAISE EXCEPTION 'synthetic commit failure'; END IF; RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER reject_probe_commit AFTER UPDATE ON google_probes DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_probe_commit();").await.unwrap();
    assert!(worker::run_once(&db,f.clock.as_ref(),&f.cfg,Some(&f.client)).await.is_err());
    assert!(!f.provider.calls.lock().await.iter().any(|(m,p,_)|m=="POST" && p.contains("/events?")));
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn conditional_delete_412_revalidates_markers_and_410_requires_known_own_evidence() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    *f.provider.delete_status.lock().await = 412;
    ticks(&f, &db, 3).await;
    assert!(f
      .row(&db)
      .await
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
    let before = f
      .provider
      .calls
      .lock()
      .await
      .iter()
      .filter(|(m, _, _)| m == "DELETE")
      .count();
    assert_eq!(before, 1);
    f.provider.event.lock().await.as_mut().unwrap()["extendedProperties"]["private"]
      ["maInstance"] = json!("OTHER_DEPLOYMENT");
    ticks(&f, &db, 2).await;
    assert_eq!(
      f.provider
        .calls
        .lock()
        .await
        .iter()
        .filter(|(m, _, _)| m == "DELETE")
        .count(),
      before
    );
    let old = f
      .row(&db)
      .await
      .try_get::<uuid::Uuid>("", "current_probe_id")
      .unwrap()
      .to_string();
    *f.provider.delete_status.lock().await = 410;
    assert_eq!(start(&f, &old).await.status(), 303);
    ticks(&f, &db, 5).await;
    let row = f.row(&db).await;
    assert!(row
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
    assert!(row
      .try_get::<Option<i64>>("", "permission_checked_at")
      .unwrap()
      .is_none());
    let probe = db
      .conn
      .query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT cleanup_outcome FROM google_probes ORDER BY started_at DESC LIMIT 1",
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      probe.try_get::<String>("", "cleanup_outcome").unwrap(),
      "deleted"
    );
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn pending_conference_exhausts_budget_and_dangerous_actual_video_uris_never_enable_capability(
) {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    let mut expected = String::new();
    for uri in [
      "http://meet.google.com/abc-def-ghi",
      "https://meet.google.com.evil/abc-def-ghi",
      "https://meet.google.com@evil/abc-def-ghi",
      "https://meet.google.com:444/abc-def-ghi",
      "https://meet.google.com/abc-def-ghi?x=1",
      "https://meet.google.com/abc-def-ghi#x",
      "https://meet.google.com/%61bc-def-ghi",
      "https://meet.google.com/abc-def-ghi\n",
      "https://meet.google.com/other/../abc-def-ghi",
    ] {
      *f.provider.video.lock().await = uri.into();
      assert_eq!(start(&f, &expected).await.status(), 303);
      ticks(&f, &db, 4).await;
      assert!(f
        .row(&db)
        .await
        .try_get::<Option<i64>>("", "capability_checked_at")
        .unwrap()
        .is_none());
      assert!(f.provider.event.lock().await.is_none());
      expected = f
        .row(&db)
        .await
        .try_get::<uuid::Uuid>("", "current_probe_id")
        .unwrap()
        .to_string();
    }
    *f.provider.conference.lock().await = "pending".into();
    assert_eq!(start(&f, &expected).await.status(), 303);
    ticks(&f, &db, 18).await;
    assert!(f.provider.event.lock().await.is_none());
    assert!(f
      .row(&db)
      .await
      .try_get::<Option<i64>>("", "capability_checked_at")
      .unwrap()
      .is_none());
    let row = db
      .conn
      .query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT max(observations) AS n FROM google_probes",
      ))
      .await
      .unwrap()
      .unwrap();
    assert!(row.try_get::<i32>("", "n").unwrap() <= 12);
  })
  .await;
}
