use super::{fixture::Fixture, *};

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn session_expiry_after_lock_wait_prevents_oauth_consumption_and_issuance() {
  use crate::modules::doctor::Clock;
  use sea_orm::TransactionTrait;
  for consuming in [false, true] {
    with_database(move |db| async move {
      let f = Fixture::new(&db).await;
      let state = if consuming { f.state().await } else { String::new() };
      let before_sequence = f.row(&db).await.try_get::<i64>("", "auth_sequence").unwrap();
      db.conn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE doctor_sessions SET expires_at=$1", vec![(f.clock.now()+1).into()])).await.unwrap();
      let inspector = crate::app::tests::reconnect(&db, 1).await;
      let held = db.conn.begin().await.unwrap();
      held.query_one(Statement::from_string(DbBackend::Postgres,
        "SELECT id FROM google_connection WHERE id=1 FOR UPDATE")).await.unwrap();
      let (app, session, csrf) = (f.app.clone(), f.session.clone(), f.csrf.clone());
      let request = tokio::spawn(async move {
        if consuming {
          authenticated_get(&app, &format!("/settings/google/callback?state={state}&code=SYNTHETIC_CODE"), &session).await
        } else {
          post(&app, "/settings/google/connect", Some(&session), Some("https://practice.test"), &[("csrf_token", csrf.as_str())]).await
        }
      });
      tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
          let row = inspector.conn.query_one(Statement::from_string(DbBackend::Postgres,
            "SELECT EXISTS(SELECT FROM pg_locks session_lock JOIN pg_locks waiting ON waiting.pid=session_lock.pid WHERE session_lock.relation='doctor_sessions'::regclass AND session_lock.mode='RowShareLock' AND session_lock.granted AND NOT waiting.granted) AS blocked")).await.unwrap().unwrap();
          if row.try_get::<bool>("", "blocked").unwrap() { break; }
          tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
      }).await.expect("OAuth operation holds session lock while waiting for connection");
      f.clock.advance(2);
      held.rollback().await.unwrap();
      let response = request.await.unwrap();
      assert!(f.provider.calls.lock().await.is_empty(), "expired session must not exchange a code");
      assert_eq!(f.row(&db).await.try_get::<i64>("", "auth_sequence").unwrap(), before_sequence);
      assert_eq!(f.row(&db).await.try_get::<i64>("", "generation").unwrap(), 0);
      if consuming {
        assert_eq!(response.headers()["location"], "/settings/google");
        let attempt = db.conn.query_one(Statement::from_string(DbBackend::Postgres,
          "SELECT outcome,verifier_ciphertext IS NOT NULL AS retained FROM google_auth_attempts")).await.unwrap().unwrap();
        assert_eq!(attempt.try_get::<String>("", "outcome").unwrap(), "pending");
        assert!(attempt.try_get::<bool>("", "retained").unwrap());
      } else {
        assert!(!response.status().is_redirection(), "expired session must not initiate authorization");
      }
      inspector.conn.close().await.unwrap();
    }).await;
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn session_expiry_while_waiting_for_connection_lock_fences_grant_commit() {
  use crate::modules::doctor::Clock;
  use sea_orm::TransactionTrait;
  with_database(|db|async move {
    let f=Fixture::new(&db).await;let state=f.state().await;
    db.conn.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE doctor_sessions SET expires_at=$1",vec![(f.clock.now()+1).into()])).await.unwrap();
    f.provider.hold_exchange.store(true,std::sync::atomic::Ordering::SeqCst);
    let (app,session)=(f.app.clone(),f.session.clone());
    let callback=tokio::spawn(async move{authenticated_get(&app,&format!("/settings/google/callback?state={state}&code=SYNTHETIC_CODE"),&session).await});
    f.provider.exchange_entered.acquire().await.unwrap().forget();
    let inspector=crate::app::tests::reconnect(&db,1).await;
    let held=db.conn.begin().await.unwrap();held.query_one(Statement::from_string(DbBackend::Postgres,"SELECT id FROM google_connection WHERE id=1 FOR UPDATE")).await.unwrap();
    f.provider.exchange_release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(1),async {
      loop {
        let row=inspector.conn.query_one(Statement::from_string(DbBackend::Postgres,"SELECT EXISTS(SELECT FROM pg_locks session_lock JOIN pg_locks waiting ON waiting.pid=session_lock.pid WHERE session_lock.relation='doctor_sessions'::regclass AND session_lock.mode='RowShareLock' AND session_lock.granted AND NOT waiting.granted) AS blocked")).await.unwrap().unwrap();
        if row.try_get::<bool>("","blocked").unwrap(){break;}
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
      }
    }).await.expect("callback holds the live session and waits for the connection fence");
    f.clock.advance(2);held.rollback().await.unwrap();
    assert_eq!(callback.await.unwrap().headers()["location"],"/settings/google");
    assert_eq!(f.row(&db).await.try_get::<i64>("","generation").unwrap(),0);
    inspector.conn.close().await.unwrap();
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn every_google_write_requires_native_origin_and_csrf_before_any_provider_request() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    for path in [
      "/settings/google/connect",
      "/settings/google/calendars",
      "/settings/google/probe",
      "/settings/google/probes/00000000-0000-0000-0000-000000000001/cleanup",
    ] {
      for origin in [None, Some("https://attacker.invalid"), Some("null")] {
        assert_eq!(
          post(
            &f.app,
            path,
            Some(&f.session),
            origin,
            &[("csrf_token", &f.csrf)]
          )
          .await
          .status(),
          403
        );
      }
      assert_eq!(
        post(
          &f.app,
          path,
          Some(&f.session),
          Some("https://practice.test"),
          &[("csrf_token", "forged")]
        )
        .await
        .status(),
        403
      );
    }
    assert!(f.provider.calls.lock().await.is_empty());
    assert_eq!(
      f.row(&db)
        .await
        .try_get::<i64>("", "auth_sequence")
        .unwrap(),
      0
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn missing_partial_scopes_or_malformed_new_credential_pairs_never_replace_old_grant() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    let valid = f.provider.grant.lock().await.clone();
    for (key, value) in [
      ("scope", serde_json::Value::Null),
      (
        "scope",
        serde_json::json!("https://www.googleapis.com/auth/calendar.events.owned"),
      ),
      ("token_type", serde_json::json!("MAC")),
      ("expires_in", serde_json::json!(0)),
      ("access_token", serde_json::json!("")),
      ("refresh_token", serde_json::json!("")),
    ] {
      f.clock.advance(3601);
      let mut grant = valid.clone();
      grant[key] = value;
      *f.provider.grant.lock().await = grant;
      f.connect().await;
      let row = f.row(&db).await;
      assert_eq!(row.try_get::<i64>("", "generation").unwrap(), 1);
      assert_eq!(row.try_get::<String>("", "status").unwrap(), "connected");
    }
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn expired_state_and_anonymous_callback_keep_clean_navigation_and_make_no_exchange() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    let state = f.state().await;
    f.clock.advance(601);
    let path = format!("/settings/google/callback?state={state}&code=SYNTHETIC_CODE");
    authenticated_get(&f.app, &path, &f.session).await;
    let response = get(&f.app, &path).await;
    assert_eq!(response.headers()["location"], "/settings/google");
    let response = get(&f.app, "/settings/google").await;
    assert_eq!(
      response.headers()["location"],
      "/login?return_to=/settings/google"
    );
    assert!(f.provider.calls.lock().await.is_empty());
    assert_eq!(
      f.row(&db).await.try_get::<i64>("", "generation").unwrap(),
      0
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn duplicate_decoded_state_denial_replay_and_bad_grant_preserve_old_connection() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    let state = f.state().await;
    for query in [
      format!("state={state}&%73tate={state}&code=x"),
      format!("state={state}&code=x&error=denied"),
      format!("state={state}&code=%zz"),
      format!("state={state}&code=%ff"),
      format!("state={state}&code=x&unknown=1&%75nknown=2"),
    ] {
      let response = authenticated_get(
        &f.app,
        &format!("/settings/google/callback?{query}"),
        &f.session,
      )
      .await;
      assert_eq!(response.headers()["referrer-policy"], "no-referrer");
      assert!(text(response).await.is_empty());
    }
    assert_eq!(
      f.provider
        .calls
        .lock()
        .await
        .iter()
        .filter(|(_, p, _)| p == "/token")
        .count(),
      1
    );
    authenticated_get(
      &f.app,
      &format!("/settings/google/callback?state={state}&error=access_denied"),
      &f.session,
    )
    .await;
    authenticated_get(
      &f.app,
      &format!("/settings/google/callback?state={state}&code=x"),
      &f.session,
    )
    .await;
    assert_eq!(
      f.row(&db).await.try_get::<i64>("", "generation").unwrap(),
      1
    );
    f.provider
      .grant
      .lock()
      .await
      .as_object_mut()
      .unwrap()
      .remove("refresh_token");
    f.connect().await;
    assert_eq!(
      f.row(&db).await.try_get::<i64>("", "generation").unwrap(),
      1
    );
    f.provider.grant.lock().await["refresh_token"] = serde_json::json!("new_refresh");
    f.provider.grant.lock().await["scope"] = serde_json::json!(format!("{SCOPES} openid"));
    f.connect().await;
    assert_eq!(
      f.row(&db).await.try_get::<i64>("", "generation").unwrap(),
      1
    );
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn logout_during_exchange_and_concurrent_callbacks_cannot_install_credentials() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    let state = f.state().await;
    f.provider
      .hold_exchange
      .store(true, std::sync::atomic::Ordering::SeqCst);
    let (app, session) = (f.app.clone(), f.session.clone());
    let path = format!("/settings/google/callback?state={state}&code=x");
    let again = path.clone();
    let pending = tokio::spawn(async move { authenticated_get(&app, &path, &session).await });
    f.provider
      .exchange_entered
      .acquire()
      .await
      .unwrap()
      .forget();
    authenticated_get(&f.app, &again, &f.session).await;
    assert_eq!(f.post("/logout", &[]).await.status(), 303);
    f.provider.exchange_release.add_permits(1);
    assert_eq!(
      pending.await.unwrap().headers()["location"],
      "/settings/google"
    );
    assert_eq!(
      f.row(&db).await.try_get::<i64>("", "generation").unwrap(),
      0
    );
    assert_eq!(f.provider.calls.lock().await.len(), 1);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn authorization_limit_and_latest_attempt_survive_reconnected_pool() {
  with_database(|db| async move {
    let mut f = Fixture::new(&db).await;
    let old = f.state().await;
    for _ in 0..4 {
      f.state().await;
    }
    let pool = crate::app::tests::reconnect(&db, 2).await;
    f.app = crate::app::router_with_clock(f.cfg.clone(), pool.clone(), f.clock.clone());
    assert_eq!(f.post("/settings/google/connect", &[]).await.status(), 429);
    authenticated_get(
      &f.app,
      &format!("/settings/google/callback?state={old}&code=x"),
      &f.session,
    )
    .await;
    assert!(f.provider.calls.lock().await.is_empty());
    f.clock.advance(3601);
    f.connect().await;
    assert_eq!(
      f.row(&db).await.try_get::<i64>("", "generation").unwrap(),
      1
    );
    pool.conn.close().await.unwrap();
  })
  .await;
}
