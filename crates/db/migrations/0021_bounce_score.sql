-- Opt-in bounded direct RCPT failure scoring; stale interval is days.
ALTER TABLE mailing_lists ADD COLUMN process_bounces INTEGER NOT NULL DEFAULT 0 CHECK (process_bounces IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN bounce_info_stale_after INTEGER NOT NULL DEFAULT 7 CHECK (bounce_info_stale_after BETWEEN 1 AND 3650);
