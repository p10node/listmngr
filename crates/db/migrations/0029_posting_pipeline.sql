-- Name of the handler pipeline an accepted post runs (Mailman attribute).
ALTER TABLE mailing_lists ADD COLUMN posting_pipeline TEXT NOT NULL DEFAULT 'default-posting-pipeline';
