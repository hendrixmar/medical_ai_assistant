use super::*;
use crate::modules::conversation::acknowledgements;

fn ack(event: &str, peer: &str, raw: &str, value: i32) -> Value {
  json!({"id":event,"event":"message.ack","session":"default","engine":"GOWS","payload":{"id":format!("true_{peer}_{raw}"),"from":peer,"to":null,"participant":null,"fromMe":true,"ack":value}})
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn early_read_stale_device_error_and_late_http_preserve_strongest_evidence() {
  crate::app::tests::with_database_connections(4,|db|async move{
    let p=Provider::new().await;let (cfg,clock,app,client,_)=outgoing(&db,&p,true).await;
    enqueue(&db,&app,clock.as_ref(),&cfg,"EARLY").await;
    sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap();
    assert_eq!(deliver(&app,&ack("early","15550100101@c.us","OUT0",3)).await.status(),200);
    assert_eq!(count(&db,"outgoing_ack_evidence WHERE matched_outbox_id IS NULL").await,1);
    assert_eq!(count(&db,"contacts WHERE privacy_first_sent_at IS NOT NULL").await,0);
    clock.advance(6);p.wire.send_mode.store(2,Ordering::SeqCst);
    let worker=tokio::spawn({let db=db.clone();let clock=clock.clone();let cfg=cfg.clone();let client=client.clone();async move{sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await}});
    p.wire.entered.notified().await;
    assert_eq!(acknowledgements::reconcile(&db,clock.as_ref()).await.unwrap(),1);
    assert_eq!(deliver(&app,&ack("device","15550100101@c.us","OUT0",2)).await.status(),200);
    assert_eq!(deliver(&app,&ack("error","15550100101@c.us","OUT0",-1)).await.status(),200);
    assert_eq!(deliver(&app,&ack("read-duplicate","15550100101@c.us","OUT0",3)).await.status(),200);
    assert_eq!(count(&db,"outgoing_ack_evidence").await,3);
    p.wire.release.notify_one();worker.await.unwrap().unwrap();
    assert_eq!(count(&db,"outbox_messages WHERE state='sent' AND positive_ack=3 AND ack_error AND returned_id IS NOT NULL").await,1);
    assert_eq!(count(&db,"contacts WHERE privacy_first_sent_at IS NOT NULL").await,1);
    enqueue(&db,&app,clock.as_ref(),&cfg,"AFTERREAD").await;
    assert_eq!(count(&db,"outbox_messages WHERE NOT privacy_prefix").await,1);
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn unmatched_alias_reconciles_only_after_authenticated_incoming_evidence() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &p, true).await;
    enqueue(&db, &app, clock.as_ref(), &cfg, "UNMATCHED").await;
    sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    clock.advance(6);
    p.wire.send_mode.store(1, Ordering::SeqCst);
    sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert_eq!(
      deliver(&app, &ack("alias-read", "99001@lid", "OUT0", 3))
        .await
        .status(),
      200
    );
    assert_eq!(
      acknowledgements::reconcile(&db, clock.as_ref())
        .await
        .unwrap(),
      0
    );
    assert_eq!(count(&db, "contact_aliases").await, 1);
    assert_eq!(
      count(&db, "outbox_messages WHERE state='uncertain'").await,
      1
    );
    let mut input = message("MAPPING", "hola");
    input["payload"]["from"] = json!("99001@lid");
    input["payload"]["id"] = json!("false_99001@lid_MAPPING");
    input["payload"]["_data"]["Info"] = json!({"SenderAlt":"15550100101@s.whatsapp.net"});
    assert_eq!(deliver(&app, &input).await.status(), 200);
    preparation::run_once(&db, clock.as_ref()).await.unwrap();
    jobs::run_once(&db, clock.as_ref(), &cfg, None)
      .await
      .unwrap();
    assert_eq!(
      acknowledgements::reconcile(&db, clock.as_ref())
        .await
        .unwrap(),
      1
    );
    assert_eq!(count(&db, "contacts").await, 1);
    assert_eq!(
      count(&db, "outbox_messages WHERE state='sent' AND positive_ack=3").await,
      1
    );
    assert_eq!(p.count("/api/sendText"), 1);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn invalid_signed_acks_have_no_effects_and_disabled_outbound_still_accepts_acks() {
  with_database(|db|async move{
    let p=Provider::new().await;let (cfg,clock,app,client,_)=outgoing(&db,&p,false).await;
    for (path,value) in [("/payload/ack",json!(5)),("/payload/ack",json!(1.5)),("/payload/ack",json!("3")),("/payload/fromMe",json!(null)),("/payload/id",json!("false_15550100101@c.us_X")),("/payload/id",json!("true_15550100102@c.us_X")),("/payload/participant",json!("someone")),("/payload/to",json!("15550100101@c.us")),("/id",json!(""))]{
      let mut event=ack("invalid","15550100101@c.us","X",3);*event.pointer_mut(path).unwrap()=value;
      assert_eq!(deliver(&app,&event).await.status(),400,"{path}");
    }
    let mut wrong_name=ack("bad-name","15550100101@c.us","X",3);wrong_name["payload"]["ackName"]=json!("DEVICE");assert_eq!(deliver(&app,&wrong_name).await.status(),400);
    for (path,value) in [("/payload/fromMe",json!(false)),("/payload/from",json!("123@g.us")),("/session",json!("foreign")),("/engine",json!("WEBJS"))] {
      let mut event=ack("ignored","15550100101@c.us","X",3);*event.pointer_mut(path).unwrap()=value;
      assert_eq!(deliver(&app,&event).await.status(),200);
    }
    assert_eq!(count(&db,"outgoing_ack_evidence").await,0);
    let response=app.clone().oneshot(Request::builder().method("POST").uri("/api/v1/webhooks/waha").body(Body::from(serde_json::to_vec(&ack("noauth","15550100101@c.us","X",3)).unwrap())).unwrap()).await.unwrap();assert_eq!(response.status(),401);
    assert_eq!(deliver(&app,&ack("unmatched","15550100101@c.us","X",3)).await.status(),200);
    assert_eq!(count(&db,"outgoing_ack_evidence").await,1);
    assert_eq!(count(&db,"contacts").await,0);assert_eq!(count(&db,"outbox_messages").await,0);
    sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap();assert!(p.wire.calls.lock().unwrap().is_empty());
    db.conn.execute_unprepared("CREATE FUNCTION fail_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic failure'; END $$; CREATE TRIGGER fail_ack BEFORE INSERT ON outgoing_ack_evidence FOR EACH ROW EXECUTE FUNCTION fail_ack()").await.unwrap();
    assert_eq!(deliver(&app,&ack("fail","15550100101@c.us","Y",3)).await.status(),503);assert_eq!(count(&db,"outgoing_ack_evidence").await,1);
  }).await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn seen_uses_original_serialized_peer_when_both_phone_and_lid_are_trusted() {
  with_database(|db|async move{
    let p=Provider::new().await;let (cfg,clock,app,client,_)=outgoing(&db,&p,true).await;
    let mut input=message("ALIASSEEN","5");input["payload"]["from"]=json!("99001@lid");input["payload"]["_data"]["Info"]=json!({"SenderAlt":"15550100101@s.whatsapp.net"});
    assert_eq!(deliver(&app,&input).await.status(),200);preparation::run_once(&db,clock.as_ref()).await.unwrap();jobs::run_once(&db, clock.as_ref(), &cfg, None).await.unwrap();
    assert_eq!(sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap(),sender::Outcome::Typing);
    clock.advance(6);sender::run_once(&db,clock.as_ref(),&cfg,Some(&client)).await.unwrap();
    let calls=p.wire.calls.lock().unwrap();
    assert_eq!(calls.iter().find(|(path,_)|path=="/api/sendSeen").unwrap().1,json!({"session":"default","chatId":"15550100101@c.us","messageIds":["false_15550100101@c.us_ALIASSEEN"]}));
    assert_eq!(calls.iter().find(|(path,_)|path=="/api/sendText").unwrap().1["chatId"],"99001@lid");
  }).await;
}
