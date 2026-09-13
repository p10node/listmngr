-- List-owned metadata, deliberately independent of spool retention.
CREATE TABLE bounce_events (
 id TEXT PRIMARY KEY NOT NULL,
 list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
 recipient TEXT NOT NULL,
 job_id TEXT NOT NULL,
 message_id TEXT NOT NULL,
 created_at BIGINT NOT NULL,
 source TEXT NOT NULL CHECK(source='smtp_permanent_failure'),
 context TEXT NOT NULL CHECK(context='normal'),
 processed BIGINT NOT NULL DEFAULT 0 CHECK(processed IN (0,1)),
 UNIQUE(job_id,recipient)
);
CREATE INDEX bounce_events_list_page ON bounce_events(list_id,created_at,id);
