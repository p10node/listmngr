-- Automatic Responses (Mailman names): notify the poster when their post is
-- held, and notify owners/moderators immediately of a held post.
ALTER TABLE mailing_lists ADD COLUMN respond_to_post_requests INTEGER NOT NULL DEFAULT 1 CHECK (respond_to_post_requests IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN admin_immed_notify INTEGER NOT NULL DEFAULT 1 CHECK (admin_immed_notify IN (0,1));
