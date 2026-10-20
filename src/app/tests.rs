use std::{future::Future, sync::Arc, time::Duration};

use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement, TransactionTrait};
use tower::ServiceExt;
use uuid::Uuid;

mod google_boundary;

use super::{router, AppState};
use crate::{
  common::config::{Config, Configuration, Environment},
  database::Db,
};

// Keep log captures independent of other tests changing process-wide tracing state.
pub(crate) fn isolate_tracing_test(name: &str) -> bool {
  const CHILD: &str = "MEDICAL_ISOLATED_TRACING_TEST";
  if std::env::var(CHILD).as_deref() == Ok(name) {
    return false;
  }
  let output = std::process::Command::new(std::env::current_exe().unwrap())
    .args(["--exact", name, "--include-ignored", "--test-threads=1"])
    .env(CHILD, name)
    .output()
    .expect("Run isolated logging test");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
    "Isolated logging test {name} failed: {stdout}\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  true
}

fn configuration(env: Environment, swagger_endpoint: &str) -> Config {
  Arc::new(Configuration {
    env,
    listen_address: "127.0.0.1:0".parse().unwrap(),
    app_port: 0,
    swagger_endpoint: swagger_endpoint.into(),
    db_dsn: std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required"),
    db_pool_max_size: 2,
    db_timeout: 5,
    db_run_migrations: true,
    bcrypt_cost: 4,
    public_origin: None,
    doctor_totp_key: None,
    waha: None,
    google: None,
    claude_api_key: None,
    claude_model: None,
    claude_test_endpoint: None,
    waha_takeover_enabled: false,
    waha_outbound_enabled: false,
    waha_contact_replies_per_hour: 20,
  })
}

fn test_database_url() -> String {
  let dsn = std::env::var("TEST_DATABASE_URL")
    .expect("TEST_DATABASE_URL is required; use a dedicated *_test Postgres database");
  let url = reqwest::Url::parse(&dsn).unwrap_or_else(|_| panic!("Invalid TEST_DATABASE_URL"));
  let name = url.path().strip_prefix('/').unwrap_or_default();
  assert!(
    matches!(url.scheme(), "postgres" | "postgresql")
      && name.len() > 5
      && name.ends_with("_test")
      && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
      && url.query().is_none()
      && url.fragment().is_none(),
    "TEST_DATABASE_URL must name a dedicated *_test Postgres database without query or fragment"
  );
  dsn
}

fn connection_options(dsn: &str) -> ConnectOptions {
  let mut options = ConnectOptions::new(dsn);
  options
    .max_connections(2)
    .min_connections(1)
    .connect_timeout(Duration::from_secs(5))
    .sqlx_logging(false);
  options
}

// Only this generated schema is ever removed, including when an assertion panics.
pub(crate) async fn with_database<F, Fut>(test: F)
where
  F: FnOnce(Db) -> Fut + Send + 'static,
  Fut: Future<Output = ()> + Send + 'static,
{
  with_database_connections(2, test).await;
}

pub(crate) async fn with_database_connections<F, Fut>(connections: u32, test: F)
where
  F: FnOnce(Db) -> Fut + Send + 'static,
  Fut: Future<Output = ()> + Send + 'static,
{
  let dsn = test_database_url();
  let admin = Database::connect(connection_options(&dsn))
    .await
    .unwrap_or_else(|_| panic!("Cannot connect to dedicated TEST_DATABASE_URL"));
  let schema = format!("issue3_{}", Uuid::now_v7().simple());
  admin
    .execute(Statement::from_string(
      DbBackend::Postgres,
      format!("CREATE SCHEMA {schema}"),
    ))
    .await
    .expect("Create isolated test schema");

  let mut options = connection_options(&dsn);
  options
    .set_schema_search_path(schema.clone())
    .max_connections(connections);
  let connection = Database::connect(options).await;
  let outcome = match connection {
    Ok(conn) => {
      let db = Db { conn };
      let outcome = tokio::spawn(test(db.clone())).await;
      db.conn.close().await.expect("Close test pool");
      Some(outcome)
    }
    Err(_) => None,
  };
  admin
    .execute(Statement::from_string(
      DbBackend::Postgres,
      format!("DROP SCHEMA {schema} CASCADE"),
    ))
    .await
    .expect("Remove own test schema");
  admin.close().await.expect("Close test setup pool");
  match outcome.expect("Cannot connect to isolated test schema") {
    Ok(()) => {}
    Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
    Err(_) => panic!("Database test task was cancelled"),
  }
}

