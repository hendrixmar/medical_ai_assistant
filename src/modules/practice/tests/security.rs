use super::*;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn every_settings_write_reuses_auth_origin_and_csrf_without_effects() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&page(&app, &session, "details").await, "csrf_token");
    assert_eq!(
      save(
        &app,
        &session,
        "payments",
        &csrf,
        &payments::payment_fields()
      )
      .await
      .status(),
      303
    );
    let id = uuid::Uuid::now_v7().to_string();
    for (step, fields) in [
      ("details", details::details_fields()),
      ("schedule", schedule::schedule_fields()),
      (
        "payments",
        changed(payments::payment_fields(), "bank", "No escribir"),
      ),
      (
        "services",
        vec![
          ("service_id", id.as_str()),
          ("service_name", "No escribir"),
          ("duration_minutes", "30"),
          ("price_mxn", "0"),
        ],
      ),
    ] {
      let path = format!("/settings/{step}");
      for (cookie, origin, token, status) in [
        (None, Some("https://practice.test"), csrf.as_str(), 401),
        (Some(session.as_str()), None, csrf.as_str(), 403),
        (
          Some(session.as_str()),
          Some("https://evil.test"),
          csrf.as_str(),
          403,
        ),
        (
          Some(session.as_str()),
          Some("https://practice.test.evil.test"),
          csrf.as_str(),
          403,
        ),
        (Some(session.as_str()), Some("null"), csrf.as_str(), 403),
        (
          Some(session.as_str()),
          Some("https://practice.test"),
          "wrong",
          403,
        ),
        (
          Some(session.as_str()),
          Some("https://practice.test"),
          "",
          403,
        ),
      ] {
        let mut submitted = fields.clone();
        submitted.push(("csrf_token", token));
        let response = post(&app, &path, cookie, origin, &submitted).await;
        assert_eq!(response.status(), status, "{step}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["referrer-policy"], "same-origin");
        assert!(!response
          .headers()
          .contains_key("access-control-allow-credentials"));
      }
      assert_eq!(
        post(
          &app,
          &path,
          Some(&session),
          Some("https://practice.test"),
          &fields
        )
        .await
        .status(),
        403
      );
      let mut duplicate = fields.clone();
      duplicate.extend([("csrf_token", csrf.as_str()), ("csrf_token", csrf.as_str())]);
      assert_eq!(
        post(
          &app,
          &path,
          Some(&session),
          Some("https://practice.test"),
          &duplicate
        )
        .await
        .status(),
        403
      );
      assert_eq!(authenticated_get(&app, &path, &session).await.status(), 405);
      let response = get(&app, &format!("/settings?step={step}")).await;
      assert_eq!(response.status(), 303);
      assert_eq!(response.headers()["location"], "/login?return_to=/settings");
      assert!(!text(response).await.contains("001234567890123456"));
    }
    let saved = page(&app, &session, "details").await;
    assert_eq!(field(&saved, "name"), "");
    assert!(selected(&saved, "timezone", ""));
    assert_eq!(
      field(&page(&app, &session, "schedule").await, "grid_minutes"),
      "30"
    );
    assert!(page(&app, &session, "services")
      .await
      .contains("Aún no hay servicios guardados"));
    let saved = page(&app, &session, "payments").await;
    for (key, value) in payments::payment_fields() {
      assert_eq!(field(&saved, key), value);
    }
    for path in ["/login", "/api/v1/health"] {
      assert!(!text(get(&app, path).await)
        .await
        .contains("001234567890123456"));
    }
    assert_eq!(get(&app, "/setup").await.status(), 404);
    assert_eq!(
      post(
        &app,
        "/setup",
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", &csrf)]
      )
      .await
      .status(),
      404
    );
    let unknown = authenticated_get(
      &app,
      "/settings?step=https://evil.test&return_to=https://evil.test",
      &session,
    )
    .await;
    assert_eq!(unknown.status(), 200);
    assert!(!text(unknown).await.contains("evil.test"));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn unicode_policy_uses_protected_64k_bound_while_login_keeps_8k_limit() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&page(&app, &session, "schedule").await, "csrf_token");
    let policy = "á".repeat(4000);
    let fields = changed(schedule::schedule_fields(), "cancellation_policy", &policy);
    let response = save(&app, &session, "schedule", &csrf, &fields).await;
    assert_eq!(response.status(), 303);
    assert!(page(&app, &session, "schedule").await.contains(&policy));
    let huge = "a".repeat(64 * 1024);
    let response = save(
      &app,
      &session,
      "schedule",
      &csrf,
      &changed(fields, "cancellation_policy", &huge),
    )
    .await;
    assert_eq!(response.status(), 403);
    assert!(page(&app, &session, "schedule").await.contains(&policy));
    let (preauth, token) = challenge(&app).await;
    let response = login(
      &app,
      &preauth,
      &token,
      &"p".repeat(8193),
      "123456",
      "/dashboard",
    )
    .await;
    assert_eq!(response.status(), 403);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn stored_and_rejected_values_are_escaped_on_the_real_settings_pages() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&page(&app, &session, "details").await, "csrf_token");
    let attack = "\"'><script>alert(1)</script>&";
    let escaped = "&quot;&#39;&gt;&lt;script&gt;alert(1)&lt;/script&gt;&amp;";
    let id = uuid::Uuid::now_v7().to_string();
    for (step, fields) in [
      (
        "details",
        changed(
          changed(details::details_fields(), "name", attack),
          "location",
          attack,
        ),
      ),
      (
        "services",
        vec![
          ("service_id", id.as_str()),
          ("service_name", attack),
          ("duration_minutes", "30"),
          ("price_mxn", "0"),
        ],
      ),
      (
        "schedule",
        changed(schedule::schedule_fields(), "cancellation_policy", attack),
      ),
      (
        "payments",
        changed(
          changed(payments::payment_fields(), "bank", attack),
          "beneficiary",
          attack,
        ),
      ),
    ] {
      assert_eq!(
        save(&app, &session, step, &csrf, &fields).await.status(),
        303
      );
      let html = page(&app, &session, step).await;
      assert!(html.contains(escaped), "{step}");
      assert!(!html.contains(attack));
      assert!(!html.contains("<script>"));
    }
    let response = save(
      &app,
      &session,
      "details",
      &csrf,
      &changed(details::details_fields(), "timezone", attack),
    )
    .await;
    assert_eq!(response.status(), 422);
    let html = text(response).await;
    assert!(html.contains(escaped));
    assert!(!html.contains(attack));
  })
  .await;
}
