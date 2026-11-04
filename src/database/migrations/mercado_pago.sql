ALTER TABLE appointment_jobs DROP CONSTRAINT appointment_jobs_kind_check;
ALTER TABLE appointment_jobs
  ADD CONSTRAINT appointment_jobs_kind_check
  CHECK (kind IN ('calendar_sync','calendar_cancel','reminders','review','expiry','conflict','mp_preference','mp_refund'));

CREATE TABLE mercado_pago_account (
  id integer PRIMARY KEY CHECK (id=1),
  user_id bigint,
  checked_at bigint NOT NULL,
  notice text,
  created_at bigint NOT NULL,
  updated_at bigint NOT NULL
);

CREATE TABLE mercado_pago_preferences (
  appointment_id uuid NOT NULL REFERENCES appointments(id),
  revision bigint NOT NULL CHECK (revision > 0),
  external_reference text NOT NULL UNIQUE,
  preference_id text UNIQUE,
  url text,
  amount_centavos bigint NOT NULL CHECK (amount_centavos > 0),
  expires_at bigint NOT NULL,
  closed_at bigint,
  next_check_at bigint,
  outcome text NOT NULL DEFAULT 'pending' CHECK (outcome IN ('pending','active','closing','closed')),
  created_at bigint NOT NULL,
  PRIMARY KEY (appointment_id, revision),
  CHECK ((preference_id IS NULL)=(url IS NULL))
);
CREATE INDEX mercado_pago_preferences_poll_due ON mercado_pago_preferences(next_check_at)
  WHERE preference_id IS NOT NULL AND next_check_at IS NOT NULL;

CREATE TABLE mercado_pago_payments (
  payment_id bigint PRIMARY KEY CHECK (payment_id > 0),
  appointment_id uuid NOT NULL,
  revision bigint NOT NULL,
  amount_centavos bigint NOT NULL,
  outcome text NOT NULL CHECK (outcome IN ('confirmed','late','duplicate','mismatch')),
  provider_status text NOT NULL,
  refund_state text NOT NULL DEFAULT 'none' CHECK (refund_state IN ('none','requested','refunded','failed')),
  refund_id bigint,
  next_check_at bigint,
  first_seen_at bigint NOT NULL,
  updated_at bigint NOT NULL,
  FOREIGN KEY (appointment_id, revision) REFERENCES mercado_pago_preferences(appointment_id, revision)
);
CREATE INDEX mercado_pago_payments_check_due ON mercado_pago_payments(next_check_at)
  WHERE next_check_at IS NOT NULL;
CREATE INDEX mercado_pago_payments_appointment ON mercado_pago_payments(appointment_id);
