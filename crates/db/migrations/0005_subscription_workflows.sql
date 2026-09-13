-- Confirm-only public subscription workflows. Raw secrets exist only in the
-- delivery spool, never in workflow state or audit. The spool is sensitive data.
CREATE TABLE subscription_workflows (
    id TEXT PRIMARY KEY,
    list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('join','leave')),
    token_hash TEXT NOT NULL UNIQUE,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    consumed INTEGER NOT NULL DEFAULT 0 CHECK(consumed IN (0,1))
);
CREATE INDEX subscription_expiry ON subscription_workflows(expires_at,id);
CREATE INDEX subscription_address_rate ON subscription_workflows(list_id,email,created_at);
-- One bounded row, also serializes public workflow writes across processes.
CREATE TABLE subscription_rate (id INTEGER PRIMARY KEY, window_start BIGINT NOT NULL, requests BIGINT NOT NULL);
INSERT INTO subscription_rate(id,window_start,requests) VALUES(1,0,0);
