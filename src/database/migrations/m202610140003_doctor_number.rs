use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
      .get_connection()
      .execute_unprepared(include_str!("doctor_number.sql"))
      .await?;
    Ok(())
  }
  async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
    Err(DbErr::Custom(
      "Doctor Number proof and fingerprint history must be preserved".into(),
    ))
  }
}
