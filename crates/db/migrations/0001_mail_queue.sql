-- BYTEA is PostgreSQL's binary type; SQLite accepts it with bound blob values.
-- Times are signed Unix milliseconds. External Message-ID is not a dedup key.
CREATE TABLE message_blobs (
  store_key TEXT PRIMARY KEY,
  raw BYTEA NOT NULL
);
CREATE TABLE messages (
  id TEXT PRIMARY KEY,
  store_key TEXT NOT NULL REFERENCES message_blobs(store_key) ON DELETE RESTRICT,
  external_id TEXT NOT NULL,
  context TEXT NOT NULL,
  created_at BIGINT NOT NULL
);
CREATE INDEX messages_external_id ON messages(external_id, context);
CREATE INDEX messages_store_key ON messages(store_key);
CREATE TABLE queue_jobs (
  id TEXT PRIMARY KEY,
  message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE RESTRICT,
  queue TEXT NOT NULL CHECK(queue IN ('in','pipeline','out','retry','bounces','command','virgin','archive','digest','nntp','shunt','bad')),
  state TEXT NOT NULL DEFAULT 'ready' CHECK(state IN ('ready','leased','done','shunted')),
  attempts BIGINT NOT NULL DEFAULT 0 CHECK(attempts >= 0),
  max_attempts BIGINT NOT NULL CHECK(max_attempts > 0),
  run_after BIGINT NOT NULL,
  locked_by TEXT,
  lease_token TEXT,
  lease_until BIGINT,
  last_error TEXT NOT NULL DEFAULT '',
  CHECK((state = 'leased' AND locked_by IS NOT NULL AND lease_token IS NOT NULL AND lease_until IS NOT NULL)
     OR (state <> 'leased' AND locked_by IS NULL AND lease_token IS NULL AND lease_until IS NULL))
);
CREATE INDEX queue_jobs_due ON queue_jobs(queue, state, run_after, lease_until);
CREATE INDEX queue_jobs_message ON queue_jobs(message_id);
