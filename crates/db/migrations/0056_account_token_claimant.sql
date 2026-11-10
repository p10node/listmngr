-- The account that asked for an address verification. A signed-in account that
-- adds an address nobody owns is not linked to it when it asks: the request
-- travels on the mailed token, and the mailbox owner's proof is what links
-- the address (and verifies it) to this account. NULL for signup tokens, whose
-- address is already linked to the account the signup created.
ALTER TABLE account_tokens ADD COLUMN claimant_user_id TEXT REFERENCES users(id) ON DELETE CASCADE;
