-- Time-based one-time passwords. The shared secret must be readable to verify
-- a code, so it is stored as the authenticator app holds it; recovery codes
-- are single-use and stored only as SHA-256 digests. A session issued by the
-- password alone waits for the second step as `pending_user_id` and counts
-- its failures.
CREATE TABLE user_totp (
  user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  secret TEXT NOT NULL,
  created_at BIGINT NOT NULL,
  confirmed_at BIGINT,
  last_counter BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE user_recovery_codes (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  code_hash TEXT NOT NULL UNIQUE,
  created_at BIGINT NOT NULL,
  used_at BIGINT
);
CREATE INDEX user_recovery_codes_owner ON user_recovery_codes(user_id, used_at);
ALTER TABLE web_sessions ADD COLUMN pending_user_id TEXT REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE web_sessions ADD COLUMN second_factor_attempts BIGINT NOT NULL DEFAULT 0;
