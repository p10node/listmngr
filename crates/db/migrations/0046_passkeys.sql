-- WebAuthn passkeys. A credential's public key and dynamic state are stored
-- in the library's binary encoding; the ceremony a browser is in the middle
-- of lives on its session until it completes or expires. The user handle the
-- authenticator returns during a discoverable login identifies the account.
CREATE TABLE user_passkeys (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  credential_id TEXT NOT NULL UNIQUE,
  static_state TEXT NOT NULL,
  dynamic_state TEXT NOT NULL,
  created_at BIGINT NOT NULL,
  last_used_at BIGINT
);
CREATE INDEX user_passkeys_owner ON user_passkeys(user_id, created_at);
ALTER TABLE users ADD COLUMN webauthn_handle TEXT;
CREATE UNIQUE INDEX users_webauthn_handle ON users(webauthn_handle);
ALTER TABLE web_sessions ADD COLUMN webauthn_state TEXT;
