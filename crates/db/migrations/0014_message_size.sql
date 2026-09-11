-- Per-list moderation limit in KiB; zero adds no limit to the site LMTP cap.
ALTER TABLE mailing_lists ADD COLUMN max_message_size INTEGER NOT NULL DEFAULT 0 CHECK (max_message_size >= 0 AND max_message_size <= 2147483647);
