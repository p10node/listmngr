-- Cooked posts and immutable issue payloads are owned by the list.
CREATE TABLE digest_issues (
 id TEXT PRIMARY KEY,
 list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
 volume BIGINT NOT NULL,
 number BIGINT NOT NULL,
 created_at BIGINT NOT NULL,
 UNIQUE(list_id,volume,number)
);
CREATE TABLE digest_posts (
 id TEXT PRIMARY KEY,
 list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
 raw BYTEA NOT NULL,
 recipients TEXT NOT NULL,
 accepted_at BIGINT NOT NULL,
 issue_id TEXT REFERENCES digest_issues(id) ON DELETE CASCADE
);
CREATE INDEX digest_pending ON digest_posts(list_id,issue_id,accepted_at,id);
CREATE TABLE digest_deliveries (
 job_id TEXT PRIMARY KEY REFERENCES queue_jobs(id) ON DELETE CASCADE,
 issue_id TEXT NOT NULL REFERENCES digest_issues(id) ON DELETE CASCADE,
 mode TEXT NOT NULL CHECK(mode IN ('plaintext_digests','mime_digests','summary_digests'))
);
