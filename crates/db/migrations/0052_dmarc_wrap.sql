-- Mailman's `wrap_message` joins the `dmarc_mitigate_action` vocabulary.
-- SQLite cannot edit a CHECK in place, so the column is recreated with its
-- values preserved, as 0034 did.
ALTER TABLE mailing_lists ADD COLUMN dmarc_action_tmp TEXT NOT NULL DEFAULT 'no_mitigation';
UPDATE mailing_lists SET dmarc_action_tmp=dmarc_mitigate_action;
ALTER TABLE mailing_lists DROP COLUMN dmarc_mitigate_action;
ALTER TABLE mailing_lists ADD COLUMN dmarc_mitigate_action TEXT NOT NULL DEFAULT 'no_mitigation' CHECK (dmarc_mitigate_action IN ('no_mitigation','munge_from','wrap_message','reject','discard'));
UPDATE mailing_lists SET dmarc_mitigate_action=dmarc_action_tmp;
ALTER TABLE mailing_lists DROP COLUMN dmarc_action_tmp;
