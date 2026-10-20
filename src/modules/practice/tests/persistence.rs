use super::*;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};

fn sql(query: &str) -> Statement {
  Statement::from_string(DbBackend::Postgres, query)
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn completed_configuration_and_stable_services_survive_closed_pool_and_new_connection() {
  with_database(|db| async move {
    let (cfg, clock, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&page(&app, &session, "details").await, "csrf_token");
    assert_eq!(save(&app, &session, "details", &csrf, &details::details_fields()).await.status(), 303);
    let next = text(authenticated_get(&app, "/settings", &session).await).await;
    assert!(next.contains("id=\"step-title\">Añadir servicio"));
    let id = field(&next, "service_id");
    assert_eq!(save(&app, &session, "services", &csrf, &[
      ("service_id", &id), ("service_name", "Consulta de prueba"), ("duration_minutes", "45"), ("price_mxn", "1000.50"),
    ]).await.status(), 303);
    let next = text(authenticated_get(&app, "/settings", &session).await).await;
    assert!(next.contains("id=\"step-title\">Horarios"));
    assert_eq!(save(&app, &session, "schedule", &csrf, &schedule::schedule_fields()).await.status(), 303);
    let next = text(authenticated_get(&app, "/settings", &session).await).await;
    assert!(next.contains("id=\"step-title\">Pagos de video"));
    assert_eq!(save(&app, &session, "payments", &csrf, &payments::payment_fields()).await.status(), 303);
    let next = text(authenticated_get(&app, "/settings", &session).await).await;
    assert!(next.contains("id=\"step-title\">Datos del consultorio"));
    assert_eq!(next.matches("<span class=\"step-state\">Listo</span>").count(), 4);

    let schema: String = db.conn.query_one(sql("SELECT current_schema() AS schema")).await.unwrap().unwrap().try_get("", "schema").unwrap();
    drop(app);
    db.conn.clone().close().await.unwrap();
    let mut options = ConnectOptions::new(cfg.db_dsn.clone());
    options.set_schema_search_path(schema).min_connections(1).max_connections(2).sqlx_logging(false);
    let fresh_db = crate::database::Db { conn: Database::connect(options).await.expect("Reconnect isolated test schema") };
    fresh_db.run_migrations().await.unwrap();
    let app = crate::app::router_with_clock(cfg, fresh_db.clone(), clock);
    let details = page(&app, &session, "details").await;
    assert_eq!(field(&details, "name"), "Consultorio de prueba");
    assert_eq!(details.matches("<span class=\"step-state\">Listo</span>").count(), 4);
    let service = page(&app, &session, &format!("services&service={id}")).await;
    assert_eq!(field(&service, "service_id"), id);
    assert_eq!(field(&service, "price_mxn"), "1000.50");
    assert_eq!(field(&page(&app, &session, "schedule").await, "grid_minutes"), "20");
    assert_eq!(field(&page(&app, &session, "payments").await, "clabe"), "001234567890123456");

    let edited = changed(details::details_fields(), "location", "Otra dirección");
    assert_eq!(save(&app, &session, "details", &csrf, &edited).await.status(), 303);
    assert!(page(&app, &session, "details").await.contains("Otra dirección"));
    assert_eq!(save(&app, &session, "schedule", &csrf, &changed(schedule::schedule_fields(), "grid_minutes", "60")).await.status(), 303);
    assert_eq!(field(&page(&app, &session, "payments").await, "clabe"), "001234567890123456");
    assert_eq!(save(&app, &session, "payments", &csrf, &changed(payments::payment_fields(), "receipt_window_minutes", "90")).await.status(), 303);
    assert_eq!(field(&page(&app, &session, "schedule").await, "grid_minutes"), "60");
    assert!(page(&app, &session, "details").await.contains("Otra dirección"));
    let row = fresh_db.conn.query_one(sql("SELECT (SELECT count(*) FROM practice) AS practices, (SELECT count(*) FROM services) AS services")).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "practices").unwrap(), 1);
    assert_eq!(row.try_get::<i64>("", "services").unwrap(), 1);
    drop(app);
    fresh_db.conn.close().await.unwrap();
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn read_and_write_failures_return_503_without_success_or_partial_payment_changes() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&page(&app, &session, "payments").await, "csrf_token");
    assert_eq!(save(&app, &session, "payments", &csrf, &payments::payment_fields()).await.status(), 303);
    db.conn.execute_unprepared("CREATE FUNCTION deny_practice_write() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic private database detail'; END $$;
      CREATE TRIGGER deny_practice_write BEFORE UPDATE ON practice FOR EACH ROW EXECUTE FUNCTION deny_practice_write();").await.unwrap();
    let response = save(&app, &session, "payments", &csrf, &changed(payments::payment_fields(), "bank", "No guardar")).await;
    assert_eq!(response.status(), 503);
    assert!(!response.headers().contains_key("location"));
    let html = text(response).await;
    assert!(!html.contains("synthetic private"));
    assert!(!html.contains("Configuración guardada"));
    let saved = page(&app, &session, "payments").await;
    for (key, value) in payments::payment_fields() { assert_eq!(field(&saved, key), value); }
    db.conn.execute_unprepared("DROP TRIGGER deny_practice_write ON practice; ALTER TABLE practice RENAME TO unavailable_practice;").await.unwrap();
    let response = authenticated_get(&app, "/settings", &session).await;
    assert_eq!(response.status(), 503);
    assert!(!text(response).await.contains("unavailable_practice"));
    assert_eq!(save(&app, &session, "details", &csrf, &details::details_fields()).await.status(), 503);
    db.conn.execute_unprepared("ALTER TABLE unavailable_practice RENAME TO practice").await.unwrap();
    assert_eq!(save(&app, &session, "payments", &csrf, &changed(payments::payment_fields(), "bank", "Banco editado")).await.status(), 303);
    assert_eq!(field(&page(&app, &session, "payments").await, "bank"), "Banco editado");
  }).await;
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn database_constraints_keep_single_practice_valid_service_numbers_and_clabe_text() {
  with_database(|db| async move {
    db.run_migrations().await.unwrap();
    for statement in [
      "INSERT INTO practice (id) VALUES (2)",
      "UPDATE practice SET clabe = '١٢٣٤٥٦٧٨٩٠١٢٣٤٥٦٧٨'",
      "UPDATE practice SET grid_minutes = 0",
      "UPDATE practice SET receipt_window_minutes = 0",
      "UPDATE practice SET grace_minutes = -1",
      "UPDATE practice SET weekly_hours = '[]'::jsonb",
      "INSERT INTO services VALUES ('00000000-0000-0000-0000-000000000001', 'Service', 0, 100)",
      "INSERT INTO services VALUES ('00000000-0000-0000-0000-000000000001', 'Service', 1, -1)",
    ] {
      assert!(
        db.conn.execute(sql(statement)).await.is_err(),
        "constraint missing: {statement}"
      );
    }
  })
  .await;
}
