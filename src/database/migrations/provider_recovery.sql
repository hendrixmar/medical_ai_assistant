ALTER TABLE contacts ADD COLUMN takeover_until bigint;
ALTER TABLE inbox_messages
  ADD COLUMN intake_source text NOT NULL DEFAULT 'webhook'
    CHECK (intake_source IN ('webhook','provider_history'));

CREATE TABLE provider_recovery_state (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  provider_state text NOT NULL DEFAULT 'unknown'
    CHECK (provider_state IN ('unknown','unavailable','stopped','starting','qr','passkey_required','passkey_confirmation_required','working','failed')),
  disconnected_at bigint,
  recovery_id uuid,
  recovery_status text NOT NULL DEFAULT 'idle'
    CHECK (recovery_status IN ('idle','disconnected','running','complete','incomplete')),
  recovery_started_at bigint,
  recovery_until bigint,
  next_status_check_at bigint NOT NULL DEFAULT 0,
  claim_token uuid,
  claim_until bigint,
  last_error_code text
    CHECK (last_error_code IS NULL OR last_error_code IN ('provider_status_unavailable','history_page_unavailable','history_invalid_page','history_identity_missing','history_media_unavailable','history_offset_limit')),
  updated_at bigint NOT NULL DEFAULT 0,
  CHECK ((claim_token IS NULL) = (claim_until IS NULL))
);
INSERT INTO provider_recovery_state(singleton) VALUES (true);

CREATE TABLE provider_recovery_checkpoints (
  contact_id uuid NOT NULL REFERENCES contacts(id),
  chat_identity text NOT NULL CHECK (chat_identity ~ '^[0-9]{1,32}@(c[.]us|lid)$'),
  covered_through bigint NOT NULL,
  updated_at bigint NOT NULL,
  PRIMARY KEY (contact_id, chat_identity)
);

CREATE TABLE provider_recovery_targets (
  contact_id uuid NOT NULL REFERENCES contacts(id),
  chat_identity text NOT NULL CHECK (chat_identity ~ '^[0-9]{1,32}@(c[.]us|lid)$'),
  recovery_id uuid NOT NULL,
  range_start bigint NOT NULL,
  range_end bigint NOT NULL,
  pending_end bigint,
  included_expired_at bigint,
  page_offset bigint NOT NULL DEFAULT 0 CHECK (page_offset >= 0),
  media_gap boolean NOT NULL DEFAULT false,
  state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','complete','incomplete')),
  attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  available_at bigint NOT NULL,
  last_error_code text
    CHECK (last_error_code IS NULL OR last_error_code IN ('history_page_unavailable','history_invalid_page','history_identity_missing','history_media_unavailable','history_offset_limit')),
  updated_at bigint NOT NULL,
  PRIMARY KEY (contact_id, chat_identity),
  CHECK (range_end >= range_start),
  CHECK (pending_end IS NULL OR pending_end >= range_end)
);
CREATE INDEX provider_recovery_targets_due
  ON provider_recovery_targets (available_at, contact_id, chat_identity)
  WHERE state = 'pending';

CREATE TABLE phone_takeover_events (
  message_fingerprint bytea PRIMARY KEY CHECK (octet_length(message_fingerprint)=32),
  contact_id uuid REFERENCES contacts(id),
  attribution_status text NOT NULL CHECK (attribution_status IN ('attributed','unavailable')),
  unavailable_reason text
    CHECK (unavailable_reason IS NULL OR unavailable_reason IN ('operator_disabled','unknown_chat','wrong_chat','recipient_mismatch','identity_conflict','invalid_message')),
  server_timestamp bigint CHECK (server_timestamp BETWEEN 0 AND 253402300799),
  body text CHECK (body IS NULL OR octet_length(body)<=1048576),
  text_deleted_at bigint,
  created_at bigint NOT NULL,
  CHECK ((attribution_status='attributed' AND contact_id IS NOT NULL AND unavailable_reason IS NULL AND server_timestamp IS NOT NULL)
      OR (attribution_status='unavailable' AND contact_id IS NULL AND unavailable_reason IS NOT NULL AND body IS NULL AND text_deleted_at IS NULL))
);
CREATE INDEX phone_takeover_timeline
  ON phone_takeover_events(contact_id,server_timestamp,message_fingerprint)
  WHERE attribution_status='attributed';
