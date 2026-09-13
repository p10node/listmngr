-- Mailman's Digest settings: whether digests are produced at all, the size
-- that triggers an issue (KiB; 0 never), whether the daily periodic run
-- sends what is pending, and how often the volume number rolls over.
ALTER TABLE mailing_lists ADD COLUMN digests_enabled INTEGER NOT NULL DEFAULT 1 CHECK (digests_enabled IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN digest_size_threshold DOUBLE PRECISION NOT NULL DEFAULT 30 CHECK (digest_size_threshold >= 0);
ALTER TABLE mailing_lists ADD COLUMN digest_send_periodic INTEGER NOT NULL DEFAULT 1 CHECK (digest_send_periodic IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN digest_volume_frequency TEXT NOT NULL DEFAULT 'monthly'
  CHECK (digest_volume_frequency IN ('yearly','monthly','quarterly','weekly','daily'));
