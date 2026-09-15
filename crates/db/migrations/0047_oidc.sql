-- OpenID Connect links: one row per (provider, subject) an account has
-- signed in with. The ceremony a browser started — state, nonce and PKCE
-- verifier — waits on its session. A credential minted for a just-in-time
-- account is a random password nobody knows; `usable` marks a password the
-- person chose, which is what unlinking and password-guarded actions need.
CREATE TABLE user_oidc (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider TEXT NOT NULL,
  subject TEXT NOT NULL,
  email TEXT NOT NULL DEFAULT '',
  created_at BIGINT NOT NULL,
  last_used_at BIGINT,
  UNIQUE(provider, subject)
);
CREATE INDEX user_oidc_owner ON user_oidc(user_id, provider);
ALTER TABLE web_sessions ADD COLUMN oidc_state TEXT;
ALTER TABLE user_credentials ADD COLUMN usable INTEGER NOT NULL DEFAULT 1;
