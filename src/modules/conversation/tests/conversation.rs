use super::*;
use crate::modules::{
  conversation::jobs,
  doctor::tests::{authenticated_get, field, get, post, signed_in, text},
};

async fn process(
  db: &crate::database::Db,
  app: &Router,
  clock: &dyn crate::modules::doctor::Clock,
  cfg: &crate::common::config::Config,
  input: &Value,
) {
  assert_eq!(deliver(app, input).await.status(), 200);
  preparation::run_once(db, clock).await.unwrap();
  assert_eq!(
    jobs::run_once(db, clock, cfg, None).await.unwrap(),
    jobs::Outcome::Processed
  );
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn public_privacy_headers_and_protected_idempotent_concurrent_resolution() {
  with_database(|db| async move {
    let (cfg, clock, app, totp) = setup(&db).await;
    let privacy = get(&app, "/aviso-de-privacidad").await;
    assert_eq!(privacy.status(), 200);
    assert_eq!(privacy.headers()["referrer-policy"], "same-origin");
    assert!(privacy.headers()["content-security-policy"]
      .to_str()
      .unwrap()
      .contains("script-src 'none'"));
    let body = text(privacy).await;
    assert!(body.contains("PROVISIONAL"));
    assert!(body.contains("ARCO"));
    assert!(!body.contains("action=\"/logout\""));
    process(&db, &app, clock.as_ref(), &cfg, &message("RESOLVE", "5")).await;
    let id: uuid::Uuid = db
      .conn
      .query_one(sql("SELECT id FROM followup_requests"))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "id")
      .unwrap();
    let path = format!("/followups/{id}/resolve");
    assert_eq!(
      post(&app, &path, None, Some("https://practice.test"), &[])
        .await
        .status(),
      401
    );
    let session = signed_in(&app, &totp, NOW + 30).await;
    let html = text(authenticated_get(&app, "/dashboard", &session).await).await;
    assert!(html.contains(&path));
    assert!(html.contains("+15550100101"));
    assert!(html.contains("UTC"));
    let csrf = field(&html, "csrf_token");
    assert_eq!(
      post(
        &app,
        &path,
        Some(&session),
        Some("https://evil.test"),
        &[("csrf_token", &csrf)]
      )
      .await
      .status(),
      403
    );
    assert_eq!(
      post(
        &app,
        &path,
        Some(&session),
        Some("https://practice.test"),
        &[("csrf_token", "bad")]
      )
      .await
      .status(),
      403
    );
    let fields = [("csrf_token", csrf.as_str())];
    let (a, b) = tokio::join!(
      post(
        &app,
        &path,
        Some(&session),
        Some("https://practice.test"),
        &fields
      ),
      post(
        &app,
        &path,
        Some(&session),
        Some("https://practice.test"),
        &fields
      )
    );
    assert_eq!(a.status(), 303);
    assert_eq!(b.status(), 303);
    assert_eq!(
      count(&db, "followup_requests WHERE resolved_at IS NOT NULL").await,
      1
    );
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE resolved_followup_id IS NOT NULL"
      )
      .await,
      1
    );
    assert_eq!(count(&db, "inbox_messages").await, 1);
    assert_eq!(
      count(
        &db,
        "processing_jobs WHERE kind='send_outbox' AND inbox_message_id IS NULL"
      )
      .await,
      2
    );
    assert!(!text(authenticated_get(&app, "/dashboard", &session).await)
      .await
      .contains("action=\"/followups/"));
    process(&db, &app, clock.as_ref(), &cfg, &message("REOPEN", "5")).await;
    assert_eq!(
      count(&db, "followup_requests WHERE resolved_at IS NULL").await,
      1
    );
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn media_captions_do_not_execute_commands_and_noise_does_not_queue_replies() {
  with_database(|db| async move {
    let (cfg, clock, app, _) = setup(&db).await;
    let cases = [
      (json!({"audioMessage":{"PTT":true}}), true),
      (json!({"audioMessage":{}}), false),
      (json!({"documentMessage":{"mimetype":"audio/ogg"}}), true),
      (json!({"imageMessage":{"caption":"5"}}), true),
      (json!({"videoMessage":{"caption":"5"}}), true),
      (
        json!({"documentMessage":{"mimetype":"application/pdf"}}),
        true,
      ),
      (json!({"audioMessage":{},"imageMessage":{}}), true),
      (
        json!({"unknownMessage":{"quotedMessage":{"audioMessage":{}}}}),
        false,
      ),
    ];
    for (n, (raw, has_media)) in cases.into_iter().enumerate() {
      let mut input = message(&format!("MEDIA{n}"), "5");
      input["payload"]["hasMedia"] = json!(has_media);
      input["payload"]["_data"]["Message"] = raw;
      process(&db, &app, clock.as_ref(), &cfg, &input).await;
    }
    assert_eq!(
      count(&db, "followup_requests").await,
      0,
      "Media captions are not the followup command"
    );
    assert_eq!(count(&db, "appointments").await, 0);
    let queued = count(&db, "outbox_messages").await;
    for (n, raw) in [
      json!({"stickerMessage":{}}),
      json!({"lottieStickerMessage":{}}),
      json!({"audioStickerMessage":{}}),
      json!({"reactionMessage":{}}),
    ]
    .into_iter()
    .enumerate()
    {
      let mut input = message(&format!("IGNORE{n}"), "5");
      input["payload"]["_data"]["Message"] = raw;
      process(&db, &app, clock.as_ref(), &cfg, &input).await;
    }
    assert_eq!(
      count(&db, "outbox_messages").await,
      queued,
      "Stickers and reactions do not queue replies"
    );
    assert_eq!(count(&db, "followup_requests").await, 0);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn conversation_and_resolve_roll_back_all_logical_effects_on_outbox_failure() {
  with_database(|db|async move{
    let (cfg,clock,app,totp)=setup(&db).await;
    assert_eq!(deliver(&app,&message("ATOMIC","5")).await.status(),200);
    preparation::run_once(&db,clock.as_ref()).await.unwrap();
    db.conn.execute_unprepared("CREATE FUNCTION fail_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic fault'; END $$; CREATE TRIGGER fail_outbox BEFORE INSERT ON outbox_messages FOR EACH ROW EXECUTE FUNCTION fail_outbox();").await.unwrap();
    assert!(jobs::run_once(&db,clock.as_ref(),&cfg,None).await.is_err());
    for table in ["contacts","contact_aliases","followup_requests","outbox_messages","inbox_messages WHERE processed_at IS NOT NULL"]{assert_eq!(count(&db,table).await,0);}
    assert_eq!(count(&db,"processing_jobs WHERE kind='process_conversation' AND status='pending'").await,1);
    db.conn.execute_unprepared("DROP TRIGGER fail_outbox ON outbox_messages").await.unwrap();
    jobs::run_once(&db, clock.as_ref(), &cfg, None).await.unwrap();
    let session=signed_in(&app,&totp,NOW+30).await;
    let csrf=field(&text(authenticated_get(&app,"/dashboard",&session).await).await,"csrf_token");
    let id:uuid::Uuid=db.conn.query_one(sql("SELECT id FROM followup_requests")).await.unwrap().unwrap().try_get("","id").unwrap();
    db.conn.execute_unprepared("CREATE TRIGGER fail_outbox BEFORE INSERT ON outbox_messages FOR EACH ROW EXECUTE FUNCTION fail_outbox()").await.unwrap();
    assert_eq!(post(&app,&format!("/followups/{id}/resolve"),Some(&session),Some("https://practice.test"),&[("csrf_token",&csrf)]).await.status(),503);
    assert_eq!(count(&db,"followup_requests WHERE resolved_at IS NULL").await,1);assert_eq!(count(&db,"outbox_messages").await,1);
    db.conn.execute_unprepared("DROP TRIGGER fail_outbox ON outbox_messages").await.unwrap();
    assert_eq!(post(&app,&format!("/followups/{id}/resolve"),Some(&session),Some("https://practice.test"),&[("csrf_token",&csrf)]).await.status(),303);
    assert_eq!(count(&db,"outbox_messages").await,2);
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn alias_conflict_never_merges_histories_and_parallel_aliases_share_one_open_followup() {
  crate::app::tests::with_database_connections(4, |db| async move {
    let (cfg, clock, app, _) = setup(&db).await;
    let mut lid = message("LID", "5");
    lid["payload"]["from"] = json!("99001@lid");
    lid["payload"]["id"] = json!("false_99001@lid_LID");
    lid["payload"]["_data"]["Info"] = json!({"SenderAlt":"15550100101@s.whatsapp.net"});
    let phone = message("PHONE", "5");
    let (a, b) = tokio::join!(deliver(&app, &lid), deliver(&app, &phone));
    assert_eq!(a.status(), 200);
    assert_eq!(b.status(), 200);
    preparation::run_once(&db, clock.as_ref()).await.unwrap();
    preparation::run_once(&db, clock.as_ref()).await.unwrap();
    let (a, b) = tokio::join!(
      jobs::run_once(&db, clock.as_ref(), &cfg, None),
      jobs::run_once(&db, clock.as_ref(), &cfg, None)
    );
    a.unwrap();
    b.unwrap();
    jobs::run_once(&db, clock.as_ref(), &cfg, None)
      .await
      .unwrap();
    assert_eq!(count(&db, "contacts").await, 1);
    assert_eq!(count(&db, "contact_aliases").await, 2);
    assert_eq!(
      count(&db, "followup_requests WHERE resolved_at IS NULL").await,
      1
    );
    let mut other = message("OTHER", "5");
    other["payload"]["from"] = json!("99002@lid");
    other["payload"]["id"] = json!("false_99002@lid_OTHER");
    process(&db, &app, clock.as_ref(), &cfg, &other).await;
    assert_eq!(count(&db, "contacts").await, 2);
    other["payload"]["id"] = json!("false_99002@lid_CONFLICT");
    other["payload"]["_data"]["Info"] = json!({"SenderAlt":"15550100101@s.whatsapp.net"});
    process(&db, &app, clock.as_ref(), &cfg, &other).await;
    assert_eq!(count(&db, "contacts WHERE identity_conflict").await, 2);
    assert_eq!(count(&db, "contacts").await, 2);
    assert_eq!(count(&db, "contact_aliases").await, 3);
    assert_eq!(
      count(&db, "followup_requests WHERE resolved_at IS NULL").await,
      2
    );
    assert_eq!(get(&app, "/dashboard").await.status(), 303);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn crossed_existing_contacts_serialize_intake_without_merging_or_provider_calls() {
  use crate::modules::{conversation::sender, whatsapp::client::Client};
  use sea_orm::TransactionTrait;
  use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
  };

  crate::app::tests::with_database_connections(4, |db| async move {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let provider = Router::new().fallback(move || {
      observed.fetch_add(1, Ordering::SeqCst);
      async { axum::http::StatusCode::SERVICE_UNAVAILABLE }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let provider_task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let (mut cfg, clock, app, _) = setup(&db).await;
    drop(app);
    let config = Arc::get_mut(&mut cfg).unwrap();
    config.waha = WahaConfig::parse(Some(&base), Some("synthetic-api-key"), Some(KEY)).unwrap();
    config.waha_outbound_enabled = true;
    let (app, client, _) = crate::app::compose(cfg.clone(), db.clone(), clock.clone());
    let client: Client = client.unwrap();

    let mapped = |id: &str, lid: &str, phone: &str| {
      let mut input = message(id, "5");
      input["payload"]["from"] = json!(lid);
      input["payload"]["id"] = json!(format!("false_{lid}_{id}"));
      input["payload"]["_data"]["Info"] = json!({"SenderAlt":phone});
      input
    };
    for input in [
      mapped("CONTACTA", "99001@lid", "15550100101@s.whatsapp.net"),
      mapped("CONTACTB", "99002@lid", "15550100222@s.whatsapp.net"),
    ] {
      process(&db, &app, clock.as_ref(), &cfg, &input).await;
    }
    assert_eq!(count(&db, "contacts").await, 2);
    assert_eq!(count(&db, "contact_aliases").await, 4);
    let history_sql = "SELECT json_build_object(
      'aliases', (SELECT json_agg(a ORDER BY jid) FROM contact_aliases a),
      'followups', (SELECT json_agg(f ORDER BY id) FROM followup_requests f),
      'inbox', (SELECT json_agg(i ORDER BY message_id) FROM inbox_messages i WHERE message_id IN ('false_99001@lid_CONTACTA','false_99002@lid_CONTACTB')),
      'outbox', (SELECT json_agg(o ORDER BY sequence) FROM outbox_messages o WHERE reply_inbox_id IN ('false_99001@lid_CONTACTA','false_99002@lid_CONTACTB'))
    )::text AS history";
    let history: String = db.conn.query_one(sql(history_sql)).await.unwrap().unwrap().try_get("", "history").unwrap();
    let queued_before_conflict = count(&db, "outbox_messages").await;
    for input in [
      mapped("CROSSA", "99001@lid", "15550100222@s.whatsapp.net"),
      mapped("CROSSB", "99002@lid", "15550100101@s.whatsapp.net"),
    ] {
      assert_eq!(deliver(&app, &input).await.status(), 200);
      preparation::run_once(&db, clock.as_ref()).await.unwrap();
    }
    db.conn.execute_unprepared("CREATE FUNCTION gate_contact_conflict() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.identity_conflict AND NOT OLD.identity_conflict THEN PERFORM pg_advisory_xact_lock(hashtext(current_schema()),17); END IF; RETURN NEW; END $$; CREATE TRIGGER gate_contact_conflict BEFORE UPDATE ON contacts FOR EACH ROW EXECUTE FUNCTION gate_contact_conflict()").await.unwrap();
    let gate = db.conn.begin().await.unwrap();
    gate.execute(sql("SELECT pg_advisory_xact_lock(hashtext(current_schema()),17)")).await.unwrap();
    let (worker_db, worker_clock, worker_cfg) = (db.clone(), clock.clone(), cfg.clone());
    let first = tokio::spawn(async move {
      jobs::run_once(&worker_db, worker_clock.as_ref(), &worker_cfg, None).await
    });
    // Tie the advisory waiter to this schema's actual Contact-locking transaction.
    let waiting_sql = "SELECT EXISTS (
      SELECT FROM pg_locks gate JOIN pg_locks contact ON contact.pid=gate.pid
      WHERE gate.locktype='advisory' AND NOT gate.granted AND gate.objid=17
        AND gate.classid::bigint=(hashtext(current_schema())::bigint & 4294967295)
        AND contact.locktype='relation' AND contact.relation='contacts'::regclass
        AND contact.mode='RowShareLock' AND contact.granted
    ) AS waiting";
    tokio::time::timeout(Duration::from_secs(2), async {
      loop {
        let row = db.conn.query_one(sql(waiting_sql)).await.unwrap().unwrap();
        if row.try_get::<bool>("", "waiting").unwrap() { break; }
        tokio::task::yield_now().await;
      }
    }).await.expect("Conflict update must reach the schema-local Contact gate");
    // Different incoming pairs still share existing Contacts: production ordering
    // keeps B out of identity assignment rather than making it wait on A's rows.
    assert_eq!(tokio::time::timeout(Duration::from_secs(2), jobs::run_once(&db, clock.as_ref(), &cfg, None)).await.expect("Crossed intake must serialize without waiting on Contact rows").unwrap(), jobs::Outcome::Idle);
    assert!(!first.is_finished());
    assert!(db.conn.query_one(sql(waiting_sql)).await.unwrap().unwrap().try_get::<bool>("", "waiting").unwrap());
    assert_eq!(count(&db, "processing_jobs WHERE kind='process_conversation' AND status='pending'").await, 2);
    assert_eq!(count(&db, "contacts WHERE identity_conflict").await, 0);
    gate.commit().await.unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(2), first).await.unwrap().unwrap().unwrap(), jobs::Outcome::Processed);
    assert_eq!(jobs::run_once(&db, clock.as_ref(), &cfg, None).await.unwrap(), jobs::Outcome::Processed);
    db.conn.execute_unprepared("DROP TRIGGER gate_contact_conflict ON contacts; DROP FUNCTION gate_contact_conflict()").await.unwrap();

    assert_eq!(count(&db, "contacts").await, 2);
    assert_eq!(count(&db, "contacts WHERE identity_conflict").await, 2);
    assert_eq!(db.conn.query_one(sql(history_sql)).await.unwrap().unwrap().try_get::<String>("", "history").unwrap(), history);
    assert_eq!(count(&db, "inbox_messages WHERE processed_at IS NOT NULL").await, 4);
    assert_eq!(count(&db, "inbox_messages i JOIN contact_aliases a ON a.jid=i.original_chat_identity WHERE i.contact_id=a.contact_id").await, 4);
    assert_eq!(count(&db, "outbox_messages o LEFT JOIN contact_aliases a ON a.jid=o.original_target WHERE o.contact_id IS DISTINCT FROM a.contact_id").await, 0);
    assert_eq!(count(&db, "outbox_messages").await, queued_before_conflict, "Conflicted identity cannot authorize a new reply");
    assert_eq!(count(&db, "followup_requests WHERE resolved_at IS NULL").await, 2);
    for _ in 0..2 {
      assert_eq!(sender::run_once(&db, clock.as_ref(), &cfg, Some(&client)).await.unwrap(), sender::Outcome::Idle);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(count(&db, "outbox_messages WHERE state='queued' AND raw_provider_id IS NULL AND dispatch_committed_at IS NULL").await, queued_before_conflict);
    provider_task.abort();
    assert!(provider_task.await.unwrap_err().is_cancelled());
  }).await;
}

#[test]
fn outbound_policy_defaults_off_and_validates_positive_bounded_cap() {
  use crate::common::config::parse_outbound_policy;
  assert_eq!(parse_outbound_policy(None, None).unwrap(), (false, 20));
  assert_eq!(
    parse_outbound_policy(Some("true"), Some("1")).unwrap(),
    (true, 1)
  );
  for value in ["0", "-1", "1001", "", "twenty"] {
    assert!(parse_outbound_policy(None, Some(value)).is_err());
  }
  for value in ["", "yes", "1"] {
    assert!(parse_outbound_policy(Some(value), None).is_err());
  }
}
