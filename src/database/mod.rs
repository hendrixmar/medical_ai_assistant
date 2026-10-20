pub mod migrations;

use anyhow::Result;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use std::time::Duration;
use tracing::info;

use crate::common::config::Config;
use crate::database::migrations::Migrator;

#[derive(Clone)]
pub struct Db {
  pub conn: DatabaseConnection,
}

impl Db {
  // We create a single connection pool for Sea-ORM that is shared across the entire application.
  // This prevents the need to open a new connection for every API call, which would be wasteful.
  async fn connect(
    dsn: &str,
    timeout_seconds: u64,
    max_connections: u32,
  ) -> Result<Self, sea_orm::DbErr> {
    let mut opt = ConnectOptions::new(dsn.to_owned());
    opt
      .connect_timeout(Duration::from_secs(timeout_seconds))
      .acquire_timeout(Duration::from_secs(timeout_seconds))
      .idle_timeout(Duration::from_secs(600))
      .max_lifetime(Duration::from_secs(1800))
      .max_connections(max_connections)
      .min_connections(1)
      .sqlx_logging(false);
    let conn = Database::connect(opt).await?;
    Ok(Self { conn })
  }

  pub async fn new(cfg: &Config) -> Result<Self, sea_orm::DbErr> {
    info!(
      max_connections = cfg.db_pool_max_size,
      timeout_seconds = cfg.db_timeout,
      "Database connection configured"
    );
    info!("Connecting to database...");
    Self::connect(&cfg.db_dsn, cfg.db_timeout, cfg.db_pool_max_size).await
  }

  /// Maintenance-only connection: one connection, 5-second timeouts, no migrations or workers.
  /// Connect errors are mapped to a value-free message so a DSN can never leak.
  pub async fn for_maintenance(dsn: &str) -> Result<Self, sea_orm::DbErr> {
    Self::connect(dsn, 5, 1)
      .await
      .map_err(|_| sea_orm::DbErr::Custom("maintenance database connection failed".into()))
  }

  pub async fn run_migrations(&self) -> Result<(), sea_orm::DbErr> {
    // This integrates database migrations into the application binary to ensure the database
    // is properly migrated during startup.
    Migrator::up(&self.conn, None).await?;
    Ok(())
  }
}
