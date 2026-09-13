-- Bounce events now also come from the bounce runner: a VERP address, a
-- delivery-status report, one whose ENVID this server issued, or the
-- heuristic detectors reading an MTA's prose. SQLite
-- cannot edit a CHECK in place, so the column is recreated with its values
-- preserved (and the Mailman-era default the runner never relies on).
ALTER TABLE bounce_events ADD COLUMN source_tmp TEXT NOT NULL DEFAULT 'smtp_permanent_failure';
UPDATE bounce_events SET source_tmp=source;
ALTER TABLE bounce_events DROP COLUMN source;
ALTER TABLE bounce_events ADD COLUMN source TEXT NOT NULL DEFAULT 'smtp_permanent_failure'
  CHECK (source IN ('smtp_permanent_failure','verp','dsn','dsn_envid','heuristic'));
UPDATE bounce_events SET source=source_tmp;
ALTER TABLE bounce_events DROP COLUMN source_tmp;
