CREATE TABLE doctor_number_challenges (
  id uuid PRIMARY KEY,
  fingerprint bytea UNIQUE NOT NULL CHECK (octet_length(fingerprint)=32),
  ciphertext bytea,
  nonce bytea,
  practice_phone text NOT NULL CHECK (practice_phone ~ '^[0-9]{1,20}@c[.]us$'),
  issued_at bigint NOT NULL,
  expires_at bigint NOT NULL CHECK (expires_at=issued_at+600000),
  failures integer NOT NULL DEFAULT 0 CHECK (failures BETWEEN 0 AND 5),
  state text NOT NULL DEFAULT 'active' CHECK (state IN ('active','superseded','expired','exhausted','used')),
  CHECK ((state='active' AND ciphertext IS NOT NULL AND nonce IS NOT NULL AND octet_length(nonce)=12) OR (state<>'active' AND ciphertext IS NULL AND nonce IS NULL))
);
CREATE INDEX doctor_number_issuance ON doctor_number_challenges(issued_at);
CREATE TABLE doctor_number_state (
  id smallint PRIMARY KEY CHECK (id=1),
  challenge_id uuid REFERENCES doctor_number_challenges(id),
  verified_phone text CHECK (verified_phone ~ '^[0-9]{1,20}@c[.]us$'),
  original_target text,
  contact_id uuid REFERENCES contacts(id),
  inbox_message_id text REFERENCES inbox_messages(message_id),
  verified_at bigint,
  practice_phone text,
  CHECK ((verified_phone IS NULL AND original_target IS NULL AND contact_id IS NULL AND inbox_message_id IS NULL AND verified_at IS NULL AND practice_phone IS NULL)
    OR (verified_phone IS NOT NULL AND original_target IS NOT NULL AND contact_id IS NOT NULL AND inbox_message_id IS NOT NULL AND verified_at IS NOT NULL AND practice_phone IS NOT NULL))
);
INSERT INTO doctor_number_state(id) VALUES (1);
ALTER TABLE outbox_messages DROP CONSTRAINT outbox_messages_template_check;
ALTER TABLE outbox_messages ADD CHECK (template IN ('menu','unavailable','followup','resolved','audio','unsupported','doctor_verified','doctor_read_only','doctor_audio'));
