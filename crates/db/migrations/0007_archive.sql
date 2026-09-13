CREATE TABLE archive_messages (
 list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
 hash TEXT NOT NULL,
 thread TEXT NOT NULL,
 subject TEXT NOT NULL,
 body TEXT NOT NULL,
 raw_b64 TEXT NOT NULL,
 created_at BIGINT NOT NULL,
 PRIMARY KEY(list_id,hash)
);
CREATE INDEX archive_thread ON archive_messages(list_id,thread,created_at,hash);
CREATE INDEX archive_recent ON archive_messages(list_id,created_at,hash);
