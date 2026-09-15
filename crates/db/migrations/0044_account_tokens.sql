-- Single-use secrets a person proves from their mailbox: address verification
-- after signup, and password reset. Only the SHA-256 of the token is stored;
-- a row is consumed once and expires on its own.
CREATE TABLE account_tokens (
  id TEXT PRIMARY KEY,
  purpose TEXT NOT NULL CHECK(purpose IN ('verify_address','password_reset')),
  address_id TEXT NOT NULL REFERENCES addresses(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE,
  created_at BIGINT NOT NULL,
  expires_at BIGINT NOT NULL,
  consumed_at BIGINT
);
CREATE INDEX account_tokens_expiry ON account_tokens(expires_at);
CREATE INDEX account_tokens_address ON account_tokens(address_id, purpose, created_at);
