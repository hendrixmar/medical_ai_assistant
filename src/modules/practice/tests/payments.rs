use super::*;

pub(super) fn payment_fields() -> Vec<(&'static str, &'static str)> {
  vec![
    ("bank", "Banco de prueba"),
    ("beneficiary", "Beneficiario de prueba"),
    ("clabe", "001234567890123456"),
    ("receipt_window_minutes", "120"),
    ("review_margin_minutes", "240"),
    ("grace_minutes", "5"),
    ("review_interval_minutes", "360"),
    ("urgent_notice_minutes", "90"),
  ]
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn payments_preserve_clabe_zeros_and_other_steps_without_enabling_video() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let initial = page(&app, &session, "payments").await;
    assert_eq!(field(&initial, "review_interval_minutes"), "240");
    assert_eq!(field(&initial, "urgent_notice_minutes"), "120");
    let csrf = field(&initial, "csrf_token");
    assert_eq!(
      save(
        &app,
        &session,
        "schedule",
        &csrf,
        &schedule::schedule_fields()
      )
      .await
      .status(),
      303
    );
    let response = save(&app, &session, "payments", &csrf, &payment_fields()).await;
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/settings?saved=1");
    let saved = page(&app, &session, "payments").await;
    for (key, value) in payment_fields() {
      assert_eq!(field(&saved, key), value);
    }
    assert!(saved.contains("no habilita las consultas por video ni crea citas"));
    assert!(saved.contains("href=\"/settings/google\""));
    assert!(saved.contains("Conexión de Google Calendar"));
    let schedule = page(&app, &session, "schedule").await;
    assert_eq!(field(&schedule, "grid_minutes"), "20");
    assert_eq!(field(&schedule, "mon_hours"), "09:00-12:00, 14:00-18:00");
    assert_eq!(
      schedule
        .matches("<span class=\"step-state\">Listo</span>")
        .count(),
      2
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn invalid_bank_or_receipt_intervals_do_not_overwrite_any_payment_field() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&page(&app, &session, "payments").await, "csrf_token");
    assert_eq!(
      save(&app, &session, "payments", &csrf, &payment_fields())
        .await
        .status(),
      303
    );
    for (key, bad) in [
      ("bank", ""),
      ("beneficiary", "  "),
      ("bank", &"a".repeat(121)),
      ("beneficiary", &"a".repeat(201)),
      ("clabe", "01234567890123456"),
      ("clabe", "0123456789012345678"),
      ("clabe", "00123456789012345x"),
      ("clabe", "٠٠١٢٣٤٥٦٧٨٩٠١٢٣٤٥٦"),
      ("clabe", "001 23456789012345"),
      ("receipt_window_minutes", ""),
      ("receipt_window_minutes", "0"),
      ("receipt_window_minutes", "-1"),
      ("receipt_window_minutes", "2147483648"),
      ("receipt_window_minutes", "1.5"),
      ("review_margin_minutes", "-1"),
      ("review_margin_minutes", ""),
      ("grace_minutes", "-1"),
      ("grace_minutes", ""),
      ("review_interval_minutes", "0"),
      ("urgent_notice_minutes", "0"),
    ] {
      let fields = changed(changed(payment_fields(), "bank", "No guardar"), key, bad);
      let response = save(&app, &session, "payments", &csrf, &fields).await;
      assert_eq!(response.status(), 422, "{key}: {bad}");
      let html = text(response).await;
      assert_eq!(field(&html, key), bad);
      assert!(html.contains(&format!("id=\"{key}-error\"")));
      let saved = page(&app, &session, "payments").await;
      for (key, value) in payment_fields() {
        assert_eq!(field(&saved, key), value);
      }
    }
    assert_eq!(
      save(&app, &session, "payments", &csrf, &[]).await.status(),
      422
    );
    // CLABE format and Review Margin/Grace remain format-only; only notification intervals require positive values.
    let fields = changed(
      changed(
        changed(payment_fields(), "clabe", "000000000000000000"),
        "review_margin_minutes",
        "0",
      ),
      "grace_minutes",
      "0",
    );
    assert_eq!(
      save(&app, &session, "payments", &csrf, &fields)
        .await
        .status(),
      303
    );
    let saved = page(&app, &session, "payments").await;
    assert_eq!(field(&saved, "clabe"), "000000000000000000");
    assert_eq!(field(&saved, "grace_minutes"), "0");
    assert!(saved.contains("Nunca amplía la hora límite"));
  })
  .await;
}
