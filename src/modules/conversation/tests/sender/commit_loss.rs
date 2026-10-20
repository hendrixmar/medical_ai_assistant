use super::*;
use sea_orm::{ConnectOptions, Database};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// Real Postgres protocol relay: discard the COMMIT success response, not the work.
// This is a test-only socket fault, never a substitute database or business mock.
struct CommitLoss {
  base: reqwest::Url,
  armed: Arc<std::sync::atomic::AtomicBool>,
  committed: Arc<tokio::sync::Notify>,
  task: tokio::task::JoinHandle<()>,
}
impl Drop for CommitLoss {
  fn drop(&mut self) {
    self.task.abort();
  }
}
async fn frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<(u8, Vec<u8>)> {
  let tag = reader.read_u8().await?;
  let length = reader.read_u32().await?;
  if !(4..=8 * 1024 * 1024).contains(&length) {
    return Err(std::io::ErrorKind::InvalidData.into());
  }
  let mut data = vec![0; (length - 4) as usize];
  reader.read_exact(&mut data).await?;
  Ok((tag, data))
}
async fn forward<W: AsyncWrite + Unpin>(
  writer: &mut W,
  tag: u8,
  data: &[u8],
) -> std::io::Result<()> {
  writer.write_u8(tag).await?;
  writer.write_u32(data.len() as u32 + 4).await?;
  writer.write_all(data).await?;
  writer.flush().await
}
impl CommitLoss {
  async fn new() -> Self {
    let mut base = reqwest::Url::parse(&std::env::var("TEST_DATABASE_URL").unwrap()).unwrap();
    let host = base.host_str().unwrap().to_owned();
    assert!(matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]"));
    let port = base.port().unwrap_or(5432);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    base.set_host(Some("127.0.0.1")).unwrap();
    base
      .set_port(Some(listener.local_addr().unwrap().port()))
      .unwrap();
    base.set_query(Some("sslmode=disable"));
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let committed = Arc::new(tokio::sync::Notify::new());
    let task = tokio::spawn({
      let armed = armed.clone();
      let committed = committed.clone();
      async move {
        loop {
          let Ok((mut downstream, _)) = listener.accept().await else {
            break;
          };
          let target = (host.clone(), port);
          let armed = armed.clone();
          let committed = committed.clone();
          tokio::spawn(async move {
            let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();
            let length = downstream.read_u32().await.unwrap();
            let mut startup = vec![0; (length - 4) as usize];
            downstream.read_exact(&mut startup).await.unwrap();
            upstream.write_u32(length).await.unwrap();
            upstream.write_all(&startup).await.unwrap();
            let (mut dr, mut dw) = downstream.into_split();
            let (mut ur, mut uw) = upstream.into_split();
            let suppress = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let frontend = {
              let suppress = suppress.clone();
              async move {
                let mut commits = 0;
                while let Ok((tag, data)) = frame(&mut dr).await {
                  if armed.load(Ordering::SeqCst) && tag == b'Q' && data == b"COMMIT\0" {
                    commits += 1;
                    if commits == 2 {
                      suppress.store(true, Ordering::SeqCst);
                    }
                  }
                  if forward(&mut uw, tag, &data).await.is_err() {
                    break;
                  }
                }
              }
            };
            let backend = async move {
              while let Ok((tag, data)) = frame(&mut ur).await {
                if suppress.load(Ordering::SeqCst) {
                  if tag == b'C' && data == b"COMMIT\0" {
                    committed.notify_one();
                    break;
                  }
                } else if forward(&mut dw, tag, &data).await.is_err() {
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
      task,
    }
  }
  async fn db(&self, existing: &crate::database::Db) -> crate::database::Db {
    let schema: String = existing
      .conn
      .query_one(sql("SELECT current_schema() AS schema"))
      .await
      .unwrap()
      .unwrap()
      .try_get("", "schema")
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
async fn committed_but_lost_database_acknowledgement_never_dispatches() {
  with_database(|db| async move {
    let p = Provider::new().await;
    let (cfg, clock, app, client, _) = outgoing(&db, &p, true).await;
    enqueue(&db, &app, clock.as_ref(), &cfg, "LOSTCOMMIT").await;
    sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    clock.advance(6);
    let proxy = CommitLoss::new().await;
    let lossy = proxy.db(&db).await;
    proxy.armed.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(
      std::time::Duration::from_secs(5),
      sender::run_once(&lossy, clock.as_ref(), &cfg, Some(&client)),
    )
    .await
    .expect("COMMIT response is deliberately dropped");
    assert!(result.is_err());
    tokio::time::timeout(
      std::time::Duration::from_secs(1),
      proxy.committed.notified(),
    )
    .await
    .expect("Real PostgreSQL committed before the lost acknowledgement");
    assert_eq!(
      count(
        &db,
        "outbox_messages WHERE state='uncertain' AND dispatch_committed_at IS NOT NULL"
      )
      .await,
      1
    );
    assert_eq!(
      count(
        &db,
        "processing_jobs WHERE kind='send_outbox' AND status='succeeded'"
      )
      .await,
      1
    );
    assert_eq!(p.count("/api/sendText"), 0);
    clock.advance(126);
    sender::run_once(&db, clock.as_ref(), &cfg, Some(&client))
      .await
      .unwrap();
    assert_eq!(p.count("/api/sendText"), 0);
    lossy.conn.close().await.unwrap();
  })
  .await;
}
