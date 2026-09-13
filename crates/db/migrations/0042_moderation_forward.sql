-- Mailman's `forward`: the address a moderator forwarded a held post to,
-- kept with the decision that carried it.
ALTER TABLE moderation_log ADD COLUMN forward_to TEXT;
