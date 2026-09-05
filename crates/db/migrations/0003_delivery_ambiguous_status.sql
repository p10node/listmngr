-- Adds a durable, quarantined `ambiguous` recipient status: a post-DATA
-- connection loss where the relay may or may not have accepted the message.
-- Ambiguous recipients are excluded from `pending_recipients` (so they are
-- never auto-retried, which could duplicate an already-accepted delivery)
-- but remain distinct from `sent`/`failed` for operator inspection/recovery.
-- SQLite has no `ALTER TABLE ... DROP CONSTRAINT`, so the CHECK constraint is
-- widened by rebuilding the table; this is portable to PostgreSQL too.
CREATE TABLE delivery_recipients_new (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES queue_jobs(id) ON DELETE CASCADE,
  email TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','sent','failed','ambiguous')),
  detail TEXT NOT NULL DEFAULT '',
  UNIQUE(job_id,email)
);
INSERT INTO delivery_recipients_new(id,job_id,email,status,detail)
  SELECT id,job_id,email,status,detail FROM delivery_recipients;
DROP TABLE delivery_recipients;
ALTER TABLE delivery_recipients_new RENAME TO delivery_recipients;
CREATE INDEX delivery_recipients_job ON delivery_recipients(job_id,status);
