-- Webhooks: where the site posts what its audit log records, and the
-- deliveries it owes them. A webhook subscribes to audit actions (`*`,
-- `member.*`, `list.config`) for the whole site or one list; a delivery is
-- written in the same transaction as the audit event it carries and is
-- posted later by the webhook runner. The secret is never stored: only its
-- hash (to show a fingerprint) and the salt it is derived from.
CREATE TABLE webhooks (
  id TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  events TEXT NOT NULL,
  list_id TEXT REFERENCES mailing_lists(list_id) ON DELETE RESTRICT,
  enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  secret_hash TEXT NOT NULL,
  secret_salt TEXT NOT NULL,
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);
CREATE INDEX webhooks_enabled ON webhooks(enabled);
CREATE TABLE webhook_deliveries (
  id TEXT PRIMARY KEY,
  webhook_id TEXT NOT NULL REFERENCES webhooks(id) ON DELETE RESTRICT,
  event TEXT NOT NULL,
  list_id TEXT,
  payload TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','delivered','failed')),
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at BIGINT NOT NULL,
  leased_until BIGINT,
  last_status INTEGER,
  last_error TEXT,
  created_at BIGINT NOT NULL,
  finished_at BIGINT
);
CREATE INDEX webhook_deliveries_due ON webhook_deliveries(state, next_attempt_at);
CREATE INDEX webhook_deliveries_webhook ON webhook_deliveries(webhook_id, created_at);
