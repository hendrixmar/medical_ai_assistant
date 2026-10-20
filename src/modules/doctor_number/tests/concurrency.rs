use super::*;
use crate::modules::{conversation::jobs, whatsapp::jobs as preparation};
use sea_orm::TransactionTrait;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn replacing_binding_never_locks_previous_contact_and_preserves_business_records() {
  crate::app::tests::with_database_connections(4,|db|async move {
    let p=Provider::new().await; let(cfg,clock,app,session,csrf)=setup(&db,&p).await;
    process(&db,&app,clock.as_ref(),&cfg,&message("FOLLOWUP","525512345678@c.us","5",clock.now_millis())).await;
    let snapshot_sql="SELECT json_build_object('practice',(SELECT row_to_json(p) FROM practice p),'services',(SELECT json_agg(s ORDER BY id) FROM services s),'followups',(SELECT json_agg(f ORDER BY id) FROM followup_requests f))::text AS snapshot";
    let before:String=db.conn.query_one(sql(snapshot_sql)).await.unwrap().unwrap().try_get("","snapshot").unwrap();
    assert_eq!(issue(&app,&session,&csrf).await.status(),303); let code=active_code(&app,&session).await; clock.advance(1);
    process(&db,&app,clock.as_ref(),&cfg,&message("FIRST","525512345678@c.us",&code,clock.now_millis())).await;
    for (i,body) in ["5","Cancelar cita","Confirmar pago recibido","Agenda de mañana"].iter().enumerate() {
      process(&db,&app,clock.as_ref(),&cfg,&message(&format!("MUTATE{i}"),"525512345678@c.us",body,clock.now_millis())).await;
    }
    assert_eq!(db.conn.query_one(sql(snapshot_sql)).await.unwrap().unwrap().try_get::<String>("","snapshot").unwrap(),before);
    clock.advance(60_000); assert_eq!(issue(&app,&session,&csrf).await.status(),303); let code=active_code(&app,&session).await; clock.advance(1);
    *p.wire.status.lock().unwrap()=json!({"name":"default","status":"WORKING","me":{"id":"525555555556@c.us"}});
    let page=text(authenticated_get(&app,PATH,&session).await).await;
    assert!(page.contains("+525555555555")); assert!(page.contains("+525555555556"));
    let mut switched=message("SWITCHED","525512345679@c.us",&code,clock.now_millis()); switched["me"]=json!({"id":"525555555556@c.us"});
    process(&db,&app,clock.as_ref(),&cfg,&switched).await;
    assert_eq!(count(&db,"doctor_number_state WHERE verified_phone='525512345678@c.us'").await,1);
    clock.advance(60_000); assert_eq!(issue(&app,&session,&csrf).await.status(),303); let code=active_code(&app,&session).await; clock.advance(1);
    let mut replacement=message("REPLACE","525512345679@c.us",&code,clock.now_millis()); replacement["me"]=json!({"id":"525555555556@c.us"});
    deliver(&app,&replacement).await; preparation::run_once(&db,clock.as_ref()).await.unwrap();
    let old=db.conn.begin().await.unwrap(); old.query_one(sql("SELECT id FROM contacts WHERE id=(SELECT contact_id FROM doctor_number_state) FOR UPDATE")).await.unwrap();
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(1),jobs::run_once(&db,clock.as_ref(),&cfg,None)).await.expect("redemption must not acquire prior Contact").unwrap(),jobs::Outcome::Processed);
    old.commit().await.unwrap();
    assert_eq!(count(&db,"doctor_number_state WHERE verified_phone='525512345679@c.us' AND practice_phone='525555555556@c.us'").await,1);
    assert_eq!(db.conn.query_one(sql(snapshot_sql)).await.unwrap().unwrap().try_get::<String>("","snapshot").unwrap(),before);
  }).await;
}

