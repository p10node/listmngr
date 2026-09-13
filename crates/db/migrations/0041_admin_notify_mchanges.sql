-- Automatic Responses (Mailman name): tell owners and moderators when a
-- member subscribes or unsubscribes. Off by default, as in Mailman.
ALTER TABLE mailing_lists ADD COLUMN admin_notify_mchanges INTEGER NOT NULL DEFAULT 0 CHECK (admin_notify_mchanges IN (0,1));
