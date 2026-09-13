-- Mailman topics (the `tagger` handler): named multi-line patterns matched
-- against subjects, keywords and header-like body lines. `topics` is a JSON
-- array of {name, pattern, description}.
ALTER TABLE mailing_lists ADD COLUMN topics_enabled INTEGER NOT NULL DEFAULT 0 CHECK (topics_enabled IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN topics_bodylines_limit INTEGER NOT NULL DEFAULT 5;
ALTER TABLE mailing_lists ADD COLUMN topics TEXT NOT NULL DEFAULT '[]';
