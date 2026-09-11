-- Portable additive SQLite/PostgreSQL settings. Conditional DNS is unsupported.
ALTER TABLE mailing_lists ADD COLUMN dmarc_mitigate_action TEXT NOT NULL DEFAULT 'no_mitigation' CHECK (dmarc_mitigate_action IN ('no_mitigation','munge_from'));
ALTER TABLE mailing_lists ADD COLUMN dmarc_mitigate_unconditionally INTEGER NOT NULL DEFAULT 0 CHECK (dmarc_mitigate_unconditionally IN (0,1) AND (dmarc_mitigate_action <> 'munge_from' OR dmarc_mitigate_unconditionally = 1));
