-- Mailman's Usenet gateway settings on a list (`gateway_to_mail`,
-- `gateway_to_news`, `linked_newsgroup`, `nntp_prefix_subject_too`,
-- `newsgroup_moderation`) and the watermark the gateway writes: the last
-- article number gated from the newsgroup, NULL until the first poll.
ALTER TABLE mailing_lists ADD COLUMN gateway_to_mail INTEGER NOT NULL DEFAULT 0 CHECK (gateway_to_mail IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN gateway_to_news INTEGER NOT NULL DEFAULT 0 CHECK (gateway_to_news IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN linked_newsgroup TEXT NOT NULL DEFAULT '';
ALTER TABLE mailing_lists ADD COLUMN nntp_prefix_subject_too INTEGER NOT NULL DEFAULT 1 CHECK (nntp_prefix_subject_too IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN newsgroup_moderation TEXT NOT NULL DEFAULT 'none' CHECK (newsgroup_moderation IN ('none','open_moderated','moderated'));
ALTER TABLE mailing_lists ADD COLUMN usenet_watermark BIGINT;
