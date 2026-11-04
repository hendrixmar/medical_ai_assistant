ALTER TABLE appointment_jobs DROP CONSTRAINT appointment_jobs_kind_check;
ALTER TABLE appointment_jobs
  ADD CONSTRAINT appointment_jobs_kind_check
  CHECK (kind IN ('calendar_sync','calendar_cancel','reminders','review','expiry','conflict','stripe_link'));

CREATE TABLE stripe_connection (
  id integer PRIMARY KEY CHECK (id=1),
  account_id text,
  charges_enabled boolean NOT NULL DEFAULT false,
  details_submitted boolean NOT NULL DEFAULT false,
  checked_at bigint,
  notice text,
  created_at bigint NOT NULL,
  updated_at bigint NOT NULL
);

CREATE TABLE stripe_payment_links (
  appointment_id uuid NOT NULL REFERENCES appointments(id),
  revision bigint NOT NULL CHECK (revision > 0),
  account_id text NOT NULL,
  price_id text,
  link_id text UNIQUE,
  url text,
  amount_centavos bigint NOT NULL CHECK (amount_centavos >= 0),
  created_at bigint NOT NULL,
  deactivate_at bigint NOT NULL,
  deactivated_at bigint,
  paid_session_id text UNIQUE,
  paid_at bigint,
  next_check_at bigint,
  outcome text NOT NULL DEFAULT 'pending' CHECK (outcome IN ('pending','active','paid','late','duplicate','deactivating','deactivated')),
  PRIMARY KEY (appointment_id, revision),
  CHECK ((link_id IS NULL)=(url IS NULL)),
  CHECK ((paid_session_id IS NULL)=(paid_at IS NULL))
);
CREATE INDEX stripe_payment_links_poll_due ON stripe_payment_links(next_check_at, created_at)
  WHERE outcome IN ('active','paid','late','duplicate','deactivating') AND deactivated_at IS NULL;
