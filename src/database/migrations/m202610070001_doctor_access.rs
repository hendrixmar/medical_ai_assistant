use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
      .get_connection()
      .execute_unprepared(
        "
      CREATE TABLE doctor (
        id smallint PRIMARY KEY CHECK (id = 1),
        password_hash text NOT NULL,
        totp_ciphertext bytea NOT NULL,
        totp_nonce bytea NOT NULL CHECK (octet_length(totp_nonce) = 12),
        last_used_totp_step bigint NOT NULL
      );
      CREATE TABLE doctor_sessions (
        token_hash bytea PRIMARY KEY CHECK (octet_length(token_hash) = 32),
        csrf_token text NOT NULL,
        created_at bigint NOT NULL,
        expires_at bigint NOT NULL
      );
      CREATE INDEX doctor_sessions_expiry ON doctor_sessions (expires_at);
      CREATE TABLE doctor_login_challenges (
        token_hash bytea PRIMARY KEY CHECK (octet_length(token_hash) = 32),
        csrf_token text NOT NULL,
        expires_at bigint NOT NULL
      );
      CREATE INDEX doctor_challenges_expiry ON doctor_login_challenges (expires_at);
      CREATE TABLE doctor_login_limit (
        id smallint PRIMARY KEY CHECK (id = 1),
        window_start bigint NOT NULL,
        attempts integer NOT NULL,
        issue_start bigint NOT NULL,
        issued integer NOT NULL
      );
      INSERT INTO doctor_login_limit VALUES (1, 0, 0, 0, 0);
    ",
      )
      .await?;
    Ok(())
  }

  async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
    Err(DbErr::Custom(
      "Doctor access migration is irreversible; preserve authentication data".into(),
    ))
  }
}
