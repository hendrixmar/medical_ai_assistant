use super::*;
use crate::modules::{
  conversation::jobs,
  doctor::{
    tests::{authenticated_get, signed_in, text},
    Clock,
  },
};

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn dashboard_paginates_combined_real_attention_and_reports_read_failures() {
  with_database(|db|async move{
    let (cfg,clock,app,totp)=setup(&db).await;
    for n in 0..27{
      let peer=format!("1555010{n:04}@c.us");let mut input=message(&format!("PAGE{n}"),"5");
      input["payload"]["from"]=json!(peer);input["payload"]["id"]=json!(format!("false_{peer}_PAGE{n}"));
      assert_eq!(deliver(&app,&input).await.status(),200);preparation::run_once(&db,clock.as_ref()).await.unwrap();jobs::run_once(&db, clock.as_ref(), &cfg, None).await.unwrap();
    }
    db.conn.execute_unprepared("UPDATE practice SET timezone='America/Mexico_City'; UPDATE outbox_messages SET state='failed' WHERE sequence=(SELECT min(sequence) FROM outbox_messages)").await.unwrap();
    let session=signed_in(&app,&totp,NOW+30).await;
    let first=text(authenticated_get(&app,"/dashboard",&session).await).await;
    assert_eq!(first.matches("class=\"attention-action\"").count(),25);assert!(first.contains("/dashboard?page=1"));assert!(!first.contains("Envío fallido"));assert!(first.contains("Mensajes en cola: 26"));assert!(first.contains("CST"));
    let second=text(authenticated_get(&app,"/dashboard?page=1",&session).await).await;
    assert_eq!(second.matches("class=\"attention-action\"").count(),2);assert!(second.contains("Envío fallido"));assert!(second.contains("/dashboard?page=0"));assert!(!second.contains("/dashboard?page=2"));
    let beyond_response=authenticated_get(&app,"/dashboard?page=4000000000",&session).await;
    assert_eq!(beyond_response.status(),200);
    let beyond=text(beyond_response).await;
    assert_eq!(beyond.matches("class=\"attention-action\"").count(),0);
    assert!(beyond.contains("/dashboard?page=3999999999"));
    db.conn.execute_unprepared("ALTER TABLE followup_requests RENAME TO unavailable_followups").await.unwrap();
    let response=authenticated_get(&app,"/dashboard",&session).await;assert_eq!(response.status(),503);let failed=text(response).await;assert!(failed.contains("No se pudieron cargar"));assert!(!failed.contains("No hay solicitudes"));
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn dashboard_only_flags_uncertainty_after_its_own_dispatch_window() {
  with_database(|db| async move {
    let (cfg, clock, app, totp) = setup(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    for n in 0..27 {
      assert_eq!(deliver(&app, &message(&format!("VISIBLE{n}"), "hola")).await.status(), 200);
      preparation::run_once(&db, clock.as_ref()).await.unwrap();
      jobs::run_once(&db, clock.as_ref(), &cfg, None).await.unwrap();
    }
    let now = clock.now_millis();
    // Persist realistic dispatch snapshots without an external send; exercise the
    // authenticated dashboard and signed acknowledgement interfaces against PG.
    db.conn.execute_unprepared(&format!("UPDATE outbox_messages SET state='uncertain',raw_provider_id='VISIBLEWIRE'||sequence::text,dispatch_committed_at={now},dispatch_deadline={}; UPDATE contacts SET dispatch_until={}", now+125_000, now+250_000)).await.unwrap();
    let active = text(authenticated_get(&app, "/dashboard", &session).await).await;
    assert!(!active.contains("Envío incierto"), "Normal active sends must not be problem attention");
    assert!(!active.contains("Página siguiente"), "Active sends must not occupy attention pages");

    db.conn.execute_unprepared(&format!("UPDATE outbox_messages SET state='failed' WHERE sequence=1; UPDATE outbox_messages SET ack_error=true WHERE sequence=2; UPDATE outbox_messages SET dispatch_deadline={now} WHERE sequence=3")).await.unwrap();
    let expired = text(authenticated_get(&app, "/dashboard", &session).await).await;
    assert!(expired.contains("Envío fallido"));
    assert!(expired.contains("Error reportado por WhatsApp"));
    assert_eq!(expired.matches("<h3>Envío incierto</h3>").count(), 1);
    assert!(!expired.contains("Página siguiente"));
    // A newer Contact dispatch window must not hide the older uncertain row.
    assert_eq!(count(&db, &format!("contacts WHERE dispatch_until>{now}")).await, 1);

    let ack = json!({"id":"evt-visible-read","event":"message.ack","session":"default","engine":"GOWS","payload":{"id":"true_15550100101@c.us_VISIBLEWIRE3","from":"15550100101@c.us","fromMe":true,"to":null,"participant":null,"ack":3}});
    assert_eq!(deliver(&app, &ack).await.status(), 200);
    let confirmed = text(authenticated_get(&app, "/dashboard", &session).await).await;
    assert!(!confirmed.contains("Envío incierto"));
    assert!(confirmed.contains("Envío fallido"));
    assert!(confirmed.contains("Error reportado por WhatsApp"));

    clock.advance(125);
    let first = text(authenticated_get(&app, "/dashboard", &session).await).await;
    let second = text(authenticated_get(&app, "/dashboard?page=1", &session).await).await;
    assert!(first.contains("Página siguiente"));
    assert_eq!(first.matches("<h3>Envío incierto</h3>").count()+second.matches("<h3>Envío incierto</h3>").count(), 24);
    assert!(!second.contains("Página siguiente"));
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn dashboard_query_cannot_claim_a_resolution_was_saved() {
  with_database(|db| async move {
    let (_, _, app, totp) = setup(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let body = text(authenticated_get(&app, "/dashboard?resolved=true", &session).await).await;
    assert!(!body.contains("El estado de la solicitud quedó guardado"));
    assert_eq!(count(&db, "followup_requests").await, 0);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn missing_origin_preserves_conversation_and_send_job_insert_failure_rolls_back() {
  with_database(|db|async move{
    let (cfg,clock,app,_)=setup(&db).await;
    assert_eq!(deliver(&app,&message("NOPREREQ","5")).await.status(),200);preparation::run_once(&db,clock.as_ref()).await.unwrap();
    drop(app);let mut cfg=cfg;Arc::get_mut(&mut cfg).unwrap().public_origin=None;
    for _ in 0..6{assert_eq!(jobs::run_once(&db,clock.as_ref(),&cfg,None).await.unwrap(),jobs::Outcome::Idle);}
    assert_eq!(count(&db,"processing_jobs WHERE kind='process_conversation' AND status='pending' AND attempts=0").await,1);
    Arc::get_mut(&mut cfg).unwrap().public_origin=Some("https://practice.test".into());
    db.conn.execute_unprepared("CREATE FUNCTION reject_send_job() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='send_outbox' THEN RAISE EXCEPTION 'synthetic fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_send_job BEFORE INSERT ON processing_jobs FOR EACH ROW EXECUTE FUNCTION reject_send_job()").await.unwrap();
    assert!(jobs::run_once(&db,clock.as_ref(),&cfg,None).await.is_err());
    for table in ["contacts","followup_requests","outbox_messages","inbox_messages WHERE processed_at IS NOT NULL"]{assert_eq!(count(&db,table).await,0);}
    db.conn.execute_unprepared("DROP TRIGGER reject_send_job ON processing_jobs").await.unwrap();jobs::run_once(&db, clock.as_ref(), &cfg, None).await.unwrap();
    assert_eq!(count(&db,"outbox_messages").await,1);assert_eq!(count(&db,"processing_jobs WHERE kind='send_outbox'").await,1);
  }).await;
}
