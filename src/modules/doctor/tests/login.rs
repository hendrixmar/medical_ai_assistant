use super::*;
use crate::modules::doctor::{crypto, store, Clock};
use sea_orm::ConnectionTrait;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn either_factor_alone_wrong_and_expired_codes_create_no_sessions() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    let (preauth, csrf) = challenge(&app).await;
    let valid = totp.generate(clock.now() as u64).to_string();
    let expired = totp.generate((clock.now() - 90) as u64).to_string();
    for (password, code) in [
      ("", valid.as_str()),
      (PASSWORD, ""),
      ("incorrect but long password", valid.as_str()),
      (PASSWORD, "invalid"),
      (PASSWORD, expired.as_str()),
    ] {
      let response = login(&app, &preauth, &csrf, password, code, "/dashboard").await;
      assert_eq!(response.status(), 401);
      assert!(!response.headers().contains_key("set-cookie"));
      let html = text(response).await;
      assert!(html.contains("No pudimos iniciar sesión"));
      assert!(!html.contains(PASSWORD));
      assert!(!html.contains(&valid));
    }
    assert_eq!(count(&db, "doctor_sessions").await, 0);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn simultaneous_code_replay_allows_exactly_one_session_and_consumes_matched_step() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    let (a_cookie, a_csrf) = challenge(&app).await;
    let (b_cookie, b_csrf) = challenge(&app).await;
    // A future skew-window match must consume that counter, not the local-time counter.
    let code = totp.generate((clock.now() + 30) as u64).to_string();
    let (a, b) = tokio::join!(
      login(&app, &a_cookie, &a_csrf, PASSWORD, &code, "/dashboard"),
      login(&app, &b_cookie, &b_csrf, PASSWORD, &code, "/dashboard")
    );
    let mut statuses = [a.status().as_u16(), b.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [303, 401]);
    assert_eq!(count(&db, "doctor_sessions").await, 1);
    clock.advance(30);
    let (preauth, csrf) = challenge(&app).await;
    assert_eq!(
      login(&app, &preauth, &csrf, PASSWORD, &code, "/dashboard")
        .await
        .status(),
      401
    );
    clock.advance(30);
    let code = totp.generate(clock.now() as u64).to_string();
    assert_eq!(
      login(&app, &preauth, &csrf, PASSWORD, &code, "/dashboard")
        .await
        .status(),
      303
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn fixed_window_counts_both_factor_failures_survives_router_restart_and_precedes_bcrypt() {
  with_database(|db| async move {
    let (cfg, clock, app, totp) = fixture(&db).await;
    let (preauth, csrf) = challenge(&app).await;
    let code = totp.generate(clock.now() as u64).to_string();
    for attempt in 0..5 {
      let (password, code) = if attempt % 2 == 0 {
        (PASSWORD, "wrong-code")
      } else {
        ("wrong but long password", code.as_str())
      };
      assert_eq!(
        login(&app, &preauth, &csrf, password, code, "/dashboard")
          .await
          .status(),
        401
      );
    }
    let old_hash: String = store::doctor(&db)
      .await
      .unwrap()
      .unwrap()
      .try_get("", "password_hash")
      .unwrap();
    // A bcrypt verification would fail with 503 on this invalid hash. Rate rejection must happen first.
    db.conn
      .execute(store::sql(
        "UPDATE doctor SET password_hash = 'invalid-bcrypt-hash'",
        vec![],
      ))
      .await
      .unwrap();
    let restarted = crate::app::router_with_clock(cfg, db.clone(), clock.clone());
    let response = login(&restarted, &preauth, &csrf, PASSWORD, &code, "/dashboard").await;
    assert_eq!(response.status(), 429);
    assert_eq!(response.headers()["retry-after"], "300");
    clock.advance(240);
    let response = login(&restarted, &preauth, &csrf, PASSWORD, &code, "/dashboard").await;
    assert_eq!(response.status(), 429);
    assert_eq!(response.headers()["retry-after"], "60");
    db.conn
      .execute(store::sql(
        "UPDATE doctor SET password_hash = $1",
        vec![old_hash.into()],
      ))
      .await
      .unwrap();
    clock.advance(60);
    let code = totp.generate(clock.now() as u64).to_string();
    assert_eq!(
      login(&restarted, &preauth, &csrf, PASSWORD, &code, "/dashboard")
        .await
        .status(),
      303
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn login_csrf_requires_bound_cookie_token_and_exact_origin_without_spending_attempts() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    let (preauth, csrf) = challenge(&app).await;
    let (other_cookie, _) = challenge(&app).await;
    let code = totp.generate(clock.now() as u64).to_string();
    for (cookie, token, origin) in [
      (None, csrf.as_str(), Some("https://practice.test")),
      (Some(preauth.as_str()), "", Some("https://practice.test")),
      (
        Some(preauth.as_str()),
        "incorrect",
        Some("https://practice.test"),
      ),
      (
        Some(other_cookie.as_str()),
        csrf.as_str(),
        Some("https://practice.test"),
      ),
      (Some(preauth.as_str()), csrf.as_str(), None),
      (Some(preauth.as_str()), csrf.as_str(), Some("null")),
      (
        Some(preauth.as_str()),
        csrf.as_str(),
        Some("https://evil.test"),
      ),
      (
        Some(preauth.as_str()),
        csrf.as_str(),
        Some("https://practice.test.evil.test"),
      ),
      (
        Some(preauth.as_str()),
        csrf.as_str(),
        Some("https://practice.test:444"),
      ),
    ] {
      let response = post(
        &app,
        "/login",
        cookie,
        origin,
        &[
          ("csrf_token", token),
          ("password", PASSWORD),
          ("code", &code),
        ],
      )
      .await;
      assert_eq!(response.status(), 403);
      assert_eq!(count(&db, "doctor_sessions").await, 0);
    }
    let attempts: i32 = db
      .conn
      .query_one(store::sql(
        "SELECT attempts FROM doctor_login_limit",
        vec![],
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "attempts")
      .unwrap();
    assert_eq!(attempts, 0);
    assert_eq!(
      login(&app, &preauth, &csrf, PASSWORD, &code, "/dashboard")
        .await
        .status(),
      303
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn concurrent_failures_cannot_overrun_limit_or_bypass_it_with_forwarded_addresses() {
  with_database(|db| async move {
    let (_, _, app, _) = fixture(&db).await;
    let (preauth, csrf) = challenge(&app).await;
    let mut requests = tokio::task::JoinSet::new();
    for attempt in 0..12 {
      let app = app.clone();
      let preauth = preauth.clone();
      let csrf = csrf.clone();
      requests.spawn(async move {
        let body = reqwest::Url::parse_with_params(
          "https://practice.test",
          [
            ("csrf_token", csrf.as_str()),
            ("password", "incorrect long password"),
            ("code", "000000"),
          ],
        )
        .unwrap()
        .query()
        .unwrap()
        .to_owned();
        app
          .oneshot(
            Request::builder()
              .method("POST")
              .uri("/login")
              .header("origin", "https://practice.test")
              .header("cookie", preauth)
              .header("x-forwarded-for", format!("192.0.2.{attempt}"))
              .header("forwarded", format!("for=192.0.2.{attempt};proto=https"))
              .header("content-type", "application/x-www-form-urlencoded")
              .body(Body::from(body))
              .unwrap(),
          )
          .await
          .unwrap()
          .status()
          .as_u16()
      });
    }
    let mut statuses = Vec::new();
    while let Some(result) = requests.join_next().await {
      statuses.push(result.unwrap());
    }
    assert_eq!(statuses.iter().filter(|&&code| code == 401).count(), 5);
    assert_eq!(statuses.iter().filter(|&&code| code == 429).count(), 7);
    assert_eq!(count(&db, "doctor_sessions").await, 0);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn challenges_expire_are_hashed_and_issuance_remains_bounded_with_cleanup() {
  with_database(|db| async move {
    let (cfg, clock, app, totp) = fixture(&db).await;
    let response = get(&app, "/login").await;
    let header = response.headers()["set-cookie"].to_str().unwrap();
    for flag in [
      "Secure",
      "HttpOnly",
      "SameSite=Lax",
      "Path=/",
      "Max-Age=600",
    ] {
      assert!(header.contains(flag));
    }
    let preauth = cookie(&response, "__Host-doctor_preauth");
    let token = preauth.split_once('=').unwrap().1;
    let csrf = field(&text(response).await, "csrf_token");
    let stored = db
      .conn
      .query_one(store::sql(
        "SELECT token_hash, csrf_token FROM doctor_login_challenges",
        vec![],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      stored.try_get::<Vec<u8>>("", "token_hash").unwrap(),
      crypto::hash(token)
    );
    assert_ne!(csrf, token);
    for _ in 1..20 {
      assert_eq!(get(&app, "/login").await.status(), 200);
    }
    let restarted = crate::app::router_with_clock(cfg, db.clone(), clock.clone());
    for _ in 0..30 {
      assert_eq!(get(&restarted, "/login").await.status(), 429);
    }
    assert_eq!(count(&db, "doctor_login_challenges").await, 20);
    clock.advance(300);
    for _ in 0..20 {
      assert_eq!(get(&app, "/login").await.status(), 200);
    }
    assert_eq!(count(&db, "doctor_login_challenges").await, 40);
    clock.advance(300);
    let code = totp.generate(clock.now() as u64).to_string();
    assert_eq!(
      login(&app, &preauth, &csrf, PASSWORD, &code, "/dashboard")
        .await
        .status(),
      403
    );
    assert_eq!(get(&app, "/login").await.status(), 200);
    assert_eq!(count(&db, "doctor_login_challenges").await, 21);
    assert_eq!(count(&db, "doctor_sessions").await, 0);
  })
  .await;
}
