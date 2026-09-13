CREATE TABLE web_sessions (
 token_hash TEXT PRIMARY KEY,
 csrf TEXT NOT NULL,
 user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
 credential_version TEXT,
 expires_at BIGINT NOT NULL
);
CREATE INDEX web_sessions_expiry ON web_sessions(expires_at);
