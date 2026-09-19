-- Archive administration (P5-ADMIN): an owner hides a post from every
-- reading surface without destroying it. A hidden post keeps its row, its
-- attachments and its place in the thread; only the owner's archive
-- administration page still names it, and only to put it back. Deletion
-- removes rows outright and needs no column of its own.
ALTER TABLE archive_messages ADD COLUMN hidden_at BIGINT;
