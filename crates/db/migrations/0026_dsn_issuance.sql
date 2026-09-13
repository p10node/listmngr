-- Current list epochs may be initialized; historical messages MUST NOT be bound.
ALTER TABLE mailing_lists ADD COLUMN delivery_incarnation TEXT NOT NULL DEFAULT '';
UPDATE mailing_lists SET delivery_incarnation=created_at;
ALTER TABLE preferences ADD COLUMN delivery_generation BIGINT NOT NULL DEFAULT 0;
ALTER TABLE delivery_recipients ADD COLUMN member_incarnation TEXT;
CREATE TABLE message_delivery_bindings (
    message_id TEXT PRIMARY KEY REFERENCES messages(id),
    list_id TEXT NOT NULL,
    list_incarnation TEXT NOT NULL
);
CREATE TABLE dsn_issuances (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES queue_jobs(id),
    message_id TEXT NOT NULL REFERENCES messages(id),
    recipient TEXT NOT NULL,
    attempt_token TEXT NOT NULL,
    claims TEXT NOT NULL,
    envid TEXT NOT NULL UNIQUE,
    issued_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    UNIQUE(job_id,recipient,attempt_token),
    CHECK(expires_at>issued_at)
);
