-- Unknown for historical events; never backfill from arbitrary diagnostics.
ALTER TABLE bounce_events ADD COLUMN smtp_stage TEXT CHECK(smtp_stage IN ('ehlo','mail_from','rcpt','data_start','data_final'));
ALTER TABLE bounce_events ADD COLUMN smtp_code BIGINT CHECK(smtp_code BETWEEN 500 AND 599);
