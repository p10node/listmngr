-- Mailman's Alter Messages, Member Policy, DMARC text and unrecognized bounce
-- settings (Mailman names and BasicOperation defaults). Lists are JSON arrays.
ALTER TABLE mailing_lists ADD COLUMN filter_content INTEGER NOT NULL DEFAULT 0 CHECK (filter_content IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN filter_types TEXT NOT NULL DEFAULT '[]';
ALTER TABLE mailing_lists ADD COLUMN pass_types TEXT NOT NULL DEFAULT '[]';
ALTER TABLE mailing_lists ADD COLUMN filter_extensions TEXT NOT NULL DEFAULT '[]';
ALTER TABLE mailing_lists ADD COLUMN pass_extensions TEXT NOT NULL DEFAULT '[]';
ALTER TABLE mailing_lists ADD COLUMN collapse_alternatives INTEGER NOT NULL DEFAULT 1 CHECK (collapse_alternatives IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN convert_html_to_plaintext INTEGER NOT NULL DEFAULT 0 CHECK (convert_html_to_plaintext IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN filter_action TEXT NOT NULL DEFAULT 'discard' CHECK (filter_action IN ('discard','reject','forward','preserve'));
ALTER TABLE mailing_lists ADD COLUMN include_rfc2369_headers INTEGER NOT NULL DEFAULT 1 CHECK (include_rfc2369_headers IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN allow_list_posts INTEGER NOT NULL DEFAULT 1 CHECK (allow_list_posts IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN reply_goes_to_list TEXT NOT NULL DEFAULT 'no_munging' CHECK (reply_goes_to_list IN ('no_munging','point_to_list','explicit_header','explicit_header_only'));
ALTER TABLE mailing_lists ADD COLUMN reply_to_address TEXT NOT NULL DEFAULT '';
ALTER TABLE mailing_lists ADD COLUMN first_strip_reply_to INTEGER NOT NULL DEFAULT 0 CHECK (first_strip_reply_to IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN personalize TEXT NOT NULL DEFAULT 'none' CHECK (personalize IN ('none','individual','full'));
ALTER TABLE mailing_lists ADD COLUMN include_sender_header INTEGER NOT NULL DEFAULT 1 CHECK (include_sender_header IN (0,1));
ALTER TABLE mailing_lists ADD COLUMN subscription_policy TEXT NOT NULL DEFAULT 'confirm' CHECK (subscription_policy IN ('open','confirm','moderate','confirm_then_moderate'));
ALTER TABLE mailing_lists ADD COLUMN unsubscription_policy TEXT NOT NULL DEFAULT 'confirm' CHECK (unsubscription_policy IN ('open','confirm','moderate','confirm_then_moderate'));
ALTER TABLE mailing_lists ADD COLUMN member_roster_visibility TEXT NOT NULL DEFAULT 'moderators' CHECK (member_roster_visibility IN ('public','members','moderators'));
ALTER TABLE mailing_lists ADD COLUMN dmarc_addresses TEXT NOT NULL DEFAULT '[]';
ALTER TABLE mailing_lists ADD COLUMN dmarc_moderation_notice TEXT NOT NULL DEFAULT '';
ALTER TABLE mailing_lists ADD COLUMN dmarc_wrapped_message_text TEXT NOT NULL DEFAULT '';
ALTER TABLE mailing_lists ADD COLUMN forward_unrecognized_bounces_to TEXT NOT NULL DEFAULT 'administrators' CHECK (forward_unrecognized_bounces_to IN ('discard','site_owner','administrators'));
