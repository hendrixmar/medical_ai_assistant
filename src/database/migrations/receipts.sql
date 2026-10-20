CREATE TABLE receipt_intake_state (
  inbox_message_id text PRIMARY KEY REFERENCES inbox_messages(message_id),
  state text NOT NULL CHECK (state IN ('pending','complete')),
  attempts integer NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 5),
  available_at bigint NOT NULL DEFAULT 0 CHECK (available_at >= 0),
  last_error_code text CHECK (last_error_code IS NULL OR last_error_code IN ('media_unavailable','invalid_media')),
  sent_at bigint NOT NULL CHECK (sent_at BETWEEN 0 AND 253402300799000),
  arrived_at bigint NOT NULL CHECK (arrived_at >= 0),
  updated_at bigint NOT NULL CHECK (updated_at >= arrived_at)
);
CREATE INDEX receipt_intake_pending ON receipt_intake_state (arrived_at, inbox_message_id) WHERE state='pending';

CREATE TABLE receipts (
  id uuid PRIMARY KEY,
  contact_id uuid NOT NULL REFERENCES contacts(id),
  inbox_message_id text NOT NULL UNIQUE REFERENCES inbox_messages(message_id),
  appointment_id uuid REFERENCES appointments(id),
  mime text NOT NULL CHECK (mime IN ('image/jpeg','image/png','image/webp','application/pdf')),
  bytes bytea CHECK (bytes IS NULL OR octet_length(bytes) BETWEEN 1 AND 10485760),
  sent_at bigint NOT NULL CHECK (sent_at >= 0),
  arrived_at bigint NOT NULL CHECK (arrived_at >= 0),
  timely boolean,
  late boolean,
  reviewed_at bigint,
  review_outcome text CHECK (review_outcome IN ('verified','rejected','new_receipt','cancelled','late_verified_recovery','late_verified_refund','late_clarification')),
  clarification_requested_at bigint,
  deleted_at bigint,
  created_at bigint NOT NULL CHECK (created_at >= 0),
  CHECK (deleted_at IS NULL OR bytes IS NULL),
  CHECK ((reviewed_at IS NULL) = (review_outcome IS NULL))
);
CREATE INDEX receipts_pending_appointment ON receipts (appointment_id, created_at) WHERE reviewed_at IS NULL AND deleted_at IS NULL;
CREATE INDEX receipts_pending_contact ON receipts (contact_id, created_at) WHERE appointment_id IS NULL AND deleted_at IS NULL;
CREATE TABLE receipt_candidates (
  receipt_id uuid NOT NULL REFERENCES receipts(id) ON DELETE CASCADE,
  appointment_id uuid NOT NULL REFERENCES appointments(id),
  PRIMARY KEY (receipt_id, appointment_id)
);
CREATE INDEX receipt_candidates_appointment ON receipt_candidates (appointment_id, receipt_id);
