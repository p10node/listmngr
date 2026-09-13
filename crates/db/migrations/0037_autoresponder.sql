-- Mailman's Automatic Responses: the list answers its owner, request and
-- posting addresses on its own, at most once per address per grace period.
ALTER TABLE mailing_lists ADD COLUMN autorespond_owner TEXT NOT NULL DEFAULT 'none'
  CHECK (autorespond_owner IN ('none','respond','respond_and_discard'));
ALTER TABLE mailing_lists ADD COLUMN autoresponse_owner_text TEXT NOT NULL DEFAULT '';
ALTER TABLE mailing_lists ADD COLUMN autorespond_postings TEXT NOT NULL DEFAULT 'none'
  CHECK (autorespond_postings IN ('none','respond','respond_and_discard'));
ALTER TABLE mailing_lists ADD COLUMN autoresponse_postings_text TEXT NOT NULL DEFAULT '';
ALTER TABLE mailing_lists ADD COLUMN autorespond_requests TEXT NOT NULL DEFAULT 'none'
  CHECK (autorespond_requests IN ('none','respond','respond_and_discard'));
ALTER TABLE mailing_lists ADD COLUMN autoresponse_request_text TEXT NOT NULL DEFAULT '';
-- Days; 0 answers every message.
ALTER TABLE mailing_lists ADD COLUMN autoresponse_grace_period INTEGER NOT NULL DEFAULT 90
  CHECK (autoresponse_grace_period BETWEEN 0 AND 3650);

-- One row per address and kind: the grace period is per recipient, as in
-- Mailman, and the rows are pruned when they fall outside it.
CREATE TABLE autoresponse_records (
    list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('owner','postings','requests')),
    responded_at BIGINT NOT NULL,
    PRIMARY KEY(list_id,email,kind)
);
CREATE INDEX autoresponse_expiry ON autoresponse_records(responded_at);
