ALTER TABLE outbox_messages ADD COLUMN appointment_id uuid REFERENCES appointments(id);
ALTER TABLE outbox_messages ADD COLUMN dedupe_key text UNIQUE;
ALTER TABLE outbox_messages ADD COLUMN expected_revision bigint;
ALTER TABLE outbox_messages ADD COLUMN purpose text NOT NULL DEFAULT 'conversation'
  CHECK (purpose IN ('conversation','status','reminder','doctor_notification','payment_details'));
UPDATE outbox_messages SET purpose='status' WHERE template='resolved';
ALTER TABLE outbox_messages ADD COLUMN text_deleted_at bigint;
ALTER TABLE inbox_messages ADD COLUMN text_deleted_at bigint;
ALTER TABLE outbox_messages DROP CONSTRAINT outbox_messages_template_check;
ALTER TABLE outbox_messages ADD CONSTRAINT outbox_messages_template_check
  CHECK (template IN ('menu','unavailable','followup','resolved','audio','unsupported','doctor_verified','doctor_read_only','doctor_audio','appointment'));
DO $$
DECLARE item record;
BEGIN
  FOR item IN SELECT conname FROM pg_constraint
    WHERE conrelid='outbox_messages'::regclass AND contype='c'
      AND pg_get_constraintdef(oid) LIKE '%reply_inbox_id%'
      AND pg_get_constraintdef(oid) LIKE '%resolved_followup_id%'
  LOOP
    EXECUTE format('ALTER TABLE outbox_messages DROP CONSTRAINT %I', item.conname);
  END LOOP;
END $$;
ALTER TABLE outbox_messages ADD CONSTRAINT outbox_source CHECK (
  ((reply_inbox_id IS NULL) <> (resolved_followup_id IS NULL))
  OR (reply_inbox_id IS NULL AND resolved_followup_id IS NULL AND dedupe_key IS NOT NULL)
);
ALTER TABLE outbox_messages ADD CONSTRAINT outbox_revision_target
  CHECK (expected_revision IS NULL OR appointment_id IS NOT NULL);
CREATE INDEX outbox_appointment ON outbox_messages(appointment_id) WHERE appointment_id IS NOT NULL;
ALTER TABLE processing_jobs ADD COLUMN claim_token uuid;
ALTER TABLE processing_jobs ADD COLUMN claim_until bigint;
ALTER TABLE processing_jobs ADD CONSTRAINT processing_claim_complete
  CHECK ((claim_token IS NULL) = (claim_until IS NULL));
