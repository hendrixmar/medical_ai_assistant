use super::*;
use crate::modules::doctor::{crypto, store, views, Clock};
use sea_orm::ConnectionTrait;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn session_cookie_is_opaque_hashed_and_absolute_thirty_day_expiry_survives_restart() {
  with_database(|db| async move {
    let (cfg, clock, app, totp) = fixture(&db).await;
    let (preauth, csrf) = challenge(&app).await;
    let response = login(
      &app,
      &preauth,
      &csrf,
      PASSWORD,
      &totp.generate(clock.now() as u64).to_string(),
      "/dashboard",
    )
    .await;
    assert_eq!(response.status(), 303);
    let session = cookie(&response, "__Host-doctor_session");
    let header = response
      .headers()
      .get_all("set-cookie")
      .iter()
      .find(|v| v.to_str().unwrap().starts_with("__Host-doctor_session="))
      .unwrap()
      .to_str()
      .unwrap();
    for flag in [
      "Secure",
      "HttpOnly",
      "SameSite=Lax",
      "Path=/",
      "Max-Age=2592000",
    ] {
      assert!(header.contains(flag));
    }
    assert!(!header.contains("Domain="));
    let token = session.split_once('=').unwrap().1;
    use base64::Engine;
    assert_eq!(
      base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .unwrap()
        .len(),
      32
    );
    let stored = db
      .conn
      .query_one(store::sql(
        "SELECT token_hash, csrf_token, created_at, expires_at FROM doctor_sessions",
        vec![],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      stored.try_get::<Vec<u8>>("", "token_hash").unwrap(),
      crypto::hash(token)
    );
    let session_csrf: String = stored.try_get("", "csrf_token").unwrap();
    assert_ne!(session_csrf, token);
    assert_ne!(session_csrf, csrf);
    assert_eq!(
      stored.try_get::<i64>("", "expires_at").unwrap()
        - stored.try_get::<i64>("", "created_at").unwrap(),
      2_592_000
    );
    assert_eq!(count(&db, "doctor_login_challenges").await, 0);
    let restarted = crate::app::router_with_clock(cfg, db.clone(), clock.clone());
    clock.advance(2_592_000 - 1);
    let response = authenticated_get(&restarted, "/dashboard", &session).await;
    assert_eq!(response.status(), 200);
    assert_eq!(field(&text(response).await, "csrf_token"), session_csrf);
    clock.advance(1);
    let response = authenticated_get(&restarted, "/dashboard", &session).await;
    assert_eq!(response.status(), 303);
    assert_eq!(
      response.headers()["location"],
      "/login?return_to=/dashboard"
    );
    assert_eq!(
      post(
        &restarted,
        "/logout",
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", &session_csrf)]
      )
      .await
      .status(),
      401
    );
    assert_eq!(get(&restarted, "/login").await.status(), 200);
    assert_eq!(count(&db, "doctor_sessions").await, 0);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn logout_rejects_missing_wrong_and_cross_origin_csrf_without_mutation_then_revokes() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, clock.now()).await;
    let csrf = field(
      &text(authenticated_get(&app, "/dashboard", &session).await).await,
      "csrf_token",
    );
    for (token, origin) in [
      ("", Some("https://practice.test")),
      ("invalid", Some("https://practice.test")),
      (csrf.as_str(), None),
      (csrf.as_str(), Some("null")),
      (csrf.as_str(), Some("https://evil.test")),
      (csrf.as_str(), Some("http://practice.test")),
    ] {
      assert_eq!(
        post(
          &app,
          "/logout",
          Some(&session),
          origin,
          &[("csrf_token", token)]
        )
        .await
        .status(),
        403
      );
      assert_eq!(count(&db, "doctor_sessions").await, 1);
      assert_eq!(
        authenticated_get(&app, "/settings", &session)
          .await
          .status(),
        200
      );
    }
    assert_eq!(
      post(
        &app,
        "/logout",
        None,
        Some("https://practice.test"),
        &[("csrf_token", &csrf)]
      )
      .await
      .status(),
      401
    );
    let response = post(
      &app,
      "/logout",
      Some(&session),
      Some("https://practice.test"),
      &[("csrf_token", &csrf)],
    )
    .await;
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/login");
    assert!(response.headers()["set-cookie"]
      .to_str()
      .unwrap()
      .contains("Max-Age=0"));
    assert_eq!(count(&db, "doctor_sessions").await, 0);
    assert_eq!(
      authenticated_get(&app, "/dashboard", &session)
        .await
        .status(),
      303
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn anonymous_and_forged_deep_links_cannot_read_shell_and_no_setup_route_exists() {
  with_database(|db| async move {
    let (_, _, app, _) = fixture(&db).await;
    for path in [
      "/dashboard",
      "/settings",
      "/dashboard?phone=5215555555555&notification=secret-link",
      "/settings?doctor=true",
    ] {
      for session in [
        "",
        "__Host-doctor_session=forged",
        "__Host-doctor_session=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
      ] {
        let response = authenticated_get(&app, path, session).await;
        assert_eq!(response.status(), 303);
        assert!(response.headers()["location"]
          .to_str()
          .unwrap()
          .starts_with("/login?return_to=/"));
        assert!(!text(response).await.contains("Necesita tu atención"));
      }
    }
    for path in [
      "/setup",
      "/register",
      "/bootstrap-doctor",
      "/dashboard/appointments/123",
    ] {
      assert_eq!(get(&app, path).await.status(), 404);
      assert_eq!(post(&app, path, None, None, &[]).await.status(), 404);
    }
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn login_return_navigation_is_an_exact_allowlist() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    for (target, expected) in [
      ("/settings", "/settings"),
      ("/settings/whatsapp", "/settings/whatsapp"),
      ("/settings/doctor-number", "/settings/doctor-number"),
      ("/settings/doctor-number/challenge", "/dashboard"),
      ("/settings/doctor-number?success=true", "/dashboard"),
      ("/settings/whatsapp/qr", "/dashboard"),
      ("/settings/whatsapp?next=https://evil.test", "/dashboard"),
      ("/dashboard", "/dashboard"),
      ("https://evil.test", "/dashboard"),
      ("//evil.test", "/dashboard"),
      ("/settings?next=https://evil.test", "/dashboard"),
      ("/dashboard\r\nLocation: https://evil.test", "/dashboard"),
      ("/settings/..//evil.test", "/dashboard"),
    ] {
      let url =
        reqwest::Url::parse_with_params("https://practice.test/login", [("return_to", target)])
          .unwrap();
      let response = get(&app, &format!("/login?{}", url.query().unwrap())).await;
      assert_eq!(response.status(), 200);
      let preauth = cookie(&response, "__Host-doctor_preauth");
      let html = text(response).await;
      assert_eq!(field(&html, "return_to"), expected);
      let csrf = field(&html, "csrf_token");
      let response = login(
        &app,
        &preauth,
        &csrf,
        PASSWORD,
        &totp.generate(clock.now() as u64).to_string(),
        target,
      )
      .await;
      assert_eq!(response.status(), 303);
      assert_eq!(response.headers()["location"], expected);
      clock.advance(300);
    }
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn real_views_have_security_headers_ordered_sections_and_no_secrets() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, clock.now()).await;
    for path in ["/login", "/dashboard", "/settings", "/assets/doctor.css"] {
      let response = authenticated_get(&app, path, &session).await;
      assert_eq!(response.status(), 200);
      assert_eq!(response.headers()["cache-control"], "no-store");
      assert_eq!(response.headers()["x-frame-options"], "DENY");
      assert_eq!(response.headers()["x-content-type-options"], "nosniff");
      // no-referrer makes native browser POST navigation send Origin: null,
      // which correctly fails our exact-Origin CSRF boundary.
      assert_eq!(response.headers()["referrer-policy"], "same-origin");
      let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
      assert!(
        csp.contains("style-src 'self'")
          && csp.contains("script-src 'none'")
          && csp.contains("frame-ancestors 'none'")
      );
      let html = text(response).await;
      assert!(!html.contains(PASSWORD));
      assert!(!html.contains("otpauth://"));
      if path == "/dashboard" {
        assert!(
          html.find("Necesita tu atención").unwrap() < html.find("Tus próximas citas").unwrap()
        );
      }
    }
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn cors_forwarded_origin_and_missing_form_fields_cannot_authorize_logout() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, clock.now()).await;
    let csrf = field(
      &text(authenticated_get(&app, "/dashboard", &session).await).await,
      "csrf_token",
    );
    let response = post(
      &app,
      "/logout",
      Some(&session),
      Some("https://practice.test"),
      &[],
    )
    .await;
    assert_eq!(response.status(), 403);
    for origins in [
      vec!["https://evil.test"],
      vec!["https://practice.test", "https://evil.test"],
    ] {
      let mut request = Request::builder()
        .method("POST")
        .uri("/logout")
        .header("cookie", &session)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("host", "practice.test")
        .header("x-forwarded-host", "practice.test")
        .header("x-forwarded-proto", "https");
      for origin in origins {
        request = request.header("origin", origin);
      }
      let response = app
        .clone()
        .oneshot(
          request
            .body(Body::from(format!("csrf_token={csrf}")))
            .unwrap(),
        )
        .await
        .unwrap();
      assert_eq!(response.status(), 403);
      assert!(!response
        .headers()
        .contains_key("access-control-allow-credentials"));
      assert_eq!(response.headers()["cache-control"], "no-store");
      assert_eq!(count(&db, "doctor_sessions").await, 1);
    }
    assert_eq!(
      authenticated_get(&app, "/logout", &session).await.status(),
      405
    );
    assert_eq!(count(&db, "doctor_sessions").await, 1);
  })
  .await;
}

#[test]
fn actual_views_escape_all_dynamic_values() {
  let attack = "\"'><script>&";
  let escaped = "&quot;&#39;&gt;&lt;script&gt;&amp;";
  for html in [
    views::login(views::LoginView {
      csrf_token: attack,
      return_to: attack,
      error: None,
    }),
    crate::modules::conversation::views::dashboard(
      crate::modules::conversation::views::DashboardView {
        csrf_token: attack,
        items: &[],
        notice: Some(attack),
        load_error: false,
        appointment_load_error: false,
        appointments: &[],
        google_status: crate::modules::conversation::views::GoogleConnectionStatus::NotConfigured,
        outbound_enabled: false,
        queued_count: 0,
        rate_deferred_count: 0,
        google_cleanup_count: 0,
        google_load_error: false,
        previous_page: None,
        next_page: None,
      },
    ),
  ] {
    assert!(!html.contains(attack));
    assert!(html.contains(escaped));
    assert!(!html.contains("<script>"));
  }
}
