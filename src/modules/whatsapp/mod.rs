pub mod client;
pub mod config;
mod http;
pub(crate) mod inbox;
pub mod jobs;
pub(crate) mod media;
pub mod messaging;
pub mod recovery;
pub(crate) mod takeover;
pub mod views;

pub(crate) async fn bound_transaction(
  tx: &sea_orm::DatabaseTransaction,
) -> Result<(), sea_orm::DbErr> {
  use sea_orm::ConnectionTrait;
  tx.execute_unprepared("SET LOCAL statement_timeout = '3s'; SET LOCAL lock_timeout = '2s'")
    .await?;
  Ok(())
}
pub use http::{dashboard_router, webhook_router};

#[cfg(test)]
mod tests;