pub(crate) async fn reconnect(db: &Db, connections: u32) -> Db {
  reconnect_with_acquire_timeout(db, connections, Duration::from_secs(5)).await
}

pub(crate) async fn reconnect_with_acquire_timeout(
  db: &Db,
  connections: u32,
  acquire_timeout: Duration,
) -> Db {
  let schema: String = db
    .conn
    .query_one(Statement::from_string(
      DbBackend::Postgres,
      "SELECT current_schema() AS schema",
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "schema")
    .unwrap();
  let mut options = connection_options(&test_database_url());
  options
    .set_schema_search_path(schema)
    .max_connections(connections)
    .acquire_timeout(acquire_timeout);
  Db {
    conn: Database::connect(options).await.unwrap(),
  }
}

pub(super) async fn marker(
  axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<String, axum::http::StatusCode> {
  let row = state
    .db
    .conn
    .query_one(Statement::from_string(
      DbBackend::Postgres,
      "SELECT value FROM foundation_marker WHERE id = 1",
    ))
    .await
    .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(axum::http::StatusCode::NOT_FOUND)?;
  row
    .try_get("", "value")
    .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get(app: &Router, path: &str) -> axum::response::Response {
  app
    .clone()
    .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn http_response_tracks_committed_database_marker() {
  with_database(|db| async move {
    db.run_migrations().await.expect("Migrate fresh schema");
    let writer = db.conn.begin().await.unwrap();
    writer
      .execute(Statement::from_string(
        DbBackend::Postgres,
        "CREATE TABLE foundation_marker (id integer PRIMARY KEY, value text NOT NULL)",
      ))
      .await
      .unwrap();
    writer
      .execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO foundation_marker (id, value) VALUES (1, $1)",
        ["first committed marker".into()],
      ))
      .await
      .unwrap();
    writer.commit().await.unwrap();

    let app = router(configuration(Environment::Production, "/docs"), db.clone());
    // Hold one pooled connection so the HTTP handler must read through another.
    let writer = db.conn.begin().await.unwrap();
    let response = get(&app, "/__test/marker").await;
    assert_eq!(response.status(), 200);
    assert!(response.headers().contains_key("x-request-id"));
    assert_eq!(
      response.into_body().collect().await.unwrap().to_bytes(),
      "first committed marker"
    );

    writer
      .execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE foundation_marker SET value = $1 WHERE id = 1",
        ["second committed marker".into()],
      ))
      .await
      .unwrap();
    writer.commit().await.unwrap();
    let response = get(&app, "/__test/marker").await;
    assert_eq!(response.status(), 200);
    assert_eq!(
      response.into_body().collect().await.unwrap().to_bytes(),
      "second committed marker"
    );
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn legacy_migration_history_is_rejected_without_erasing_data() {
  with_database(|db| async move {
    db.run_migrations().await.unwrap();
    db.conn
      .execute(Statement::from_string(
        DbBackend::Postgres,
        "CREATE TABLE foundation_marker (id integer PRIMARY KEY, value text NOT NULL)",
      ))
      .await
      .unwrap();
    db.conn
      .execute(Statement::from_string(
        DbBackend::Postgres,
        "INSERT INTO foundation_marker VALUES (1, 'preserve legacy data')",
      ))
      .await
      .unwrap();
    db.conn
      .execute(Statement::from_string(
        DbBackend::Postgres,
        "INSERT INTO seaql_migrations (version, applied_at)
       VALUES ('m20240126114845_create_users_table', 1700000000)",
      ))
      .await
      .unwrap();

    let error = db
      .run_migrations()
      .await
      .expect_err("Legacy history must fail closed");
    assert!(error
      .to_string()
      .contains("m20240126114845_create_users_table"));
    let app = router(configuration(Environment::Production, "/docs"), db.clone());
    let response = get(&app, "/__test/marker").await;
    assert_eq!(response.status(), 200);
    assert_eq!(
      response.into_body().collect().await.unwrap().to_bytes(),
      "preserve legacy data"
    );
    let row = db
      .conn
      .query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT count(*) AS count FROM seaql_migrations
       WHERE version = 'm20240126114845_create_users_table'",
      ))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(row.try_get::<i64>("", "count").unwrap(), 1);
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn health_uses_the_real_router_and_request_id_middleware() {
  with_database(|db| async move {
    for env in [Environment::Development, Environment::Production] {
      let app = router(configuration(env, "/docs"), db.clone());
      let response = get(&app, "/api/v1/health").await;
      assert_eq!(response.status(), 200);
      let request_id = response
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
      assert!(Uuid::parse_str(request_id).is_ok());
      let body = response.into_body().collect().await.unwrap().to_bytes();
      assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"status": "ok"})
      );

      let response = app
        .oneshot(
          Request::builder()
            .uri("/api/v1/health")
            .header("x-request-id", "foundation-smoke")
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
      assert_eq!(response.status(), 200);
      assert_eq!(response.headers()["x-request-id"], "foundation-smoke");
    }
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn development_serves_swagger_and_only_current_api_documentation() {
  with_database(|db| async move {
    for endpoint in ["/docs", "/private-docs"] {
      let app = router(
        configuration(Environment::Development, endpoint),
        db.clone(),
      );
      let response = get(&app, &format!("{endpoint}/")).await;
      assert_eq!(response.status(), 200);
      let body = response.into_body().collect().await.unwrap().to_bytes();
      assert!(std::str::from_utf8(&body).unwrap().contains("Swagger UI"));
      let response = get(&app, &format!("{endpoint}/api-doc/openapi.json")).await;
      assert_eq!(response.status(), 200);
      let body = response.into_body().collect().await.unwrap().to_bytes();
      let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
      let paths = doc["paths"].as_object().unwrap();
      assert_eq!(paths.len(), 1);
      assert!(paths.contains_key("/api/v1/health"));
      assert!(doc["components"]["securitySchemes"].is_null());
      assert!(doc["components"]["schemas"]
        .as_object()
        .unwrap()
        .keys()
        .all(|name| !name.contains("User") && !name.contains("Auth")));
    }
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn obsolete_accounts_and_graphql_are_unavailable() {
  with_database(|db| async move {
    for env in [Environment::Development, Environment::Production] {
      let app = router(configuration(env, "/docs"), db.clone());
      for (method, path) in [
        ("POST", "/api/v1/auth/register"),
        ("POST", "/api/v1/auth/login"),
        ("POST", "/api/v1/users"),
        ("GET", "/api/v1/users"),
        ("GET", "/api/v1/users/00000000-0000-0000-0000-000000000001"),
        ("PUT", "/api/v1/users/00000000-0000-0000-0000-000000000001"),
        (
          "DELETE",
          "/api/v1/users/00000000-0000-0000-0000-000000000001",
        ),
        ("GET", "/graphql"),
        ("POST", "/graphql"),
      ] {
        let response = app
          .clone()
          .oneshot(
            Request::builder()
              .method(method)
              .uri(path)
              .header("content-type", "application/json")
              .body(Body::from("{}"))
              .unwrap(),
          )
          .await
          .unwrap();
        assert_eq!(response.status(), 404, "Obsolete route: {method} {path}");
      }
    }
  })
  .await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn production_does_not_expose_swagger_or_openapi() {
  with_database(|db| async move {
    for endpoint in ["/docs", "/private-docs"] {
      let app = router(configuration(Environment::Production, endpoint), db.clone());
      for path in [
        endpoint.to_string(),
        format!("{endpoint}/"),
        format!("{endpoint}/api-doc/openapi.json"),
      ] {
        let response = get(&app, &path).await;
        assert_eq!(response.status(), 404, "Production exposed {path}");
      }
    }
  })
  .await;
}
