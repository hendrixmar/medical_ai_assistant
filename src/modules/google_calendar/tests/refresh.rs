use super::{fixture::Fixture, *};
use serde_json::json;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn supplied_invalid_refresh_replacement_never_reuses_previous_credentials() {
  for replacement in [json!(null), json!(""), json!(false), json!({})] {
    with_database(move |db| async move {
      let f = Fixture::new(&db).await;
      f.connect().await;
      let before = f.row(&db).await;
      let calls_before = f.provider.calls.lock().await.len();
      f.clock.advance(3601);
      f.provider.grant.lock().await["refresh_token"] = replacement;
      authenticated_get(&f.app, "/settings/google", &f.session).await;
      let after = f.row(&db).await;
      assert_eq!(
        after.try_get::<String>("", "status").unwrap(),
        "reauthorize"
      );
      assert_eq!(
        after.try_get::<String>("", "notice").unwrap(),
        "refresh_uncertain"
      );
      for column in ["generation", "token_version", "credential_expires_at"] {
        assert_eq!(
          after.try_get::<i64>("", column).unwrap(),
          before.try_get::<i64>("", column).unwrap()
        );
      }
      for column in ["credential_ciphertext", "credential_nonce"] {
        assert_eq!(
          after.try_get::<Vec<u8>>("", column).unwrap(),
          before.try_get::<Vec<u8>>("", column).unwrap()
        );
      }
      assert_eq!(f.provider.calls.lock().await.len(), calls_before + 1);
      assert_eq!(f.provider.calls.lock().await.last().unwrap().1, "/token");
      authenticated_get(&f.app, "/settings/google", &f.session).await;
      assert_eq!(f.provider.calls.lock().await.len(), calls_before + 1);
    })
    .await;
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn definite_transient_refresh_does_not_claim_revoked_or_lose_generation() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.clock.advance(3601);
    *f.provider.token_status.lock().await = 503;
    *f.provider.grant.lock().await =
      json!({"error":"temporarily_unavailable","error_description":"RAW_ERROR_SENTINEL"});
    let response = authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert!(!text(response).await.contains("RAW_ERROR_SENTINEL"));
    let row = f.row(&db).await;
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "connected");
    assert_eq!(row.try_get::<i64>("", "generation").unwrap(), 1);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn refresh_rotation_omission_single_flight_and_reconnect_fence_use_real_http() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.clock.advance(3601);
    f.provider
      .grant
      .lock()
      .await
      .as_object_mut()
      .unwrap()
      .remove("refresh_token");
    authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert_eq!(
      f.row(&db)
        .await
        .try_get::<i64>("", "token_version")
        .unwrap(),
      2
    );
    f.clock.advance(3601);
    f.provider.grant.lock().await["refresh_token"] = json!("ROTATED_REFRESH_SENTINEL");
    authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert_eq!(
      f.row(&db)
        .await
        .try_get::<i64>("", "token_version")
        .unwrap(),
      3
    );
    f.clock.advance(3601);
    f.provider
      .hold_exchange
      .store(true, std::sync::atomic::Ordering::SeqCst);
    let (app, session) = (f.app.clone(), f.session.clone());
    let pending =
      tokio::spawn(async move { authenticated_get(&app, "/settings/google", &session).await });
    f.provider
      .exchange_entered
      .acquire()
      .await
      .unwrap()
      .forget();
    let second = authenticated_get(&f.app, "/settings/google", &f.session).await;
    assert!(!text(second).await.contains("Conexión de Google disponible"));
    let calls = f.provider.calls.lock().await;
    let refresh: Vec<_> = calls
      .iter()
      .filter(|(_, p, b)| {
        p == "/token"
          && b
            .as_str()
            .is_some_and(|s| s.contains("grant_type=refresh_token"))
      })
      .collect();
    assert_eq!(refresh.len(), 3);
    assert!(refresh[2]
      .2
      .as_str()
      .unwrap()
      .contains("ROTATED_REFRESH_SENTINEL"));
    drop(calls);
    f.provider
      .hold_exchange
      .store(false, std::sync::atomic::Ordering::SeqCst);
    f.connect().await;
    f.provider.exchange_release.add_permits(1);
    pending.await.unwrap();
    assert_eq!(
      f.row(&db).await.try_get::<i64>("", "generation").unwrap(),
      2
    );
    assert_eq!(
      f.row(&db)
        .await
        .try_get::<i64>("", "token_version")
        .unwrap(),
      4
    );
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn crashed_dispatched_refresh_and_wrong_key_preserve_metadata_without_provider_use() {
  with_database(|db|async move {
    let mut f=Fixture::new(&db).await;f.connect().await;
    db.conn.execute(Statement::from_string(DbBackend::Postgres,"UPDATE google_connection SET refresh_claim='00000000-0000-0000-0000-000000000001',refresh_deadline=1,refresh_dispatched=true WHERE id=1")).await.unwrap();
    authenticated_get(&f.app,"/settings/google",&f.session).await;
    assert_eq!(f.row(&db).await.try_get::<String>("","status").unwrap(),"reauthorize");
    assert_eq!(f.provider.calls.lock().await.len(),1);
    f.connect().await;
    f.app=axum::Router::new();
    // Release composition's Arc before changing only the operator key.
    Arc::get_mut(&mut f.cfg).unwrap().google.as_mut().unwrap().key=[72;32];
    f.app=crate::app::router_with_clock(f.cfg.clone(),db.clone(),f.clock.clone());
    let response=authenticated_get(&f.app,"/settings/google",&f.session).await;
    assert_eq!(response.status(),503);let body=text(response).await;
    assert!(!body.contains("action=\"/settings/google/connect\""));
    assert_eq!(f.row(&db).await.try_get::<i64>("","generation").unwrap(),2);
    assert_eq!(f.provider.calls.lock().await.len(),2);
  }).await;
}
