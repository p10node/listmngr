-- Archive interactions (P5-INTERACTIONS): one vote per reader per post,
-- tags on threads, a list's categories with one per thread, and a reader's
-- favourite threads.
CREATE TABLE archive_votes (
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
  hash TEXT NOT NULL,
  value INTEGER NOT NULL,
  voted_at BIGINT NOT NULL,
  PRIMARY KEY(user_id, list_id, hash)
);
CREATE TABLE archive_tags (
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
  thread TEXT NOT NULL,
  tag TEXT NOT NULL,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  tagged_at BIGINT NOT NULL,
  PRIMARY KEY(list_id, thread, tag)
);
CREATE TABLE archive_categories (
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  PRIMARY KEY(list_id, name)
);
CREATE TABLE archive_thread_categories (
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
  thread TEXT NOT NULL,
  category TEXT NOT NULL,
  PRIMARY KEY(list_id, thread)
);
CREATE TABLE archive_favorites (
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
  thread TEXT NOT NULL,
  marked_at BIGINT NOT NULL,
  PRIMARY KEY(user_id, list_id, thread)
);
