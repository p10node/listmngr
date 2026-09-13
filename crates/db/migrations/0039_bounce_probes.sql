-- Mailman's bounce probes: a member whose score reaches the threshold is
-- sent a probe from a one-time VERP address, and only the probe's own bounce
-- disables delivery. The token is stored hashed; the probe's envelope sender
-- rides on the notice so the out runner can use it.
CREATE TABLE bounce_probes (
    token_hash TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES members(id) ON DELETE CASCADE,
    list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
    sent_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    CHECK (expires_at > sent_at)
);
CREATE INDEX bounce_probes_member ON bounce_probes(member_id);
ALTER TABLE workflow_notices ADD COLUMN mail_from TEXT;
