ALTER TABLE practice
  ADD COLUMN reminder_72h_enabled boolean NOT NULL DEFAULT false,
  ADD COLUMN reminder_24h_enabled boolean NOT NULL DEFAULT true;

CREATE TABLE knowledge_entries (
  id uuid PRIMARY KEY,
  topic text NOT NULL CHECK (char_length(btrim(topic)) BETWEEN 1 AND 120),
  question text NOT NULL CHECK (char_length(btrim(question)) BETWEEN 1 AND 300),
  answer text NOT NULL CHECK (char_length(btrim(answer)) BETWEEN 1 AND 2000),
  enabled boolean NOT NULL DEFAULT true,
  updated_at bigint NOT NULL
);
CREATE INDEX knowledge_entries_enabled_idx ON knowledge_entries (enabled, id);
