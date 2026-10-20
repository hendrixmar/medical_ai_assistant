use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
      .get_connection()
      .execute_unprepared(include_str!("doctor_number_fingerprint_key.sql"))
      .await?;
    Ok(())
  }
  async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
    Err(DbErr::Custom(
      "Doctor Number fingerprint key and fingerprint history must be preserved".into(),
    ))
  }
}
