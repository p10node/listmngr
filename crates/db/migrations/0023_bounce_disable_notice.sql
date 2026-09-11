ALTER TABLE mailing_lists ADD COLUMN bounce_notify_owner_on_disable BIGINT NOT NULL DEFAULT 1 CHECK (bounce_notify_owner_on_disable IN (0,1));
