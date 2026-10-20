use super::*;
use crate::modules::conversation::jobs;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn earlier_input_on_an_alias_blocks_only_its_contact_not_other_conversations() {
  with_database(|db|async move{
    let (cfg,clock,app,_)=setup(&db).await;
    let mut mapped=message("KNOWN","5");mapped["payload"]["from"]=json!("99001@lid");mapped["payload"]["id"]=json!("false_99001@lid_KNOWN");mapped["payload"]["_data"]["Info"]=json!({"SenderAlt":"15550100101@s.whatsapp.net"});
    assert_eq!(deliver(&app,&mapped).await.status(),200);preparation::run_once(&db,clock.as_ref()).await.unwrap();jobs::run_once(&db, clock.as_ref(), &cfg, None).await.unwrap();
    let mut earlier=message("EARLIER","5");earlier["payload"]["from"]=json!("99001@lid");earlier["payload"]["id"]=json!("false_99001@lid_EARLIER");
    assert_eq!(deliver(&app,&earlier).await.status(),200);
    db.conn.execute_unprepared("UPDATE inbox_messages SET raw_envelope=convert_to('{}','UTF8') WHERE message_id='false_99001@lid_EARLIER'").await.unwrap();
    assert!(matches!(preparation::run_once(&db,clock.as_ref()).await.unwrap(),preparation::Outcome::Failed(_)));
    assert_eq!(deliver(&app,&message("LATER","5")).await.status(),200);preparation::run_once(&db,clock.as_ref()).await.unwrap();
    let mut independent=message("INDEPENDENT","5");independent["payload"]["from"]=json!("15550100222@c.us");independent["payload"]["id"]=json!("false_15550100222@c.us_INDEPENDENT");
    assert_eq!(deliver(&app,&independent).await.status(),200);preparation::run_once(&db,clock.as_ref()).await.unwrap();
    assert_eq!(jobs::run_once(&db,clock.as_ref(),&cfg,None).await.unwrap(),jobs::Outcome::Processed);
    assert_eq!(count(&db,"outbox_messages WHERE original_target='15550100222@c.us'").await,1);
    assert_eq!(count(&db,"outbox_messages WHERE reply_inbox_id='false_15550100101@c.us_LATER'").await,0);
    assert_eq!(jobs::run_once(&db,clock.as_ref(),&cfg,None).await.unwrap(),jobs::Outcome::Idle);
  }).await;
}
