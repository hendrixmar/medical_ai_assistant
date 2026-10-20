use super::*;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn ten_digit_phone_starting_52_is_not_a_mexican_country_code_alias() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(1);
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("BOUND", "525255123456@c.us", &code, clock.now_millis()),
    )
    .await;
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("SHORT", "5255123456@c.us", "5", clock.now_millis()),
    )
    .await;
    assert_eq!(count(&db, "followup_requests").await, 1);
    assert_eq!(
      count(&db, "outbox_messages WHERE template='doctor_read_only'").await,
      0
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn only_current_resolved_sender_and_matching_numeric_me_can_redeem() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(1);
    for (i, me) in [
      Value::Null,
      json!({"id":"not-a-phone"}),
      json!({"id":"99001@lid"}),
      json!({"id":"525555555556@c.us"}),
    ]
    .into_iter()
    .enumerate()
    {
      let mut input = message(
        &format!("ME{i}"),
        "525512345678@c.us",
        &code,
        clock.now_millis(),
      );
      input["me"] = me;
      process(&db, &app, clock.as_ref(), &cfg, &input).await;
    }
    for (i, sender) in ["525555555555@c.us", "5215555555555@c.us", "99001@lid"]
      .iter()
      .enumerate()
    {
      process(
        &db,
        &app,
        clock.as_ref(),
        &cfg,
        &message(&format!("SELF{i}"), sender, &code, clock.now_millis()),
      )
      .await;
    }
    // A historically mapped LID has no current phone proof on the next message.
    let mut mapped = message("MAP", "99002@lid", "Hola", clock.now_millis());
    mapped["payload"]["_data"]["Info"] = json!({"SenderAlt":"525512345678@s.whatsapp.net"});
    process(&db, &app, clock.as_ref(), &cfg, &mapped).await;
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("HISTORY", "99002@lid", &code, clock.now_millis()),
    )
    .await;
    // Valid raw text is required: neither captions nor conflicting projected body prove it.
    for (i, raw) in [
      json!({"audioMessage":{}}),
      json!({"imageMessage":{"caption":code}}),
      json!({"documentMessage":{"mimetype":"application/pdf"}}),
      json!({"conversation":"not the code"}),
    ]
    .into_iter()
    .enumerate()
    {
      let mut input = message(
        &format!("MEDIA{i}"),
        "525512345678@c.us",
        &code,
        clock.now_millis(),
      );
      input["payload"]["_data"]["Message"] = raw;
      process(&db, &app, clock.as_ref(), &cfg, &input).await;
    }
    for (i, id) in [
      "arbitrary-id",
      "true_525512345678@c.us_FROMME",
      "false_525500000000@c.us_UNTRUSTED",
    ]
    .iter()
    .enumerate()
    {
      let mut input = message(
        &format!("ID{i}"),
        "525512345678@c.us",
        &code,
        clock.now_millis(),
      );
      input["payload"]["id"] = json!(id);
      process(&db, &app, clock.as_ref(), &cfg, &input).await;
    }
    assert_eq!(
      count(&db, "doctor_number_state WHERE verified_phone IS NOT NULL").await,
      0
    );
    assert_eq!(
      count(
        &db,
        "doctor_number_challenges WHERE state='active' AND failures=0"
      )
      .await,
      1
    );
    let mut good = message(
      "GOOD",
      "99002@lid",
      &format!(" {code}\n"),
      clock.now_millis(),
    );
    good["payload"]["_data"]["Info"] = json!({"SenderAlt":"525512345678@s.whatsapp.net"});
    good["me"] = json!({"id":"5215555555555@s.whatsapp.net"});
    process(&db, &app, clock.as_ref(), &cfg, &good).await;
    assert!(text(authenticated_get(&app, PATH, &session).await)
      .await
      .contains("+525512345678"));
    let proof = db
      .conn
      .query_one(sql(
        "SELECT original_target,practice_phone FROM doctor_number_state",
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      proof.try_get::<String>("", "original_target").unwrap(),
      "99002@lid"
    );
    assert_eq!(
      proof.try_get::<String>("", "practice_phone").unwrap(),
      "5215555555555@c.us"
    );
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("NOALT", "99002@lid", "5", clock.now_millis()),
    )
    .await;
    assert_eq!(
      count(&db, "followup_requests WHERE resolved_at IS NULL").await,
      1
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn identity_conflict_cannot_bind_or_recognize_and_exact_mexican_near_misses_stay_ordinary() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(1);
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("BOUND", "525512345678@c.us", &code, clock.now_millis()),
    )
    .await;
    for (i, sender) in [
      "5215512345679@c.us",
      "525512345679@c.us",
      "155512345678@c.us",
      "5255512345678@c.us",
      "52155123456780@c.us",
    ]
    .iter()
    .enumerate()
    {
      process(
        &db,
        &app,
        clock.as_ref(),
        &cfg,
        &message(&format!("NEAR{i}"), sender, "5", clock.now_millis()),
      )
      .await;
    }
    assert_eq!(count(&db, "followup_requests").await, 5);
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("NEWLID", "99999@lid", "Hola", clock.now_millis()),
    )
    .await;
    clock.advance(60_000);
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let next = active_code(&app, &session).await;
    clock.advance(1);
    let followups_before_conflict = count(&db, "followup_requests").await;
    let mut conflict = message("CONFLICT", "99999@lid", &next, clock.now_millis());
    conflict["payload"]["_data"]["Info"] = json!({"SenderAlt":"525512345678@s.whatsapp.net"});
    process(&db, &app, clock.as_ref(), &cfg, &conflict).await;
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("DENY", "525512345678@c.us", "5", clock.now_millis()),
    )
    .await;
    assert_eq!(count(&db, "contacts WHERE identity_conflict").await, 2);
    assert_eq!(
      count(
        &db,
        "doctor_number_challenges WHERE state='active' AND failures=0"
      )
      .await,
      1
    );
    assert_eq!(
      count(&db, "followup_requests").await,
      followups_before_conflict,
      "Identity conflict cannot authorize a new Contact action"
    );
    assert_eq!(
      count(&db, "outbox_messages WHERE template='doctor_verified'").await,
      1
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn millisecond_order_provider_seconds_and_duplicate_immutability_prevent_preissuance_redemption(
) {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    // Code history fixture lets an earlier raw arrival contain the exact next challenge value.
    let early = message("EARLY", "525512345678@c.us", "000007", clock.now_millis());
    deliver(&app, &early).await;
    clock.advance(1);
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    super::lifecycle::set_active_code(&db, &cfg, "000007").await;
    crate::modules::whatsapp::jobs::run_once(&db, clock.as_ref())
      .await
      .unwrap();
    crate::modules::conversation::jobs::run_once(&db, clock.as_ref(), &cfg, None)
      .await
      .unwrap();
    let same = message("SAME", "525512345678@c.us", "000007", clock.now_millis());
    process(&db, &app, clock.as_ref(), &cfg, &same).await;
    clock.advance(1);
    let mut old = message(
      "OLDSECONDS",
      "525512345678@c.us",
      "000007",
      clock.now_millis(),
    );
    old["payload"]["timestamp"] = json!(clock.now() - 1);
    process(&db, &app, clock.as_ref(), &cfg, &old).await;
    deliver(&app, &early).await;
    deliver(&app, &same).await;
    assert_eq!(
      count(&db, "doctor_number_state WHERE verified_phone IS NOT NULL").await,
      0
    );
    assert_eq!(
      count(
        &db,
        "doctor_number_challenges WHERE failures=0 AND state='active'"
      )
      .await,
      1
    );
    let arrival = db
      .conn
      .query_one(sql(
        "SELECT arrived_at FROM inbox_messages WHERE message_id='false_525512345678@c.us_EARLY'",
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get::<chrono::DateTime<chrono::Utc>>("", "arrived_at")
      .unwrap();
    assert_eq!(arrival.timestamp_millis(), (NOW + 30) * 1000 + 123);
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("FRESH", "525512345678@c.us", "000007", clock.now_millis()),
    )
    .await;
    assert_eq!(
      count(&db, "outbox_messages WHERE template='doctor_verified'").await,
      1
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn active_replacement_cannot_redeem_without_key_but_recognition_and_intake_continue() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (mut cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let first = active_code(&app, &session).await;
    clock.advance(1);
    process(&db, &app, clock.as_ref(), &cfg, &message("ORIGINALDOCTOR", "525512345678@c.us", &first, clock.now_millis())).await;
    clock.advance(60_000);
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let replacement = active_code(&app, &session).await;
    assert_ne!(replacement, first);
    let snapshot_sql = "SELECT json_build_object('binding',(SELECT row_to_json(s) FROM doctor_number_state s),'challenges',(SELECT json_agg(c ORDER BY issued_at,id) FROM doctor_number_challenges c))::text AS snapshot";
    let before: String = db.conn.query_one(sql(snapshot_sql)).await.unwrap().unwrap().try_get("", "snapshot").unwrap();
    let key = cfg.doctor_totp_key;
    drop(app);
    Arc::get_mut(&mut cfg).unwrap().doctor_totp_key = None;
    let app = crate::app::router_with_clock(cfg.clone(), db.clone(), clock.clone());
    clock.advance(1);
    let candidate = message("MISSINGKEYCANDIDATE", "525598765432@c.us", &replacement, clock.now_millis());
    process(&db, &app, clock.as_ref(), &cfg, &candidate).await;
    deliver(&app, &candidate).await;
    process(&db, &app, clock.as_ref(), &cfg, &message("KNOWNDOCTOR", "525512345678@c.us", "5", clock.now_millis())).await;
    process(&db, &app, clock.as_ref(), &cfg, &message("ORDINARYCONTACT", "525598765433@c.us", "5", clock.now_millis())).await;
    assert_eq!(db.conn.query_one(sql(snapshot_sql)).await.unwrap().unwrap().try_get::<String>("", "snapshot").unwrap(), before);
    assert_eq!(count(&db, "doctor_number_challenges WHERE state='active' AND failures=0 AND ciphertext IS NOT NULL").await, 1);
    assert_eq!(count(&db, "outbox_messages WHERE template='doctor_verified'").await, 1);
    assert_eq!(count(&db, "outbox_messages WHERE template='doctor_read_only'").await, 1);
    assert_eq!(count(&db, "outbox_messages WHERE reply_inbox_id='false_525598765432@c.us_MISSINGKEYCANDIDATE' AND template='menu'").await, 1);
    assert_eq!(count(&db, "followup_requests").await, 1);
    assert!(p.wire.outgoing.lock().unwrap().is_empty());

    // Restoring the same key preserves the active challenge; only a fresh, valid
    // message may now redeem it, not the already-processed no-key input.
    drop(app);
    Arc::get_mut(&mut cfg).unwrap().doctor_totp_key = key;
    let app = crate::app::router_with_clock(cfg.clone(), db.clone(), clock.clone());
    assert_eq!(active_code(&app, &session).await, replacement);
    deliver(&app, &candidate).await;
    assert_eq!(count(&db, "outbox_messages WHERE template='doctor_verified'").await, 1);
    clock.advance(1);
    process(&db, &app, clock.as_ref(), &cfg, &message("RESTOREDKEYCANDIDATE", "525598765432@c.us", &replacement, clock.now_millis())).await;
    assert_eq!(count(&db, "doctor_number_state WHERE verified_phone='525598765432@c.us'").await, 1);
    assert_eq!(count(&db, "outbox_messages WHERE template='doctor_verified'").await, 2);
    assert_eq!(count(&db, "doctor_number_challenges WHERE state='active'").await, 0);
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn recognized_media_is_read_only_and_missing_crypto_key_does_not_break_ordinary_intake() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(1);
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("VERIFY", "525512345678@c.us", &code, clock.now_millis()),
    )
    .await;
    for (i, raw) in [
      json!({"audioMessage":{}}),
      json!({"imageMessage":{"caption":"5"}}),
      json!({"stickerMessage":{}}),
    ]
    .into_iter()
    .enumerate()
    {
      let mut input = message(
        &format!("MEDIA{i}"),
        "525512345678@c.us",
        "5",
        clock.now_millis(),
      );
      input["payload"]["_data"]["Message"] = raw;
      process(&db, &app, clock.as_ref(), &cfg, &input).await;
    }
    assert_eq!(
      count(&db, "outbox_messages WHERE template='doctor_audio'").await,
      1
    );
    assert_eq!(
      count(&db, "outbox_messages WHERE template='doctor_read_only'").await,
      1
    );
    assert_eq!(count(&db, "followup_requests").await, 0);
    drop(app);
    let mut cfg = cfg;
    Arc::get_mut(&mut cfg).unwrap().doctor_totp_key = None;
    let app = crate::app::router_with_clock(cfg.clone(), db.clone(), clock.clone());
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message("NOKEYDOC", "525512345678@c.us", "5", clock.now_millis()),
    )
    .await;
    process(
      &db,
      &app,
      clock.as_ref(),
      &cfg,
      &message(
        "NOKEYORDINARY",
        "525512345679@c.us",
        "5",
        clock.now_millis(),
      ),
    )
    .await;
    assert_eq!(
      count(&db, "outbox_messages WHERE template='doctor_read_only'").await,
      2
    );
    assert_eq!(count(&db, "followup_requests").await, 1);
  })
  .await;
}
