-- Archive UI (P5-UI): when a signed-in reader last opened each thread, so
-- thread lists can mark the ones with newer posts.
CREATE TABLE archive_thread_views (
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
  thread TEXT NOT NULL,
  viewed_at BIGINT NOT NULL,
  PRIMARY KEY(user_id, list_id, thread)
);
