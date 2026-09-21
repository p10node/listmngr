# Migrating to listmngr

## From Mailman 2.1 (`listmngr import21`)

`listmngr import21 <list_id> /path/to/lists/<name>/config.pck` reads the
2.1 list's pickled configuration and applies it to a list that already
exists here (create it first with `listmngr lists create`, on a domain
that matches the 2.1 `host_name`). `--dry-run` prints the plan as JSON
and changes nothing; every run prints what it did and its warnings, and
running it again leaves what is there alone (members already subscribed
in a role are reported as `skipped`). The whole import is recorded in the
audit log: each write with its own event, and one `list.import21` event
carrying the report.

The mapping is Mailman 3's own `import21` (`mailman/utilities/importer.py`)
so a site migrated to Mailman 3 and one migrated here end up the same.

### Settings

| Mailman 2.1 | listmngr | Conversion |
| --- | --- | --- |
| `real_name` | `display_name` | as is |
| `description`, `info`, `advertised`, `anonymous_list`, `admin_immed_notify`, `admin_notify_mchanges`, `administrivia`, `require_explicit_destination`, `respond_to_post_requests`, `include_rfc2369_headers`, `emergency`, `collapse_alternatives`, `convert_html_to_plaintext`, `filter_content`, `digest_send_periodic`, `next_digest_number`, `first_strip_reply_to`, `reply_to_address`, `max_message_size`, `max_num_recipients`, `gateway_to_mail`, `gateway_to_news`, `linked_newsgroup`, `topics_enabled`, `topics_bodylines_limit`, `bounce_score_threshold`, `bounce_you_are_disabled_warnings`, `bounce_notify_owner_on_disable`, `bounce_notify_owner_on_removal`, `dmarc_moderation_notice`, `dmarc_wrapped_message_text`, `autoresponse_postings_text`, `autoresponse_request_text` | same name | as is (Python truth for booleans) |
| `subject_prefix` | `subject_prefix` | trimmed, one space appended (2.1 added the space itself) |
| `preferred_language` | `preferred_language` | only a language this site has (`en`, `vi`); otherwise a warning and the list's language kept |
| `send_welcome_msg`, `send_goodbye_msg` | `send_welcome_message`, `send_goodbye_message` | |
| `include_list_post_header` | `allow_list_posts` | |
| `autorespond_admin`, `autoresponse_admin_text` | `autorespond_owner`, `autoresponse_owner_text` | `0` none, `1` respond_and_continue, `2` respond_and_discard |
| `autorespond_postings`, `autorespond_requests` | same | as above |
| `autoresponse_graceperiod` | `autoresponse_grace_period` | days |
| `bounce_processing` | `process_bounces` | |
| `bounce_info_stale_after`, `bounce_you_are_disabled_warnings_interval` | same | seconds → days |
| `bounce_unrecognized_goes_to_list_owner` | `forward_unrecognized_bounces_to` | Mailman's quirk: `True` → `site_owner`, `False` → `discard` |
| `filter_action` | `filter_action` | `0` discard, `1` reject, `2` forward, `3` preserve |
| `filter_mime_types`, `pass_mime_types` | `filter_types`, `pass_types` | |
| `filter_filename_extensions`, `pass_filename_extensions` | `filter_extensions`, `pass_extensions` | |
| `default_member_moderation` + `member_moderation_action` | `default_member_action` | not moderated → `defer`; else `0` hold, `1` reject, `2` discard |
| `generic_nonmember_action` | `default_nonmember_action` | `0` defer, `1` hold, `2` reject, `3` discard |
| `from_is_list`, `dmarc_moderation_action` | `dmarc_mitigate_action`, `dmarc_mitigate_unconditionally` | the larger wins: `0` no_mitigation, `1` munge_from, `2` wrap_message (`3`/`4`/`5` reject/discard/no action, as Mailman); unconditional when `from_is_list` is the larger |
| `dmarc_moderation_addresses` | `dmarc_addresses` | |
| `archive`, `archive_private` | `archive_policy` | not archived → `never`; private → `private`; else `public` |
| `digest_size_threshhold` | `digest_size_threshold` | |
| `digest_volume_frequency` | same | `0` yearly, `1` monthly, `2` quarterly, `3` weekly, `4` daily |
| `reply_goes_to_list` | same | `0` no_munging, `1` point_to_list, `2` explicit_header |
| `personalize` | same | `0` none, `1` individual, `2` full |
| `subscribe_policy` | `subscription_policy` | `0` open, `1` confirm, `2` moderate, `3` confirm_then_moderate |
| `private_roster` | `member_roster_visibility` | `0` public, `1` members, `2` moderators |
| `news_moderation` | `newsgroup_moderation` | `0` none, `1` open_moderated, `2` moderated |
| `news_prefix_subject_too` | `nntp_prefix_subject_too` | |
| `topics` | `topics` | `(name, pattern, description, _)` → `{name, pattern, description}` |
| `acceptable_aliases` | `acceptable_aliases` | one per line, each anchored with `^`, plus `^<list name>@` for the list's own name |
| `accept_these_nonmembers`, `hold_these_nonmembers`, `reject_these_nonmembers`, `discard_these_nonmembers` | same (patterns only) | a `^` pattern stays on the list; an address becomes a nonmember with that action; an `@other-list` entry is warned about and dropped |
| `ban_list` | bans | addresses and `^` patterns; a pattern that is not a valid regular expression is dropped with a warning |
| `header_filter_rules` | header matches | one match per line of each rule, the header split from the pattern at the first of `: `, `:.*`, `:.`, `:`; an empty pattern is `.*`; the action `0` leaves the site default, `2` reject, `3` discard, `6` accept, `7` hold (others warned about); a line without a header, an invalid pattern or a duplicate is skipped with a warning |
| `mod_password` | — | a hash; a warning asks for a new one |
| `password`, `unsubscribe_policy`, `bounce_info`, `usernames`/`passwords` for users, `data_version`, timestamps | — | not imported, as Mailman does not |

