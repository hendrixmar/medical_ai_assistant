use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
  async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager.get_connection().execute_unprepared("
      CREATE TABLE inbox_messages (
        message_id text PRIMARY KEY CHECK (octet_length(message_id) BETWEEN 1 AND 512),
        original_chat_identity text NOT NULL CHECK (original_chat_identity ~ '^[0-9]{1,32}@(c[.]us|lid)$'),
        resolved_phone_identity text CHECK (resolved_phone_identity ~ '^[0-9]{1,20}@c[.]us$'),
        server_timestamp bigint NOT NULL CHECK (server_timestamp BETWEEN 0 AND 253402300799),
        arrived_at timestamptz NOT NULL,
        raw_envelope bytea NOT NULL CHECK (octet_length(raw_envelope) BETWEEN 1 AND 2097152),
        preparation_state text NOT NULL DEFAULT 'accepted' CHECK (preparation_state IN ('accepted', 'ready_for_conversation')),
        message_kind text CHECK (message_kind IN ('text', 'media', 'other')),
        body text,
        media_metadata jsonb CHECK (jsonb_typeof(media_metadata) = 'object'),
        prepared_at timestamptz,
        CHECK ((preparation_state = 'accepted' AND prepared_at IS NULL AND message_kind IS NULL)
          OR (preparation_state = 'ready_for_conversation' AND prepared_at IS NOT NULL AND message_kind IS NOT NULL))
      );
      CREATE TABLE processing_jobs (
        inbox_message_id text NOT NULL REFERENCES inbox_messages(message_id),
        kind text NOT NULL CHECK (kind = 'prepare_inbox'),
        status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'succeeded', 'failed')),
        attempts integer NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 5),
        available_at timestamptz NOT NULL,
        last_error_code text CHECK (last_error_code IN ('invalid_stored_message', 'preparation_database_error')),
        finished_at timestamptz,
        PRIMARY KEY (inbox_message_id, kind),
        CHECK ((status = 'pending' AND finished_at IS NULL) OR (status <> 'pending' AND finished_at IS NOT NULL))
      );
      CREATE INDEX processing_jobs_due ON processing_jobs (available_at, inbox_message_id) WHERE status = 'pending';
    ").await?;
    Ok(())
  }
  async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
    Err(DbErr::Custom(
      "Inbox migration is irreversible; preserve received messages".into(),
    ))
  }
}
