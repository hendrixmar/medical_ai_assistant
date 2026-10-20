CREATE TABLE contacts (
  id uuid PRIMARY KEY,
  created_at bigint NOT NULL,
  privacy_first_sent_at bigint,
  identity_conflict boolean NOT NULL DEFAULT false,
  claim_token uuid,
  claim_until bigint,
  dispatch_until bigint,
  CHECK ((claim_token IS NULL) = (claim_until IS NULL))
);
ALTER TABLE inbox_messages ADD COLUMN intake_order bigserial UNIQUE;
ALTER TABLE inbox_messages ADD COLUMN contact_id uuid REFERENCES contacts(id);
ALTER TABLE inbox_messages ADD COLUMN processed_at bigint;
ALTER TABLE inbox_messages ADD COLUMN conversation_ignored boolean NOT NULL DEFAULT false;
CREATE TABLE contact_aliases (
  jid text PRIMARY KEY CHECK (jid ~ '^[0-9]{1,32}@(c[.]us|lid)$'),
  contact_id uuid NOT NULL REFERENCES contacts(id),
  inbox_message_id text NOT NULL REFERENCES inbox_messages(message_id)
);
CREATE INDEX contact_aliases_contact ON contact_aliases(contact_id);
CREATE TABLE followup_requests (
  id uuid PRIMARY KEY,
  contact_id uuid NOT NULL REFERENCES contacts(id),
  inbox_message_id text NOT NULL REFERENCES inbox_messages(message_id),
  original_target text NOT NULL,
  created_at bigint NOT NULL,
  resolved_at bigint
);
CREATE UNIQUE INDEX followup_one_open ON followup_requests(contact_id) WHERE resolved_at IS NULL;
CREATE TABLE outbox_messages (
  id uuid PRIMARY KEY,
  contact_id uuid NOT NULL REFERENCES contacts(id),
  wrote_first_inbox_id text NOT NULL REFERENCES inbox_messages(message_id),
  original_target text NOT NULL,
  reply_inbox_id text UNIQUE REFERENCES inbox_messages(message_id),
  resolved_followup_id uuid UNIQUE REFERENCES followup_requests(id),
  sequence bigserial UNIQUE,
  template text NOT NULL CHECK (template IN ('menu','unavailable','followup','resolved','audio','unsupported')),
  template_version integer NOT NULL DEFAULT 1 CHECK (template_version=1),
  rendered_text text NOT NULL,
  privacy_prefix boolean NOT NULL,
  created_at bigint NOT NULL,
  state text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued','typing','uncertain','sent','failed','cancelled')),
  typing_delay_ms integer NOT NULL CHECK (typing_delay_ms BETWEEN 2000 AND 6000),
  typing_due_at bigint,
  eligible_at bigint NOT NULL,
  rate_deferred_until bigint,
  preflight_failures integer NOT NULL DEFAULT 0 CHECK (preflight_failures BETWEEN 0 AND 5),
  last_error_code text CHECK (last_error_code IN ('preflight_unavailable','invalid_incoming_id','provider_rejected')),
  raw_provider_id text UNIQUE,
  returned_id text,
  dispatch_committed_at bigint,
  dispatch_deadline bigint,
  positive_ack integer NOT NULL DEFAULT 0 CHECK (positive_ack BETWEEN 0 AND 4),
  ack_error boolean NOT NULL DEFAULT false,
  CHECK ((reply_inbox_id IS NULL) <> (resolved_followup_id IS NULL)),
  CHECK ((dispatch_committed_at IS NULL) = (dispatch_deadline IS NULL)),
  CHECK (dispatch_committed_at IS NULL OR raw_provider_id IS NOT NULL),
  CHECK (state NOT IN ('sent','uncertain') OR dispatch_committed_at IS NOT NULL)
);
CREATE INDEX outbox_contact_order ON outbox_messages(contact_id, sequence);
CREATE INDEX outbox_budget ON outbox_messages(contact_id, dispatch_committed_at) WHERE dispatch_committed_at IS NOT NULL;
CREATE TABLE outgoing_ack_evidence (
  id uuid PRIMARY KEY,
  envelope_id text UNIQUE NOT NULL,
  session text NOT NULL CHECK (session='default'),
  raw_provider_id text NOT NULL,
  exact_id text NOT NULL,
  peer text NOT NULL,
  from_me boolean NOT NULL CHECK (from_me),
  ack integer NOT NULL CHECK (ack BETWEEN -1 AND 4),
  arrived_at bigint NOT NULL,
  matched_outbox_id uuid REFERENCES outbox_messages(id),
  UNIQUE(session,exact_id,ack)
);
CREATE INDEX ack_unmatched ON outgoing_ack_evidence(raw_provider_id,arrived_at) WHERE matched_outbox_id IS NULL;
ALTER TABLE processing_jobs DROP CONSTRAINT processing_jobs_pkey;
ALTER TABLE processing_jobs ADD COLUMN id uuid NOT NULL DEFAULT gen_random_uuid() PRIMARY KEY;
ALTER TABLE processing_jobs ALTER COLUMN inbox_message_id DROP NOT NULL;
ALTER TABLE processing_jobs ADD COLUMN outbox_message_id uuid REFERENCES outbox_messages(id);
ALTER TABLE processing_jobs DROP CONSTRAINT processing_jobs_kind_check;
ALTER TABLE processing_jobs DROP CONSTRAINT processing_jobs_last_error_code_check;
ALTER TABLE processing_jobs ADD CHECK (kind IN ('prepare_inbox','process_conversation','send_outbox'));
ALTER TABLE processing_jobs ADD CHECK (
  (kind IN ('prepare_inbox','process_conversation') AND inbox_message_id IS NOT NULL AND outbox_message_id IS NULL)
  OR (kind='send_outbox' AND inbox_message_id IS NULL AND outbox_message_id IS NOT NULL)
);
ALTER TABLE processing_jobs ADD CHECK (last_error_code IN ('invalid_stored_message','preparation_database_error','conversation_database_error','preflight_unavailable','invalid_incoming_id'));
ALTER TABLE processing_jobs ADD UNIQUE(inbox_message_id,kind);
ALTER TABLE processing_jobs ADD UNIQUE(outbox_message_id,kind);
INSERT INTO processing_jobs(inbox_message_id,kind,available_at)
  SELECT message_id,'process_conversation',prepared_at FROM inbox_messages WHERE preparation_state='ready_for_conversation';
