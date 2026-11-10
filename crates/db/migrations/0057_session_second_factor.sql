-- How a signed-in browser session was established beyond the password: 'totp',
-- 'recovery' or 'passkey' when a second factor was presented for this very
-- session. NULL for password-only and OIDC-only sessions, which therefore never
-- satisfy security.require_2fa_for, whatever the account has enrolled.
ALTER TABLE web_sessions ADD COLUMN second_factor TEXT;
