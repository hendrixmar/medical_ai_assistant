SELECT pg_advisory_xact_lock(hashtextextended('medical-assistant:btree_gist',0));
CREATE EXTENSION IF NOT EXISTS btree_gist WITH SCHEMA public;

ALTER TABLE practice
  ADD COLUMN reminder_2h_enabled boolean NOT NULL DEFAULT false,
  ADD COLUMN review_interval_minutes integer NOT NULL DEFAULT 240 CHECK (review_interval_minutes > 0),
  ADD COLUMN urgent_notice_minutes integer NOT NULL DEFAULT 120 CHECK (urgent_notice_minutes > 0);

CREATE TABLE appointments (
  id uuid PRIMARY KEY,
  contact_id uuid NOT NULL REFERENCES contacts(id),
  patient_name text NOT NULL,
  service_id uuid NOT NULL,
  service_name text NOT NULL,
  duration_minutes integer NOT NULL CHECK (duration_minutes > 0),
  price_centavos bigint NOT NULL CHECK (price_centavos >= 0),
  format text NOT NULL CHECK (format IN ('in_person','video')),
  starts_at bigint NOT NULL,
  ends_at bigint NOT NULL CHECK (ends_at > starts_at),
  timezone text NOT NULL,
  location text NOT NULL,
  buffer_before_minutes integer NOT NULL CHECK (buffer_before_minutes >= 0),
  buffer_after_minutes integer NOT NULL CHECK (buffer_after_minutes >= 0),
  status text NOT NULL CHECK (status IN ('requested','awaiting_payment','confirmed','cancelled','expired')),
  payment text NOT NULL CHECK (payment IN ('not_required','awaiting_receipt','under_review','verified','rejected')),
  calendar_sync text NOT NULL CHECK (calendar_sync IN ('not_yet','pending','synced','failed')),
  bank text,
  beneficiary text,
  clabe text,
  payment_reference text UNIQUE,
  receipt_deadline bigint,
  booking_calendar text,
  google_generation bigint,
  google_selection_version bigint,
  event_id text UNIQUE CHECK (event_id IS NULL OR event_id ~ '^[0-9a-f]{32}$'),
  event_etag text,
  meet_url text,
  revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
  attendance_confirmed_at bigint,
  cancelled_at bigint,
  expired_at bigint,
  created_at bigint NOT NULL,
  updated_at bigint NOT NULL
);
CREATE INDEX appointments_contact_time ON appointments(contact_id,starts_at);
CREATE INDEX appointments_due_sync ON appointments(updated_at) WHERE calendar_sync IN ('pending','failed');

CREATE TABLE appointment_holds (
  id uuid PRIMARY KEY,
  appointment_id uuid NOT NULL REFERENCES appointments(id),
  starts_at bigint NOT NULL,
  ends_at bigint NOT NULL CHECK (ends_at > starts_at),
  active boolean NOT NULL DEFAULT true,
  expires_at bigint
);
CREATE INDEX appointment_holds_active_by_appointment ON appointment_holds(appointment_id) WHERE active;
ALTER TABLE appointment_holds
  ADD CONSTRAINT appointment_holds_no_cross_appointment_overlap
  EXCLUDE USING gist (
    int8range(starts_at,ends_at,'[)') WITH &&,
    appointment_id WITH <>
  ) WHERE (active);

CREATE TABLE appointment_history (
  id uuid PRIMARY KEY,
  appointment_id uuid NOT NULL REFERENCES appointments(id),
  actor text NOT NULL CHECK (actor IN ('Contact','Doctor','System')),
  kind text NOT NULL,
  created_at bigint NOT NULL,
  data jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(data)='object')
);
CREATE INDEX appointment_history_order ON appointment_history(appointment_id,created_at,id);

CREATE TABLE attention_items (
  id uuid PRIMARY KEY,
  appointment_id uuid REFERENCES appointments(id),
  contact_id uuid REFERENCES contacts(id),
  kind text NOT NULL,
  created_at bigint NOT NULL,
  resolved_at bigint,
  dedupe_key text NOT NULL UNIQUE
);
CREATE INDEX attention_unresolved ON attention_items(created_at,id) WHERE resolved_at IS NULL;
CREATE UNIQUE INDEX attention_single_unresolved ON attention_items(kind,appointment_id,contact_id) NULLS NOT DISTINCT WHERE resolved_at IS NULL;

CREATE TABLE appointment_jobs (
  id uuid PRIMARY KEY,
  appointment_id uuid NOT NULL REFERENCES appointments(id),
  kind text NOT NULL CHECK (kind IN ('calendar_sync','calendar_cancel','reminders','review','expiry','conflict')),
  due_at bigint NOT NULL,
  claim uuid,
  claim_until bigint,
  attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  finished_at bigint,
  last_error text,
  dedupe_key text NOT NULL UNIQUE,
  CHECK ((claim IS NULL)=(claim_until IS NULL))
);
CREATE INDEX appointment_jobs_due ON appointment_jobs(due_at,id) WHERE finished_at IS NULL;

CREATE TABLE conversation_state (
  contact_id uuid PRIMARY KEY REFERENCES contacts(id),
  stage text NOT NULL,
  data jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(data)='object'),
  updated_at bigint NOT NULL
);

CREATE TABLE message_appointments (
  inbox_message_id text NOT NULL REFERENCES inbox_messages(message_id) ON DELETE CASCADE,
  appointment_id uuid NOT NULL REFERENCES appointments(id),
  PRIMARY KEY (inbox_message_id,appointment_id)
);