### Templates

| Mailman 2.1 | template |
| --- | --- |
| `goodbye_msg` | `list:user:notice:goodbye` |
| `msg_header`, `msg_footer` | `list:member:regular:header`, `list:member:regular:footer` |
| `digest_header`, `digest_footer` | `list:member:digest:header`, `list:member:digest:footer` |

A template equal to Mailman 3's built-in default (the 2.1 default footer,
for one) is not written. Placeholders are converted in Mailman's order:
`%(real_name)s@%(host_name)s` → `To unsubscribe send an email to
${short_listname}-leave@${domain}`, `%(real_name)s mailing list` →
`$display_name mailing list -- $listname`, the `listinfo` line dropped,
`%(real_name)s` → `$display_name`, `%(list_name)s` → `$listname`,
`%(description)s`, `%(info)s`, `%(user_address)s` → `$user_email`,
`%(user_delivered_to)s`, `%(user_name)s`, `%(cgiext)s` and
`%(user_password)s` → nothing. The same conversion is applied to the
three `autoresponse_*` texts (Mailman leaves them, which breaks their
`%(...)s` on a Mailman 3 site).

### Rosters

| Mailman 2.1 | listmngr |
| --- | --- |
| `members` | role `member`, regular delivery; the value, when it is the address as the member wrote it, is kept as the subscribed spelling |
| `digest_members` | role `member`, `mime_digests`, or `plaintext_digests` when the `DisableMime` option (8) is set |
| `owner`, `moderator` | roles `owner`, `moderator` |
| `usernames` | the member's display name |
| `language` | the member's `preferred_language`, when this site has it |
| `user_options` bits | `DontReceiveOwnPosts` (2) → `receive_own_postings` false; `AcknowledgePosts` (4) → `acknowledge_posts`; `ConcealSubscription` (16) → `hide_address`; `Moderate` (128) → the member's own `moderation_action` = the list's `member_moderation_action` (else `defer`); `DontReceiveDuplicates` (256) → `receive_list_copy` false |
| `delivery_status` `(code, when)` | `1` unknown, `2` by_user, `3` by_moderator, `4` by_bounces; owners and moderators always enabled |
| `*_these_nonmembers` addresses | role `nonmember` with `moderation_action` defer/hold/reject/discard |

A banned address is not subscribed (a warning names it); an address listed
twice in one role is subscribed once. Members are subscribed by address
(`subscription_mode` `as_address`), with no user account and no
password: 2.1 passwords are not importable, and a member logs in by
setting a new one.

### Archives

The 2.1 `archives/private/<list>.mbox/<list>.mbox` is imported separately
with `listmngr archive import <list_id> <file.mbox>`, which keeps the
Message-ID-Hash URLs Mailman 3 would give the same messages.

## From Mailman 3 (`import3`)

Not yet available (`P6-IMPORT3` in `docs/PLAN.md` §7).
