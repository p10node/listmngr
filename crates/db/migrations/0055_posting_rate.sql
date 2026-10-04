-- The posting-rate rule's ledger: one row per post the `in` runner
-- accepted for a list, by the sender's envelope address, written in the
-- transaction that accepts the post. The rule counts the rows newer than
-- the configured window; the task sweep deletes rows older than a day,
-- the longest window `security.rate_limit.post` can name. Nothing else
-- reads it, and a list's rows go with the list.
CREATE TABLE posting_rate (
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
  email TEXT NOT NULL,
  posted_at BIGINT NOT NULL
);
CREATE INDEX posting_rate_sender ON posting_rate(list_id, email, posted_at);
CREATE INDEX posting_rate_age ON posting_rate(posted_at);
