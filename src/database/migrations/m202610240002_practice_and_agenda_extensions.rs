use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
      .get_connection()
      .execute_unprepared(include_str!("practice_and_agenda_extensions.sql"))
      .await?;
    Ok(())
  }

  async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
    Err(DbErr::Custom(
      "Practice reminder and knowledge configuration must be preserved".into(),
    ))
  }
}
