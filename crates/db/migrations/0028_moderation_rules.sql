-- Message Acceptance settings consumed by the chain rules (Mailman names).
-- Address lists are JSON arrays of exact addresses or ^-anchored regexes.
ALTER TABLE mailing_lists ADD COLUMN administrivia INTEGER NOT NULL DEFAULT 1 CHECK (administrivia IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN require_explicit_destination INTEGER NOT NULL DEFAULT 1 CHECK (require_explicit_destination IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN acceptable_aliases TEXT NOT NULL DEFAULT '[]';
ALTER TABLE mailing_lists ADD COLUMN accept_these_nonmembers TEXT NOT NULL DEFAULT '[]';
ALTER TABLE mailing_lists ADD COLUMN hold_these_nonmembers TEXT NOT NULL DEFAULT '[]';
ALTER TABLE mailing_lists ADD COLUMN reject_these_nonmembers TEXT NOT NULL DEFAULT '[]';
ALTER TABLE mailing_lists ADD COLUMN discard_these_nonmembers TEXT NOT NULL DEFAULT '[]';
-- Argon2id PHC string for the `Approved:` posting key; NULL disables the rule.
ALTER TABLE mailing_lists ADD COLUMN moderator_password TEXT;
