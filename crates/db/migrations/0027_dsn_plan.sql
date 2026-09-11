-- Historical string-only plans intentionally remain without issuance authority.
ALTER TABLE delivery_recipients ADD COLUMN authority_snapshot TEXT;
