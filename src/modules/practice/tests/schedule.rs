use super::*;

pub(super) fn schedule_fields() -> Vec<(&'static str, &'static str)> {
  vec![
    ("mon_status", "open"),
    ("mon_hours", "09:00-12:00, 14:00-18:00"),
    ("tue_status", "open"),
    ("tue_hours", "22:00-24:00"),
    ("wed_status", "closed"),
    ("thu_status", "closed"),
    ("fri_status", "closed"),
    ("sat_status", "closed"),
    ("sun_status", "closed"),
    ("buffer_before_minutes", "0"),
    ("buffer_after_minutes", "15"),
    ("minimum_notice_minutes", "60"),
    ("grid_minutes", "20"),
    ("horizon_days", "40"),
    ("reminder_2h_enabled", "true"),
    (
      "cancellation_policy",
      "Política elegida por el consultorio de prueba.",
    ),
  ]
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn schedule_saves_explicit_weekdays_multiple_ranges_and_policy() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let initial = page(&app, &session, "schedule").await;
    assert!(!initial.contains(
      "name=\"reminder_2h_enabled\" type=\"checkbox\" value=\"true\" checked"
    ));
    let csrf = field(&initial, "csrf_token");
    let response = save(&app, &session, "schedule", &csrf, &schedule_fields()).await;
    assert_eq!(response.status(), 303);
    assert_eq!(
      response.headers()["location"],
      "/settings?step=payments&saved=1"
    );
    let saved = page(&app, &session, "schedule").await;
    assert_eq!(field(&saved, "mon_hours"), "09:00-12:00, 14:00-18:00");
    assert_eq!(field(&saved, "tue_hours"), "22:00-24:00");
    assert_eq!(field(&saved, "buffer_before_minutes"), "0");
    assert_eq!(field(&saved, "minimum_notice_minutes"), "60");
    assert_eq!(field(&saved, "grid_minutes"), "20");
    assert_eq!(field(&saved, "horizon_days"), "40");
    assert!(saved.contains(
      "name=\"reminder_2h_enabled\" type=\"checkbox\" value=\"true\" checked"
    ));
    assert!(saved.contains("Política elegida por el consultorio de prueba."));
    assert_eq!(
      saved
        .matches("<span class=\"step-state\">Listo</span>")
        .count(),
      1
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn malformed_weekdays_and_intervals_cannot_partially_replace_saved_schedule() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&page(&app, &session, "schedule").await, "csrf_token");
    assert_eq!(
      save(&app, &session, "schedule", &csrf, &schedule_fields())
        .await
        .status(),
      303
    );
    for (key, bad) in [
      ("mon_hours", "09:00-12:00, 11:59-13:00"),
      ("mon_hours", "22:00-02:00"),
      ("mon_hours", "09:00-09:00"),
      ("mon_hours", "24:00-24:00"),
      ("mon_hours", "09:00-24:01"),
      ("mon_hours", "9:00-12:00"),
      ("mon_hours", "09:60-12:00"),
      ("mon_hours", "09:00-12:00,"),
      ("mon_hours", ""),
      ("mon_hours", "ññ:00-12:00"),
      ("mon_hours", "09:00"),
      ("wed_hours", "09:00-12:00"),
      ("sun_status", ""),
      ("sun_status", "yes"),
      ("buffer_before_minutes", "-1"),
      ("buffer_after_minutes", "-1"),
      ("minimum_notice_minutes", "-1"),
      ("grid_minutes", "0"),
      ("horizon_days", "0"),
      ("buffer_after_minutes", "2147483648"),
      ("minimum_notice_minutes", "1.5"),
      ("buffer_before_minutes", ""),
      ("cancellation_policy", " "),
      ("cancellation_policy", &"ñ".repeat(4001)),
      ("reminder_2h_enabled", "sometimes"),
    ] {
      let fields = changed(changed(schedule_fields(), "grid_minutes", "45"), key, bad);
      let response = save(&app, &session, "schedule", &csrf, &fields).await;
      assert_eq!(response.status(), 422, "{key}: {bad}");
      let html = text(response).await;
      assert!(html.contains(&format!("id=\"{key}-error\"")));
      let saved = page(&app, &session, "schedule").await;
      assert_eq!(field(&saved, "grid_minutes"), "20");
      assert_eq!(field(&saved, "mon_hours"), "09:00-12:00, 14:00-18:00");
      assert!(selected(&saved, "sun_status", "closed"));
      assert!(saved.contains("Política elegida por el consultorio de prueba."));
    }
    let missing = schedule_fields()
      .into_iter()
      .filter(|(key, _)| *key != "fri_status")
      .collect::<Vec<_>>();
    assert_eq!(
      save(&app, &session, "schedule", &csrf, &missing)
        .await
        .status(),
      422
    );
    for hours in ["00:00-12:00, 12:00-24:00", "14:00-18:00, 09:00-12:00"] {
      assert_eq!(
        save(
          &app,
          &session,
          "schedule",
          &csrf,
          &changed(schedule_fields(), "mon_hours", hours)
        )
        .await
        .status(),
        303
      );
    }
    // All closed is an explicit valid decision; no invented working-days requirement.
    let fields = changed(
      changed(
        changed(
          changed(schedule_fields(), "mon_status", "closed"),
          "mon_hours",
          "",
        ),
        "tue_status",
        "closed",
      ),
      "tue_hours",
      "",
    );
    let fields = changed(fields, "minimum_notice_minutes", "2147483647");
    assert_eq!(
      save(&app, &session, "schedule", &csrf, &fields)
        .await
        .status(),
      303
    );
    let saved = page(&app, &session, "schedule").await;
    assert!(selected(&saved, "mon_status", "closed"));
    assert_eq!(field(&saved, "minimum_notice_minutes"), "2147483647");
  })
  .await;
}
