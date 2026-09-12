-- Mailman's subscription workflow states. `consumed` stays the confirmation
-- token's own flag; `state` is the request's: a request that still needs a
-- moderator outlives its token and is never swept by the expiry cleanup.
ALTER TABLE subscription_workflows ADD COLUMN state TEXT NOT NULL DEFAULT 'pending_confirmation'
  CHECK (state IN ('pending_confirmation','pending_moderation','closed'));
UPDATE subscription_workflows SET state='closed' WHERE consumed=1;
CREATE INDEX subscription_pending ON subscription_workflows(list_id,state,created_at);
