-- Bounded direct SMTP threshold; no retrospective processing.
ALTER TABLE mailing_lists ADD COLUMN bounce_score_threshold DOUBLE PRECISION NOT NULL DEFAULT 5 CHECK (bounce_score_threshold > 0 AND bounce_score_threshold <= 1000000);
