-- Held/moderated messages and durable per-recipient outgoing progress.
-- Times are signed Unix milliseconds, consistent with 0001_mail_queue.sql.
CREATE TABLE held_messages (
  id TEXT PRIMARY KEY,
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE RESTRICT,
  message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE RESTRICT,
  sender TEXT NOT NULL,
  subject TEXT NOT NULL DEFAULT '',
  reason TEXT NOT NULL,
  hold_date BIGINT NOT NULL,
  disposition TEXT CHECK(disposition IN ('accepted','rejected','discarded')),
  moderator_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
  disposed_at BIGINT,
  CHECK((disposition IS NULL AND disposed_at IS NULL) OR (disposition IS NOT NULL AND disposed_at IS NOT NULL))
);
CREATE INDEX held_messages_list ON held_messages(list_id, disposition);
CREATE TABLE moderation_log (
  id TEXT PRIMARY KEY,
  held_id TEXT NOT NULL REFERENCES held_messages(id) ON DELETE RESTRICT,
  action TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  moderator_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
  at BIGINT NOT NULL
);
CREATE INDEX moderation_log_held ON moderation_log(held_id);
-- Snapshot of the accepted recipient set for one outgoing delivery job, so a
-- retried job never resends to a recipient already confirmed sent.
CREATE TABLE delivery_recipients (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES queue_jobs(id) ON DELETE CASCADE,
  email TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','sent','failed')),
  detail TEXT NOT NULL DEFAULT '',
  UNIQUE(job_id,email)
);
CREATE INDEX delivery_recipients_job ON delivery_recipients(job_id,status);
