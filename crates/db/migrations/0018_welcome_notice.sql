-- Portable boolean representation for SQLx Any; opt-in preserves existing delivery.
ALTER TABLE mailing_lists ADD COLUMN send_welcome_message INTEGER NOT NULL DEFAULT 0 CHECK (send_welcome_message IN (0,1));
