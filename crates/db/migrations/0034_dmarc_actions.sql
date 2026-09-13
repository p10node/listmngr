-- Mailman's full `dmarc_mitigate_action` set (`reject` and `discard` join
-- `no_mitigation` and `munge_from`) and conditional mitigation: the
-- `munge_from`-requires-unconditional rule from 0015 goes, now that the `in`
-- runner evaluates the From domain's DMARC policy. SQLite cannot edit a CHECK
-- in place, so the two columns are recreated with their values preserved.
ALTER TABLE mailing_lists ADD COLUMN dmarc_action_tmp TEXT NOT NULL DEFAULT 'no_mitigation';
ALTER TABLE mailing_lists ADD COLUMN dmarc_unconditional_tmp INTEGER NOT NULL DEFAULT 0;
UPDATE mailing_lists SET dmarc_action_tmp=dmarc_mitigate_action, dmarc_unconditional_tmp=dmarc_mitigate_unconditionally;
ALTER TABLE mailing_lists DROP COLUMN dmarc_mitigate_unconditionally;
ALTER TABLE mailing_lists DROP COLUMN dmarc_mitigate_action;
ALTER TABLE mailing_lists ADD COLUMN dmarc_mitigate_action TEXT NOT NULL DEFAULT 'no_mitigation' CHECK (dmarc_mitigate_action IN ('no_mitigation','munge_from','reject','discard'));
ALTER TABLE mailing_lists ADD COLUMN dmarc_mitigate_unconditionally INTEGER NOT NULL DEFAULT 0 CHECK (dmarc_mitigate_unconditionally IN (0,1));
UPDATE mailing_lists SET dmarc_mitigate_action=dmarc_action_tmp, dmarc_mitigate_unconditionally=dmarc_unconditional_tmp;
ALTER TABLE mailing_lists DROP COLUMN dmarc_action_tmp;
ALTER TABLE mailing_lists DROP COLUMN dmarc_unconditional_tmp;
