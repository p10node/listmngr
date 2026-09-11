ALTER TABLE mailing_lists ADD COLUMN bounce_notify_owner_on_bounce_increment BIGINT NOT NULL DEFAULT 0 CHECK (bounce_notify_owner_on_bounce_increment IN (0,1));
