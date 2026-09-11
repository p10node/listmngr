-- Visible To/Cc mailbox count; zero disables the per-list posting check.
ALTER TABLE mailing_lists ADD COLUMN max_num_recipients INTEGER NOT NULL DEFAULT 0 CHECK (max_num_recipients >= 0 AND max_num_recipients <= 2147483647);
