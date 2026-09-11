ALTER TABLE mailing_lists ADD COLUMN bounce_you_are_disabled_warnings BIGINT NOT NULL DEFAULT 3 CHECK (bounce_you_are_disabled_warnings BETWEEN 0 AND 100);
ALTER TABLE mailing_lists ADD COLUMN bounce_you_are_disabled_warnings_interval BIGINT NOT NULL DEFAULT 7 CHECK (bounce_you_are_disabled_warnings_interval BETWEEN 0 AND 36500);
ALTER TABLE mailing_lists ADD COLUMN bounce_notify_owner_on_removal BIGINT NOT NULL DEFAULT 1 CHECK (bounce_notify_owner_on_removal IN (0,1));
