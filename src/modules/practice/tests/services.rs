use super::*;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn service_identity_is_stable_on_retry_and_edit_with_exact_centavos() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let initial = page(&app, &session, "services").await;
    let csrf = field(&initial, "csrf_token");
    let id = field(&initial, "service_id");
    assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 7);
    for _ in 0..2 {
      let response = save(
        &app,
        &session,
        "services",
        &csrf,
        &[
          ("service_id", &id),
          ("service_name", "Consulta de prueba"),
          ("duration_minutes", "45"),
          ("price_mxn", "1234.50"),
        ],
      )
      .await;
      assert_eq!(response.status(), 303);
      assert_eq!(
        response.headers()["location"],
        "/settings?step=services&saved=1"
      );
    }
    let saved = page(&app, &session, "services").await;
    assert_eq!(
      saved
        .matches(&format!(
          "href=\"/settings?step=services&amp;service={id}\""
        ))
        .count(),
      1,
      "{saved}"
    );
    let edit = page(&app, &session, &format!("services&service={id}")).await;
    assert_eq!(field(&edit, "service_id"), id);
    assert_eq!(field(&edit, "price_mxn"), "1234.50");
    assert_eq!(field(&edit, "duration_minutes"), "45");
    assert_eq!(
      save(
        &app,
        &session,
        "services",
        &csrf,
        &[
          ("service_id", &id),
          ("service_name", "Consulta editada"),
          ("duration_minutes", "30"),
          ("price_mxn", "0"),
        ]
      )
      .await
      .status(),
      303
    );
    let edit = page(&app, &session, &format!("services&service={id}")).await;
    assert_eq!(field(&edit, "service_name"), "Consulta editada");
    assert_eq!(field(&edit, "price_mxn"), "0.00");
    assert_eq!(field(&edit, "service_id"), id);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn invalid_service_numbers_and_identity_never_partially_overwrite_a_service() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let initial = page(&app, &session, "services").await;
    let csrf = field(&initial, "csrf_token");
    let id = field(&initial, "service_id");
    let valid = vec![
      ("service_id", id.as_str()),
      ("service_name", "Original"),
      ("duration_minutes", "45"),
      ("price_mxn", "1.05"),
    ];
    assert_eq!(
      save(&app, &session, "services", &csrf, &valid)
        .await
        .status(),
      303
    );
    for (key, bad) in [
      ("price_mxn", "-1"),
      ("price_mxn", "1e2"),
      ("price_mxn", "1,50"),
      ("price_mxn", "1.001"),
      ("price_mxn", "92233720368547758.08"),
      ("price_mxn", "9223372036854775808"),
      ("price_mxn", ".50"),
      ("price_mxn", "1."),
      ("price_mxn", ""),
      ("price_mxn", "+1"),
      ("price_mxn", "NaN"),
      ("duration_minutes", "0"),
      ("duration_minutes", "-1"),
      ("duration_minutes", "1.5"),
      ("duration_minutes", "2147483648"),
      ("duration_minutes", ""),
      ("service_name", "  "),
      ("service_name", &"a".repeat(121)),
      ("service_id", "invalid"),
    ] {
      let fields = changed(
        changed(valid.clone(), "service_name", "No guardar"),
        key,
        bad,
      );
      let response = save(&app, &session, "services", &csrf, &fields).await;
      assert_eq!(response.status(), 422, "{key}: {bad}");
      let html = text(response).await;
      assert_eq!(field(&html, key), bad);
      let stored = page(&app, &session, &format!("services&service={id}")).await;
      assert_eq!(field(&stored, "service_name"), "Original");
      assert_eq!(field(&stored, "duration_minutes"), "45");
      assert_eq!(field(&stored, "price_mxn"), "1.05");
    }
    for (input, expected) in [
      ("92233720368547758.07", "92233720368547758.07"),
      ("1.5", "1.50"),
      ("000.01", "0.01"),
      ("0", "0.00"),
    ] {
      assert_eq!(
        save(
          &app,
          &session,
          "services",
          &csrf,
          &changed(valid.clone(), "price_mxn", input)
        )
        .await
        .status(),
        303
      );
      assert_eq!(
        field(
          &page(&app, &session, &format!("services&service={id}")).await,
          "price_mxn"
        ),
        expected
      );
    }
    for service in ["invalid".into(), uuid::Uuid::now_v7().to_string()] {
      assert_eq!(
        authenticated_get(
          &app,
          &format!("/settings?step=services&service={service}"),
          &session
        )
        .await
        .status(),
        404
      );
    }
  })
  .await;
}
