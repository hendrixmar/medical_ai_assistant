use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager.get_connection().execute_unprepared("
      CREATE TABLE practice (
        id smallint PRIMARY KEY CHECK (id = 1),
        name text CHECK (char_length(name) BETWEEN 1 AND 120),
        location text CHECK (char_length(location) BETWEEN 1 AND 500),
        timezone text,
        weekly_hours jsonb CHECK (jsonb_typeof(weekly_hours) = 'array' AND jsonb_array_length(weekly_hours) = 7),
        buffer_before_minutes integer CHECK (buffer_before_minutes >= 0),
        buffer_after_minutes integer CHECK (buffer_after_minutes >= 0),
        minimum_notice_minutes integer CHECK (minimum_notice_minutes >= 0),
        grid_minutes integer CHECK (grid_minutes > 0),
        horizon_days integer CHECK (horizon_days > 0),
        cancellation_policy text CHECK (char_length(cancellation_policy) BETWEEN 1 AND 4000),
        bank text CHECK (char_length(bank) BETWEEN 1 AND 120),
        beneficiary text CHECK (char_length(beneficiary) BETWEEN 1 AND 200),
        clabe text CHECK (clabe ~ '^[0-9]{18}$'),
        receipt_window_minutes integer CHECK (receipt_window_minutes > 0),
        review_margin_minutes integer CHECK (review_margin_minutes >= 0),
        grace_minutes integer CHECK (grace_minutes >= 0)
      );
      INSERT INTO practice (id, grid_minutes, horizon_days, review_margin_minutes, grace_minutes)
        VALUES (1, 30, 30, 180, 10);
      CREATE TABLE services (
        id uuid PRIMARY KEY,
        name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 120),
        duration_minutes integer NOT NULL CHECK (duration_minutes > 0),
        price_centavos bigint NOT NULL CHECK (price_centavos >= 0)
      );
    ").await?;
    Ok(())
  }

  async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
    Err(DbErr::Custom(
      "Practice configuration migration is irreversible; preserve saved decisions".into(),
    ))
  }
}
