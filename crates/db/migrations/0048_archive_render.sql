-- Archive rendering (P5-RENDER): the sender and date of each archived post as
-- the cooked copy shows them, the post it replies to (a Message-ID-Hash that
-- may name a post the archive never received), and the attachments stored
-- once at indexing time so downloads never re-parse the raw message.
ALTER TABLE archive_messages ADD COLUMN sender_name TEXT NOT NULL DEFAULT '';
ALTER TABLE archive_messages ADD COLUMN sender_email TEXT NOT NULL DEFAULT '';
ALTER TABLE archive_messages ADD COLUMN message_date BIGINT;
ALTER TABLE archive_messages ADD COLUMN parent_hash TEXT;
CREATE TABLE archive_attachments (
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE CASCADE,
  hash TEXT NOT NULL,
  position BIGINT NOT NULL,
  filename TEXT NOT NULL,
  content_type TEXT NOT NULL,
  size BIGINT NOT NULL,
  content BYTEA NOT NULL,
  PRIMARY KEY(list_id, hash, position)
);
CREATE INDEX archive_parent ON archive_messages(list_id, parent_hash);
