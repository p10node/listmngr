-- Per-list overrides inherit site defaults when NULL. Existing announcement
-- lists must gain the same posting policy as newly created announcement lists.
ALTER TABLE mailing_lists ADD COLUMN default_member_action TEXT
    CHECK (default_member_action IN ('defer','accept','hold','reject','discard'));
ALTER TABLE mailing_lists ADD COLUMN default_nonmember_action TEXT
    CHECK (default_nonmember_action IN ('defer','accept','hold','reject','discard'));
UPDATE mailing_lists SET default_member_action='hold', default_nonmember_action='hold'
    WHERE style_name='legacy-announce';
