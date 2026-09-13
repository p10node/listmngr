-- Help cooldown is independent of subscription tokens; bounded cleanup per command.
CREATE TABLE email_help_requests (
    list_id TEXT NOT NULL,
    email TEXT NOT NULL,
    requested_at BIGINT NOT NULL,
    PRIMARY KEY(list_id,email)
);
CREATE INDEX email_help_expiry ON email_help_requests(requested_at);
