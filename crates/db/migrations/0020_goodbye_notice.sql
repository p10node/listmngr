-- Optional private notice on actual Member removal; existing lists remain silent.
ALTER TABLE mailing_lists ADD COLUMN send_goodbye_message INTEGER NOT NULL DEFAULT 0 CHECK (send_goodbye_message IN (0,1));
