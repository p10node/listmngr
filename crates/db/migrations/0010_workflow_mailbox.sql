-- Preserve transport mailbox spelling separately from normalized identity.
-- Legacy workflows can only fall back to their previously persisted spelling.
ALTER TABLE subscription_workflows ADD COLUMN original_email TEXT NOT NULL DEFAULT '';
UPDATE subscription_workflows SET original_email=email WHERE original_email='';
