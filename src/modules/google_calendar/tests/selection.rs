use super::{fixture::Fixture, *};
use serde_json::json;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn concurrent_selection_saves_have_one_winner_and_never_inherit_capability() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    let (a, b) = tokio::join!(f.select(), f.select());
    let mut statuses = vec![a.status().as_u16(), b.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, vec![303, 409]);
    assert_eq!(
      f.row(&db)
        .await
        .try_get::<i64>("", "selection_version")
        .unwrap(),
      2
    );
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
async fn full_fresh_pages_owner_manager_selection_and_explicit_zero_blocking() {
  with_database(|db|async move {
    let f=Fixture::new(&db).await;f.connect().await;
    *f.provider.pages.lock().await=vec![json!({"items":[{"id":"writer@example.invalid","summary":"Escritura","accessRole":"writerWithoutPrivateAccess"}],"nextPageToken":"1"}),json!({"items":[{"id":"secondary@example.invalid","summary":"Secundario <seguro>","accessRole":"owner","hidden":true}]})];
    let response=authenticated_get(&f.app,"/settings/google",&f.session).await;
    assert_eq!(response.status(),200);let body=text(response).await;
    assert!(body.contains("Secundario &lt;seguro&gt;"));
    assert!(!body.contains("propietario verificado"));
    assert_eq!(f.select().await.status(),303);
    let row=f.row(&db).await;
    assert_eq!(row.try_get::<String>("","booking_calendar").unwrap(),"secondary@example.invalid");
    assert_eq!(row.try_get::<serde_json::Value>("","blocking_calendars").unwrap(),json!([]));
    assert_eq!(row.try_get::<i64>("","selection_version").unwrap(),2);
    assert_eq!(f.select().await.status(),409,"stale form cannot mutate");
    assert!(!f.provider.calls.lock().await.iter().any(|(_,p,_)|p.contains("/events") || p=="/freeBusy"));
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn blocking_chunks_are_explicit_and_per_item_errors_or_duplicate_forms_do_not_save() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    let mut entries =
      vec![json!({"id":"secondary@example.invalid","summary":"Consultorio","accessRole":"owner"})];
    let ids: Vec<_> = (0..100)
      .map(|n| {
        format!(
          "blocking-{n}-{}@group.calendar.google.invalid",
          "x".repeat(70)
        )
      })
      .collect();
    entries.extend(
      ids
        .iter()
        .map(|id| json!({"id":id,"summary":id,"accessRole":"freeBusyReader"})),
    );
    *f.provider.pages.lock().await = vec![json!({"items":entries})];
    let mut form = vec![
      ("generation", "1"),
      ("selection_version", "1"),
      ("booking_calendar", "secondary@example.invalid"),
    ];
    form.extend(ids.iter().map(|id| ("blocking_calendar", id.as_str())));
    *f.provider.freebusy_error.lock().await = true;
    assert_eq!(
      f.post("/settings/google/calendars", &form).await.status(),
      503
    );
    assert_eq!(
      f.row(&db)
        .await
        .try_get::<i64>("", "selection_version")
        .unwrap(),
      1
    );
    *f.provider.freebusy_error.lock().await = false;
    assert_eq!(
      f.post("/settings/google/calendars", &form).await.status(),
      303
    );
    let calls = f.provider.calls.lock().await;
    let busy: Vec<_> = calls.iter().filter(|(_, p, _)| p == "/freeBusy").collect();
    assert_eq!(busy.len(), 3);
    assert!(busy
      .iter()
      .all(|(_, _, b)| b["items"].as_array().unwrap().len() <= 50));
    drop(calls);
    let duplicate = f
      .post(
        "/settings/google/calendars",
        &[
          ("generation", "1"),
          ("generation", "1"),
          ("selection_version", "2"),
          ("booking_calendar", "secondary@example.invalid"),
        ],
      )
      .await;
    assert_eq!(duplicate.status(), 409);
  })
  .await;
}
