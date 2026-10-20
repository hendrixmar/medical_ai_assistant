use super::*;

pub(super) fn details_fields() -> Vec<(&'static str, &'static str)> {
  vec![
    ("name", "Consultorio de prueba"),
    ("location", "Ubicación de prueba"),
    ("timezone", "America/Tijuana"),
  ]
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn invalid_details_keep_posted_values_but_not_partial_writes_or_missing_old_values() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&page(&app, &session, "details").await, "csrf_token");
    assert_eq!(
      save(&app, &session, "details", &csrf, &details_fields())
        .await
        .status(),
      303
    );
    for (key, invalid) in [
      ("timezone", ""),
      ("timezone", "Mexico/Invented"),
      ("location", "   "),
      ("name", &"a".repeat(121)),
      ("location", &"ñ".repeat(501)),
    ] {
      let fields = changed(
        changed(details_fields(), "name", "Sin guardar"),
        key,
        invalid,
      );
      let response = save(&app, &session, "details", &csrf, &fields).await;
      assert_eq!(response.status(), 422, "{key}: {invalid}");
      let html = text(response).await;
      assert!(html.contains("No se guardaron los cambios"));
      let stored = page(&app, &session, "details").await;
      assert_eq!(field(&stored, "name"), "Consultorio de prueba");
      assert!(stored.contains("Ubicación de prueba"));
      assert!(selected(&stored, "timezone", "America/Tijuana"));
    }
    let response = save(
      &app,
      &session,
      "details",
      &csrf,
      &[("location", "Dirección enviada")],
    )
    .await;
    assert_eq!(response.status(), 422);
    let html = text(response).await;
    assert!(
      selected(&html, "timezone", ""),
      "a missing posted choice must not appear to retain the old choice"
    );
    assert_eq!(field(&html, "name"), "");
    assert!(html.contains("Dirección enviada"));
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn explicit_timezone_examples_follow_iana_dst_and_support_zones_outside_mexico() {
  with_database(|db| async move {
    let (_, clock, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&page(&app, &session, "details").await, "csrf_token");
    let fields = changed(
      changed(details_fields(), "timezone", "Asia/Tokyo"),
      "name",
      "",
    );
    assert_eq!(
      save(&app, &session, "details", &csrf, &fields)
        .await
        .status(),
      303
    );
    let html = page(&app, &session, "details").await;
    assert_eq!(field(&html, "name"), "");
    assert!(html.contains("2027-01-15 17:00:30 +09:00"));
    assert!(selected(&html, "timezone", "Asia/Tokyo"));
    // Cross the March DST transition and sign in again after session expiry.
    clock.advance(60 * 24 * 60 * 60);
    let session = signed_in(&app, &totp, NOW + 30 + 60 * 24 * 60 * 60).await;
    let csrf = field(&page(&app, &session, "details").await, "csrf_token");
    assert_eq!(
      save(&app, &session, "details", &csrf, &details_fields())
        .await
        .status(),
      303
    );
    let html = page(&app, &session, "details").await;
    assert!(html.contains("2027-03-16 01:00:30 -07:00"));
    assert!(selected(&html, "timezone", "America/Tijuana"));
  })
  .await;
}
