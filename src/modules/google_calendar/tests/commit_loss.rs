use super::{
  fixture::Fixture,
  probes::{start, ticks},
  *,
};
use crate::modules::{doctor::Clock, google_calendar::worker};
use sea_orm::{ConnectOptions, Database, TransactionTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// Real local Postgres wire fault. No provider TLS change and no database mock.
struct CommitLoss {
  base: reqwest::Url,
  armed: Arc<AtomicBool>,
  committed: Arc<tokio::sync::Notify>,
  release: Arc<tokio::sync::Semaphore>,
  task: tokio::task::JoinHandle<()>,
}
impl Drop for CommitLoss {
  fn drop(&mut self) {
    self.task.abort();
  }
}
async fn frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<(u8, Vec<u8>)> {
  let tag = r.read_u8().await?;
  let n = r.read_u32().await?;
  if !(4..=8 * 1024 * 1024).contains(&n) {
    return Err(std::io::ErrorKind::InvalidData.into());
  }
  let mut bytes = vec![0; (n - 4) as usize];
  r.read_exact(&mut bytes).await?;
  Ok((tag, bytes))
}
async fn forward<W: AsyncWrite + Unpin>(w: &mut W, tag: u8, bytes: &[u8]) -> std::io::Result<()> {
  w.write_u8(tag).await?;
  w.write_u32(bytes.len() as u32 + 4).await?;
  w.write_all(bytes).await?;
  w.flush().await
}
impl CommitLoss {
  async fn new() -> Self {
    Self::intercept(b"SET phase='create_committed'", false, 0).await
  }
  async fn intercept(target_sql: &'static [u8], delay: bool, skip: usize) -> Self {
    let mut base = reqwest::Url::parse(&std::env::var("TEST_DATABASE_URL").unwrap()).unwrap();
    let host = base.host_str().unwrap().to_owned();
    assert!(matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]"));
    assert!(base.query().is_none());
    let port = base.port().unwrap_or(5432);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    base.set_host(Some("127.0.0.1")).unwrap();
    base
      .set_port(Some(listener.local_addr().unwrap().port()))
      .unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let committed = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let task = tokio::spawn({
      let armed = armed.clone();
      let committed = committed.clone();
      let release = release.clone();
      async move {
        loop {
          let Ok((mut downstream, _)) = listener.accept().await else {
            break;
          };
          let (host, armed, committed, release) = (
            host.clone(),
            armed.clone(),
            committed.clone(),
            release.clone(),
          );
          tokio::spawn(async move {
            let mut upstream = tokio::net::TcpStream::connect((host, port)).await.unwrap();
            upstream.set_nodelay(true).unwrap();
            downstream.set_nodelay(true).unwrap();
            loop {
              let n = downstream.read_u32().await.unwrap();
              assert!((8..=1024 * 1024).contains(&n));
              let mut startup = vec![0; (n - 4) as usize];
              downstream.read_exact(&mut startup).await.unwrap();
              upstream.write_u32(n).await.unwrap();
              upstream.write_all(&startup).await.unwrap();
              upstream.flush().await.unwrap();
              if n == 8 && startup == [4, 210, 22, 47] {
                // Forward the disposable server's real SSL negotiation unchanged.
                let reply = upstream.read_u8().await.unwrap();
                assert_eq!(
                  reply, b'N',
                  "wire fault requires the existing plaintext disposable Postgres fixture"
                );
                downstream.write_u8(reply).await.unwrap();
                downstream.flush().await.unwrap();
              } else {
                break;
              }
            }
            let (mut dr, mut dw) = downstream.into_split();
            let (mut ur, mut uw) = upstream.into_split();
            let suppress = Arc::new(AtomicBool::new(false));
            let frontend = {
              let suppress = suppress.clone();
              async move {
                let mut target = false;
                let mut statements = std::collections::HashSet::new();
                let mut skip = skip;
                while let Ok((tag, bytes)) = frame(&mut dr).await {
                  if tag == b'P' && bytes.windows(target_sql.len()).any(|w| w == target_sql) {
                    statements.insert(bytes.split(|b| *b == 0).next().unwrap().to_vec());
                  }
                  if armed.load(Ordering::SeqCst)
                    && tag == b'B'
                    && bytes
                      .split(|b| *b == 0)
                      .nth(1)
                      .is_some_and(|name| statements.contains(name))
                  {
                    if skip > 0 {
                      skip -= 1;
                    } else {
                      target = armed.swap(false, Ordering::SeqCst);
                    }
                  }
                  if target && tag == b'Q' && bytes == b"COMMIT\0" {
                    suppress.store(true, Ordering::SeqCst);
                    target = false;
                  }
                  if forward(&mut uw, tag, &bytes).await.is_err() {
                    break;
                  }
                }
              }
            };
            let backend = async move {
              while let Ok((tag, bytes)) = frame(&mut ur).await {
                if suppress.load(Ordering::SeqCst) {
                  if tag == b'C' && bytes == b"COMMIT\0" {
                    committed.notify_one();
                    if !delay {
                      break;
                    }
                    release.acquire().await.unwrap().forget();
                    suppress.store(false, Ordering::SeqCst);
                    if forward(&mut dw, tag, &bytes).await.is_err() {
                      break;
                    }
                  }
                } else if forward(&mut dw, tag, &bytes).await.is_err() {
                  break;
                }
              }
            };
            tokio::select! {_=frontend=>{},_=backend=>{}}
          });
        }
      }
    });
    Self {
      base,
      armed,
      committed,
      release,
      task,
    }
  }
  async fn db(&self, db: &crate::database::Db) -> crate::database::Db {
    let schema = db
      .conn
      .query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT current_schema() AS schema",
      ))
      .await
      .unwrap()
      .unwrap()
      .try_get::<String>("", "schema")
      .unwrap();
    let mut options = ConnectOptions::new(self.base.to_string());
    options
      .max_connections(1)
      .min_connections(1)
      .set_schema_search_path(schema)
      .sqlx_logging(false);
    crate::database::Db {
      conn: Database::connect(options).await.unwrap(),
    }
  }
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn probe_refresh_rechecks_budget_and_claim_after_reservation_and_database_waits() {
  for stage in ["lock", "refresh_commit", "fence_lock", "fence_commit"] {
    for invalid in [
      "boundary",
      "insufficient",
      "expired",
      "stale_claim",
      "expired_claim",
    ] {
      with_database(move |db| async move {
        let f = Fixture::new(&db).await;
        f.provider.grant.lock().await["expires_in"] = serde_json::json!(60);
        f.connect().await; f.select().await; start(&f, "").await;
        ticks(&f, &db, 2).await;
        f.clock.advance(10);
        db.conn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
          "UPDATE google_probes SET cleanup_deadline=$1",vec![(f.clock.now_millis()+9_000).into()])).await.unwrap();
        let (sql, skip): (&[u8], usize) = match stage {
          "lock" => (b"SET cleanup_calls=cleanup_calls+1",0),
          "fence_commit" => (b"SELECT * FROM google_connection WHERE id=1 FOR UPDATE",3),
          _ => (b"SET refresh_claim=",0),
        };
        let proxy = CommitLoss::intercept(sql,true,skip).await;
        let delayed = proxy.db(&db).await;
        proxy.armed.store(true,Ordering::SeqCst);
        let before = f.provider.calls.lock().await.len();
        let (pool, clock, cfg, client) = (delayed.clone(),f.clock.clone(),f.cfg.clone(),f.client.clone());
        let task = tokio::spawn(async move { worker::run_once(&pool,clock.as_ref(),&cfg,Some(&client)).await });
        tokio::time::timeout(std::time::Duration::from_secs(4),proxy.committed.notified()).await.expect("real refresh-path COMMIT");
        let lock = if stage.ends_with("lock") {
          let tx = db.conn.begin().await.unwrap();
          tx.query_one(Statement::from_string(DbBackend::Postgres,"SELECT * FROM google_connection WHERE id=1 FOR UPDATE")).await.unwrap();
          proxy.release.add_permits(1);
          tokio::time::timeout(std::time::Duration::from_secs(1),async {
            loop {
              let row = db.conn.query_one(Statement::from_string(DbBackend::Postgres,
                "SELECT EXISTS(SELECT FROM pg_locks held JOIN pg_locks waiting ON waiting.pid=held.pid WHERE held.relation='google_connection'::regclass AND held.mode='RowShareLock' AND held.granted AND NOT waiting.granted) AS blocked")).await.unwrap().unwrap();
              if row.try_get::<bool>("","blocked").unwrap() {break;}
              tokio::task::yield_now().await;
            }
          }).await.expect("actual refresh connection lock waiter");
          Some(tx)
        } else { None };
        if invalid == "stale_claim" {
          // When the final read fence already committed, only elapsed time can be
          // checked without another endless fence. Exercise stale claims before it.
          if stage == "fence_commit" { f.clock.advance(2); }
          else {
            let statement = Statement::from_string(DbBackend::Postgres,"UPDATE google_probes SET claim='00000000-0000-0000-0000-000000000002'");
            if let Some(tx) = &lock {tx.execute(statement).await.unwrap();} else {db.conn.execute(statement).await.unwrap();}
          }
        } else { f.clock.advance(match invalid {"boundary"=>1,"insufficient"=>2,"expired"=>10,_=>31}); }
        if let Some(tx) = lock {tx.commit().await.unwrap();} else {proxy.release.add_permits(1);}
        task.await.unwrap().unwrap();
        assert_eq!(f.provider.calls.lock().await.len(),before,"no refresh HTTP at {stage}/{invalid}");
        delayed.conn.close().await.unwrap();
      }).await;
    }
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn delayed_confirmed_commit_cannot_outlive_observation_or_cleanup_budget() {
  for cleanup in [false, true] {
    for advance in [1, 2, 10] {
      with_database(move |db| async move {
        let f = Fixture::new(&db).await;
        f.connect().await; f.select().await; start(&f, "").await;
        ticks(&f, &db, if cleanup { 2 } else { 1 }).await;
        db.conn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
          if cleanup { "UPDATE google_probes SET cleanup_deadline=$1" } else { "UPDATE google_probes SET observation_deadline=$1" },
          vec![(f.clock.now_millis()+9_000).into()])).await.unwrap();
        let proxy = CommitLoss::intercept(if cleanup { b"SET cleanup_calls=cleanup_calls+1" } else { b"SET observations=observations+1" },true,if cleanup {2} else {0}).await;
        let delayed = proxy.db(&db).await;
        proxy.armed.store(true,Ordering::SeqCst);
        let before = f.provider.calls.lock().await.len();
        let (pool, clock, cfg, client) = (delayed.clone(),f.clock.clone(),f.cfg.clone(),f.client.clone());
        let mut task = tokio::spawn(async move { worker::run_once(&pool,clock.as_ref(),&cfg,Some(&client)).await });
        tokio::select! {
          result = &mut task => panic!("worker finished before phase COMMIT: {result:?}"),
          result = tokio::time::timeout(std::time::Duration::from_secs(4),proxy.committed.notified()) => { result.expect("real phase reservation COMMIT"); }
        }
        f.clock.advance(advance);
        proxy.release.add_permits(1);
        task.await.unwrap().unwrap();
        assert_eq!(f.provider.calls.lock().await.len(),before + usize::from(cleanup),"no send after delayed confirmed phase COMMIT");
        delayed.conn.close().await.unwrap();
      }).await;
    }
  }
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn successful_create_commit_with_lost_ack_forbids_insert_and_restart_never_reinserts() {
  with_database(|db| async move {
    let f = Fixture::new(&db).await;
    f.connect().await;
    f.select().await;
    start(&f, "").await;
    let proxy = CommitLoss::new().await;
    let lossy = proxy.db(&db).await;
    proxy.armed.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(
      std::time::Duration::from_secs(8),
      worker::run_once(&lossy, f.clock.as_ref(), &f.cfg, Some(&f.client)),
    )
    .await
    .expect("bounded dropped PostgreSQL COMMIT acknowledgement");
    assert!(result.is_err());
    tokio::time::timeout(
      std::time::Duration::from_secs(1),
      proxy.committed.notified(),
    )
    .await
    .expect("real Postgres committed");
    let row = db
      .conn
      .query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT phase,create_committed_at FROM google_probes",
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      row.try_get::<String>("", "phase").unwrap(),
      "create_committed"
    );
    assert!(row
      .try_get::<Option<i64>>("", "create_committed_at")
      .unwrap()
      .is_some());
    assert!(!f
      .provider
      .calls
      .lock()
      .await
      .iter()
      .any(|(m, p, _)| m == "POST" && p.contains("/events?")));
    f.clock.advance(31);
    ticks(&f, &db, 32).await;
    assert!(!f
      .provider
      .calls
      .lock()
      .await
      .iter()
      .any(|(m, p, _)| m == "POST" && p.contains("/events?")));
    lossy.conn.close().await.unwrap();
  })
  .await;
}
