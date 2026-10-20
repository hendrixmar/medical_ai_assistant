use super::*;
use crate::modules::doctor::Clock;
use crate::{
  app::tests::with_database, database::Db, modules::google_calendar::tests::fixture::Fixture,
};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

fn rfc3339(at: i64) -> String {
  chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at)
    .unwrap()
    .to_rfc3339()
}

fn event(id: &str, start: i64, end: i64) -> Value {
  json!({
    "id": id,
    "status": "confirmed",
    "transparency": "opaque",
    "start": {"dateTime": rfc3339(start)},
    "end": {"dateTime": rfc3339(end)}
  })
}

async fn connect(fixture: &Fixture) {
  fixture.connect().await;
  assert_eq!(fixture.select().await.status(), 303);
}

async fn select_blocking(fixture: &Fixture, version: &str, calendars: &[&str]) {
  assert_eq!(
    fixture
      .select_with_blocking(version, calendars)
      .await
      .status(),
    303
  );
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn availability_pages_booking_events_excludes_own_id_and_fails_closed() {
  with_database(|db| async move {
    let fixture = Fixture::new(&db).await;
    connect(&fixture).await;

    let now = fixture.clock.now_millis();
    let start = now + 86_400_000;
    let end = start + 3_600_000;
    let own_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let ext_start = start + 900_000;
    let ext_end = ext_start + 600_000;
    *fixture.provider.pages.lock().await = vec![json!({"items":[
      {"id":"secondary@example.invalid","summary":"Consultorio","accessRole":"owner"},
      {"id":"other@example.invalid","summary":"Otro","accessRole":"freeBusyReader"}
    ]})];
    *fixture.provider.event_pages.lock().await = vec![
      json!({"items":[event(own_id, start + 300_000, start + 600_000)], "nextPageToken":"1"}),
      json!({"items":[event("external-event", ext_start, ext_end)]}),
    ];

    let unselected_busy = availability(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
      start,
      end,
      Some(own_id),
    )
    .await
    .unwrap();
    assert!(unselected_busy.is_empty());
    assert!(!fixture
      .provider
      .calls
      .lock()
      .await
      .iter()
      .any(|(method, uri, _)| method == "GET" && uri.contains("/events")));

    select_blocking(&fixture, "2", &["secondary@example.invalid"]).await;
    let busy = availability(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
      start,
      end,
      Some(own_id),
    )
    .await
    .unwrap();
    assert_eq!(busy, vec![(ext_start, ext_end)]);
    let event_list_calls: Vec<_> = fixture
      .provider
      .calls
      .lock()
      .await
      .iter()
      .filter(|(method, uri, _)| method == "GET" && uri.contains("/events"))
      .cloned()
      .collect();
    assert_eq!(event_list_calls.len(), 2);
    assert!(event_list_calls[1].1.contains("pageToken=1"));
    assert!(event_list_calls
      .iter()
      .all(|(_, uri, _)| !uri.contains("attendees")));

    select_blocking(
      &fixture,
      "3",
      &["secondary@example.invalid", "other@example.invalid"],
    )
    .await;
    *fixture.provider.freebusy_error.lock().await = true;
    assert!(availability(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
      start,
      end,
      Some(own_id),
    )
    .await
    .is_err());

    *fixture.provider.freebusy_error.lock().await = false;
    *fixture.provider.event_pages.lock().await = vec![json!({"items":"malformed"})];
    assert!(availability(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
      start,
      end,
      Some(own_id),
    )
    .await
    .is_err());
  })
  .await;
}

async fn create_video_appointment(db: &Db, now: i64) -> (Uuid, String, i64, i64) {
  let contact = Uuid::now_v7();
  let id = Uuid::now_v7();
  let service = Uuid::now_v7();
  let event_id = id.simple().to_string();
  let starts_at = now + 172_800_000;
  let ends_at = starts_at + 30 * 60_000;
  let inbox_id = format!("synthetic-incoming-{contact}");
  let alias = "5215550000001@c.us";
  let tx = store::transaction(db).await.unwrap();
  tx.execute(store::sql(
    "INSERT INTO contacts(id,created_at) VALUES ($1,$2)",
    vec![contact.into(), now.into()],
  ))
  .await
  .unwrap();
  tx.execute(store::sql(
    "INSERT INTO inbox_messages(message_id,original_chat_identity,server_timestamp,arrived_at,raw_envelope,preparation_state,message_kind,body,prepared_at,contact_id) VALUES ($1,$2,$3,to_timestamp($4::double precision/1000),$5,'ready_for_conversation','text','Hola',to_timestamp($4::double precision/1000),$6)",
    vec![inbox_id.clone().into(), alias.into(), (now / 1000).into(), now.into(), vec![1_u8].into(), contact.into()],
  ))
  .await
  .unwrap();
  tx.execute(store::sql(
    "INSERT INTO contact_aliases(jid,contact_id,inbox_message_id) VALUES ($1,$2,$3)",
    vec![alias.into(), contact.into(), inbox_id.into()],
  ))
  .await
  .unwrap();
  tx.execute(store::sql(
    "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,booking_calendar,google_generation,google_selection_version,event_id,revision,created_at,updated_at) VALUES ($1,$2,'Synthetic Patient',$3,'Consulta general',30,10000,'video',$4,$5,'America/Mexico_City','Consultorio',0,0,'confirmed','verified','pending','secondary@example.invalid',1,1,$6,1,$7,$7)",
    vec![id.into(), contact.into(), service.into(), starts_at.into(), ends_at.into(), event_id.clone().into(), now.into()],
  ))
  .await
  .unwrap();
  tx.execute(store::sql(
    "INSERT INTO appointment_jobs(id,appointment_id,kind,due_at,dedupe_key) VALUES ($1,$2,'calendar_sync',$3,$4)",
    vec![Uuid::now_v7().into(), id.into(), now.into(), job_key(id, 1, "calendar_sync").into()],
  ))
  .await
  .unwrap();
  tx.commit().await.unwrap();
  (id, event_id, starts_at, ends_at)
}

async fn appointment_state(db: &Db, id: Uuid) -> sea_orm::QueryResult {
  db.conn
    .query_one(store::sql(
      "SELECT status,payment,calendar_sync,revision,event_id,event_etag,meet_url,starts_at,ends_at,price_centavos FROM appointments WHERE id=$1",
      vec![id.into()],
    ))
    .await
    .unwrap()
    .unwrap()
}

async fn outbox_count(db: &Db, id: Uuid) -> i64 {
  db.conn
    .query_one(store::sql(
      "SELECT count(*) AS count FROM outbox_messages WHERE appointment_id=$1",
      vec![id.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "count")
    .unwrap()
}
async fn reminder_count(db: &Db, id: Uuid) -> i64 {
  db.conn
    .query_one(store::sql(
      "SELECT count(*) AS count FROM appointment_jobs WHERE appointment_id=$1 AND kind='reminders'",
      vec![id.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "count")
    .unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn video_sync_reconciles_409_polls_meet_patches_in_place_and_cancels() {
  with_database(|db| async move {
    let fixture = Fixture::new(&db).await;
    connect(&fixture).await;
    db.conn
      .execute(store::sql(
        "UPDATE practice SET reminder_2h_enabled=true WHERE id=1",
        vec![],
      ))
      .await
      .unwrap();
    let now = fixture.clock.now_millis();
    let (id, event_id, starts_at, ends_at) = create_video_appointment(&db, now).await;
    *fixture.provider.insert_status.lock().await = 409;
    *fixture.provider.conference.lock().await = "pending".to_owned();

    assert!(run_once(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
    )
    .await
    .unwrap());
    let failed = appointment_state(&db, id).await;
    assert_eq!(failed.try_get::<String>("", "calendar_sync").unwrap(), "failed");
    assert_eq!(failed.try_get::<String>("", "payment").unwrap(), "verified");
    assert_eq!(failed.try_get::<i64>("", "revision").unwrap(), 2);
    assert!(failed.try_get::<Option<String>>("", "meet_url").unwrap().is_none());
    assert!(fixture.provider.event.lock().await.is_none());
    assert_eq!(outbox_count(&db, id).await, 1);

    fixture.clock.advance(3);
    *fixture.provider.insert_status.lock().await = 200;
    assert!(run_once(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
    )
    .await
    .unwrap());
    let event = fixture.provider.event.lock().await.clone().unwrap();
    assert_eq!(event["id"], event_id);
    assert_eq!(event["conferenceData"]["createRequest"]["requestId"], format!("ma-{event_id}"));
    assert_eq!(event["conferenceData"]["createRequest"]["status"]["statusCode"], "pending");
    assert!(event.get("attendees").is_none());
    let pending = appointment_state(&db, id).await;
    assert_eq!(pending.try_get::<String>("", "payment").unwrap(), "verified");
    assert_eq!(pending.try_get::<String>("", "calendar_sync").unwrap(), "failed");
    assert_eq!(pending.try_get::<Option<String>>("", "meet_url").unwrap(), None);
    assert_eq!(reminder_count(&db, id).await, 0);

    fixture.clock.advance(3);
    *fixture.provider.conference.lock().await = "success".to_owned();
    assert!(run_once(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
    )
    .await
    .unwrap());
    let synced = appointment_state(&db, id).await;
    assert_eq!(synced.try_get::<String>("", "status").unwrap(), "confirmed");
    assert_eq!(synced.try_get::<String>("", "payment").unwrap(), "verified");
    assert_eq!(synced.try_get::<String>("", "calendar_sync").unwrap(), "synced");
    assert_eq!(synced.try_get::<i64>("", "revision").unwrap(), 3);
    assert_eq!(
      synced.try_get::<Option<String>>("", "meet_url").unwrap(),
      Some("https://meet.google.com/abcd-ef-ghijk".to_owned())
    );
    assert_eq!(reminder_count(&db, id).await, 2);
    assert_eq!(outbox_count(&db, id).await, 2);
    assert!(!run_once(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
    )
    .await
    .unwrap());
    assert_eq!(outbox_count(&db, id).await, 2);

    let moved_start = starts_at + 3_600_000;
    let moved_end = ends_at + 3_600_000;
    let now = fixture.clock.now_millis();
    db.conn
      .execute(store::sql(
        "UPDATE appointments SET starts_at=$2,ends_at=$3,calendar_sync='pending',revision=4,updated_at=$4 WHERE id=$1",
        vec![id.into(), moved_start.into(), moved_end.into(), now.into()],
      ))
      .await
      .unwrap();
    db.conn
      .execute(store::sql(
        "INSERT INTO appointment_jobs(id,appointment_id,kind,due_at,dedupe_key) VALUES ($1,$2,'calendar_sync',$3,$4)",
        vec![Uuid::now_v7().into(), id.into(), now.into(), job_key(id, 4, "calendar_sync").into()],
      ))
      .await
      .unwrap();
    assert!(run_once(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
    )
    .await
    .unwrap());
    let event = fixture.provider.event.lock().await.clone().unwrap();
    assert_eq!(
      chrono::DateTime::parse_from_rfc3339(event["start"]["dateTime"].as_str().unwrap())
        .unwrap()
        .timestamp_millis(),
      moved_start
    );
    assert_eq!(
      chrono::DateTime::parse_from_rfc3339(event["end"]["dateTime"].as_str().unwrap())
        .unwrap()
        .timestamp_millis(),
      moved_end
    );
    assert_eq!(event["conferenceData"]["entryPoints"][0]["uri"], "https://meet.google.com/abcd-ef-ghijk");
    assert_eq!(outbox_count(&db, id).await, 3);
    let rescheduled = appointment_state(&db, id).await;
    assert_eq!(rescheduled.try_get::<String>("", "event_id").unwrap(), event_id);
    assert_eq!(
      rescheduled.try_get::<Option<String>>("", "meet_url").unwrap(),
      Some("https://meet.google.com/abcd-ef-ghijk".to_owned())
    );
    assert_eq!(rescheduled.try_get::<String>("", "payment").unwrap(), "verified");
    assert_eq!(reminder_count(&db, id).await, 4);

    let calls = fixture.provider.calls.lock().await.clone();
    let patches: Vec<_> = calls.iter().filter(|(method, uri, _)| method == "PATCH" && uri.contains("/events/")).collect();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].2["id"], Value::Null);
    assert_eq!(patches[0].2.get("conferenceData"), None);
    assert!(patches[0].1.contains("conferenceDataVersion=1"));
    let posts: Vec<_> = calls
      .iter()
      .filter(|(method, uri, _)| method == "POST" && uri.contains("/events?"))
      .collect();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].2["id"], posts[1].2["id"]);
    assert!(posts.iter().all(|(_, _, body)| body.get("attendees").is_none()));

    let now = fixture.clock.now_millis();
    db.conn
      .execute(store::sql(
        "UPDATE appointments SET status='cancelled',calendar_sync='pending',revision=6,cancelled_at=$2,updated_at=$2 WHERE id=$1",
        vec![id.into(), now.into()],
      ))
      .await
      .unwrap();
    db.conn
      .execute(store::sql(
        "INSERT INTO appointment_jobs(id,appointment_id,kind,due_at,dedupe_key) VALUES ($1,$2,'calendar_cancel',$3,$4)",
        vec![Uuid::now_v7().into(), id.into(), now.into(), job_key(id, 6, "calendar_cancel").into()],
      ))
      .await
      .unwrap();
    assert!(run_once(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
    )
    .await
    .unwrap());
    assert!(fixture.provider.event.lock().await.is_none());
    let cancelled = appointment_state(&db, id).await;
    assert_eq!(cancelled.try_get::<String>("", "status").unwrap(), "cancelled");
    assert_eq!(cancelled.try_get::<String>("", "payment").unwrap(), "verified");
    assert_eq!(cancelled.try_get::<String>("", "calendar_sync").unwrap(), "synced");
    assert_eq!(cancelled.try_get::<i64>("", "revision").unwrap(), 7);
    assert_eq!(outbox_count(&db, id).await, 4);
    let calls = fixture.provider.calls.lock().await.clone();
    assert_eq!(calls.iter().filter(|(method, uri, _)| method == "DELETE" && uri.contains("/events/")).count(), 1);
    let now = fixture.clock.now_millis();
    db.conn
      .execute(store::sql(
        "INSERT INTO appointment_jobs(id,appointment_id,kind,due_at,dedupe_key) VALUES ($1,$2,'calendar_cancel',$3,$4)",
        vec![
          Uuid::now_v7().into(),
          id.into(),
          now.into(),
          format!("{}:absence-recheck", job_key(id, 7, "calendar_cancel")).into(),
        ],
      ))
      .await
      .unwrap();
    assert!(run_once(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
    )
    .await
    .unwrap());
    assert_eq!(outbox_count(&db, id).await, 4);
    assert_eq!(
      appointment_state(&db, id)
        .await
        .try_get::<i64>("", "revision")
        .unwrap(),
      7
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn external_conflict_creates_attention_without_changing_appointment() {
  with_database(|db| async move {
    let fixture = Fixture::new(&db).await;
    connect(&fixture).await;
    select_blocking(&fixture, "2", &["secondary@example.invalid"]).await;
    let now = fixture.clock.now_millis();
    let (id, _, starts_at, ends_at) = create_video_appointment(&db, now).await;
    db.conn
      .execute(store::sql(
        "UPDATE appointment_jobs SET finished_at=$2 WHERE appointment_id=$1 AND kind='calendar_sync'",
        vec![id.into(), now.into()],
      ))
      .await
      .unwrap();
    db.conn
      .execute(store::sql(
        "INSERT INTO appointment_jobs(id,appointment_id,kind,due_at,dedupe_key) VALUES ($1,$2,'conflict',$3,$4)",
        vec![Uuid::now_v7().into(), id.into(), now.into(), job_key(id, 1, "conflict").into()],
      ))
      .await
      .unwrap();
    *fixture.provider.event_pages.lock().await = vec![json!({
      "items":[event("external-event", starts_at + 60_000, ends_at - 60_000)]
    })];

    let before = appointment_state(&db, id).await;
    let before_state = (
      before.try_get::<String>("", "status").unwrap(),
      before.try_get::<String>("", "payment").unwrap(),
      before.try_get::<String>("", "calendar_sync").unwrap(),
      before.try_get::<i64>("", "revision").unwrap(),
      before.try_get::<String>("", "event_id").unwrap(),
      before.try_get::<i64>("", "starts_at").unwrap(),
      before.try_get::<i64>("", "ends_at").unwrap(),
      before.try_get::<i64>("", "price_centavos").unwrap(),
    );
    assert!(run_once(
      &db,
      fixture.clock.as_ref(),
      &fixture.cfg,
      Some(&fixture.client),
    )
    .await
    .unwrap());
    let after = appointment_state(&db, id).await;
    assert_eq!(
      (
        after.try_get::<String>("", "status").unwrap(),
        after.try_get::<String>("", "payment").unwrap(),
        after.try_get::<String>("", "calendar_sync").unwrap(),
        after.try_get::<i64>("", "revision").unwrap(),
        after.try_get::<String>("", "event_id").unwrap(),
        after.try_get::<i64>("", "starts_at").unwrap(),
        after.try_get::<i64>("", "ends_at").unwrap(),
        after.try_get::<i64>("", "price_centavos").unwrap(),
      ),
      before_state
    );
    let attention = db
      .conn
      .query_one(store::sql(
        "SELECT count(*) AS count FROM attention_items WHERE appointment_id=$1 AND kind='calendar_conflict' AND resolved_at IS NULL",
        vec![id.into()],
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(attention.try_get::<i64>("", "count").unwrap(), 1);
    assert_eq!(outbox_count(&db, id).await, 0);
  })
  .await;
}
