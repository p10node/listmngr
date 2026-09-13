-- Administrative subscriptions carry the operator's display name and their
-- `pre_approved` decision, so a confirmation that was already approved (an
-- invitation included) does not stop at the moderator afterwards.
ALTER TABLE subscription_workflows ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
ALTER TABLE subscription_workflows ADD COLUMN pre_approved INTEGER NOT NULL DEFAULT 0
  CHECK (pre_approved IN (0,1));
