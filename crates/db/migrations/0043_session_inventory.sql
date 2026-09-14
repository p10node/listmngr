-- A reader can only manage sessions the page can name, so every session row
-- gets an opaque id and a creation time. Existing rows are dropped rather than
-- backfilled: a browser session lives at most eight hours, and reusing the
-- token hash as a public id would put a credential digest on a page.
DELETE FROM web_sessions;
ALTER TABLE web_sessions ADD COLUMN id TEXT;
ALTER TABLE web_sessions ADD COLUMN created_at BIGINT;
CREATE UNIQUE INDEX web_sessions_id ON web_sessions(id);
CREATE INDEX web_sessions_owner ON web_sessions(user_id, created_at);