async fn waiting(db: &crate::database::Db, minimum: i64) {
  tokio::time::timeout(std::time::Duration::from_secs(1),async {
    loop {
      let row=db.conn.query_one(sql("SELECT count(DISTINCT l.pid) AS n FROM pg_locks l JOIN pg_locks r ON r.pid=l.pid WHERE NOT l.granted AND r.relation='doctor_number_state'::regclass AND r.granted")).await.unwrap().unwrap();
      if row.try_get::<i64>("","n").unwrap()>=minimum { break; }
      tokio::task::yield_now().await;
    }
  }).await.expect("workers must reach Doctor Number singleton gate");
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn simultaneous_different_chats_consume_once_under_the_singleton_lock() {
  crate::app::tests::with_database_connections(5, |db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(1);
    for (id, sender) in [("A", "525512345678@c.us"), ("B", "525512345679@c.us")] {
      deliver(&app, &message(id, sender, &code, clock.now_millis())).await;
      preparation::run_once(&db, clock.as_ref()).await.unwrap();
    }
    let gate = db.conn.begin().await.unwrap();
    gate
      .query_one(sql(
        "SELECT id FROM doctor_number_state WHERE id=1 FOR UPDATE",
      ))
      .await
      .unwrap();
    let (d, c, f) = (db.clone(), clock.clone(), cfg.clone());
    let a = tokio::spawn(async move { jobs::run_once(&d, c.as_ref(), &f, None).await });
    waiting(&db, 1).await;
    let (d, c, f) = (db.clone(), clock.clone(), cfg.clone());
    let b = tokio::spawn(async move { jobs::run_once(&d, c.as_ref(), &f, None).await });
    waiting(&db, 2).await;
    gate.commit().await.unwrap();
    a.await.unwrap().unwrap();
    b.await.unwrap().unwrap();
    assert_eq!(
      count(&db, "doctor_number_challenges WHERE state='used'").await,
      1
    );
    assert_eq!(
      count(&db, "outbox_messages WHERE template='doctor_verified'").await,
      1
    );
    assert_eq!(count(&db, "outbox_messages WHERE template='menu'").await, 1);
    assert_eq!(
      count(
        &db,
        "processing_jobs WHERE kind='process_conversation' AND status='succeeded'"
      )
      .await,
      2
    );
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn expiry_is_resampled_after_waiting_for_the_singleton() {
  crate::app::tests::with_database_connections(4, |db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(599_999);
    deliver(
      &app,
      &message("WAIT", "525512345678@c.us", &code, clock.now_millis()),
    )
    .await;
    preparation::run_once(&db, clock.as_ref()).await.unwrap();
    let gate = db.conn.begin().await.unwrap();
    gate
      .query_one(sql(
        "SELECT id FROM doctor_number_state WHERE id=1 FOR UPDATE",
      ))
      .await
      .unwrap();
    let (d, c, f) = (db.clone(), clock.clone(), cfg.clone());
    let worker = tokio::spawn(async move { jobs::run_once(&d, c.as_ref(), &f, None).await });
    waiting(&db, 1).await;
    clock.advance(1);
    gate.commit().await.unwrap();
    worker.await.unwrap().unwrap();
    assert_eq!(
      count(&db, "doctor_number_state WHERE verified_phone IS NOT NULL").await,
      0
    );
    assert_eq!(
      count(&db, "doctor_number_challenges WHERE state='expired'").await,
      1
    );
    assert_eq!(count(&db, "outbox_messages WHERE template='menu'").await, 1);
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn issuance_waiting_behind_redemption_keeps_the_binding_and_supersedes_only_the_challenge() {
  crate::app::tests::with_database_connections(5, |db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(60_000);
    deliver(
      &app,
      &message("RACE", "525512345678@c.us", &code, clock.now_millis()),
    )
    .await;
    preparation::run_once(&db, clock.as_ref()).await.unwrap();
    let gate = db.conn.begin().await.unwrap();
    gate
      .query_one(sql(
        "SELECT id FROM doctor_number_state WHERE id=1 FOR UPDATE",
      ))
      .await
      .unwrap();
    let (d, c, f) = (db.clone(), clock.clone(), cfg.clone());
    let worker = tokio::spawn(async move { jobs::run_once(&d, c.as_ref(), &f, None).await });
    waiting(&db, 1).await;
    let (a, s, t) = (app.clone(), session.clone(), csrf.clone());
    let issuer = tokio::spawn(async move { issue(&a, &s, &t).await });
    waiting(&db, 2).await;
    gate.commit().await.unwrap();
    worker.await.unwrap().unwrap();
    assert_eq!(issuer.await.unwrap().status(), 303);
    assert_eq!(
      count(
        &db,
        "doctor_number_state WHERE verified_phone='525512345678@c.us'"
      )
      .await,
      1
    );
    assert_eq!(
      count(&db, "doctor_number_challenges WHERE state='used'").await,
      1
    );
    assert_eq!(
      count(&db, "doctor_number_challenges WHERE state='active'").await,
      1
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
async fn redemption_waiting_behind_issuance_cannot_consume_the_superseded_code() {
  crate::app::tests::with_database_connections(5, |db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(60_000);
    deliver(
      &app,
      &message("RACE", "525512345678@c.us", &code, clock.now_millis()),
    )
    .await;
    preparation::run_once(&db, clock.as_ref()).await.unwrap();
    let gate = db.conn.begin().await.unwrap();
    gate
      .query_one(sql(
        "SELECT id FROM doctor_number_state WHERE id=1 FOR UPDATE",
      ))
      .await
      .unwrap();
    let (a, s, t) = (app.clone(), session.clone(), csrf.clone());
    let issuer = tokio::spawn(async move { issue(&a, &s, &t).await });
    waiting(&db, 1).await;
    let (d, c, f) = (db.clone(), clock.clone(), cfg.clone());
    let worker = tokio::spawn(async move { jobs::run_once(&d, c.as_ref(), &f, None).await });
    waiting(&db, 2).await;
    gate.commit().await.unwrap();
    assert_eq!(issuer.await.unwrap().status(), 303);
    worker.await.unwrap().unwrap();
    assert_eq!(
      count(&db, "doctor_number_state WHERE verified_phone IS NOT NULL").await,
      0
    );
    assert_eq!(
      count(
        &db,
        "doctor_number_challenges WHERE state='superseded' AND ciphertext IS NULL"
      )
      .await,
      1
    );
    assert_eq!(
      count(
        &db,
        "doctor_number_challenges WHERE state='active' AND failures=0"
      )
      .await,
      1
    );
    assert_eq!(
      count(&db, "outbox_messages WHERE template='doctor_verified'").await,
      0
    );
  })
  .await;
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn processing_clock_is_sampled_again_at_consumption() {
  struct ConsumeClock {
    calls: AtomicUsize,
    before: i64,
  }
  impl Clock for ConsumeClock {
    fn now(&self) -> i64 {
      self.before / 1000
    }
    fn now_millis(&self) -> i64 {
      self.before + i64::from(self.calls.fetch_add(1, Ordering::SeqCst) >= 3)
    }
  }
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, session, csrf) = setup(&db, &p).await;
    assert_eq!(issue(&app, &session, &csrf).await.status(), 303);
    let code = active_code(&app, &session).await;
    clock.advance(599_999);
    deliver(
      &app,
      &message("CONSUME", "525512345678@c.us", &code, clock.now_millis()),
    )
    .await;
    preparation::run_once(&db, clock.as_ref()).await.unwrap();
    let consumption = ConsumeClock {
      calls: AtomicUsize::new(0),
      before: clock.now_millis(),
    };
    jobs::run_once(&db, &consumption, &cfg, None).await.unwrap();
    assert_eq!(
      count(&db, "doctor_number_state WHERE verified_phone IS NOT NULL").await,
      0
    );
    assert_eq!(
      count(&db, "doctor_number_challenges WHERE state='expired'").await,
      1
    );
  })
  .await;
}
