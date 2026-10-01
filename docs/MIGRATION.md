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

### Accounts and passwords

Every Mailman account is created here with its display name, its
`is_server_owner` flag, its addresses (an address nobody owns is adopted
with its registration date, and one Mailman had verified stays verified;
an address that already belongs to an account here is left alone and the
Mailman account is skipped), its preferred address and its own
preferences.

**No password is carried over.** Mailman hashes passwords with passlib
(`pbkdf2_sha512` by default), which this site cannot verify, so each
imported account gets a random password nobody knows, stored as
unusable: nobody can sign in with it, and the owner takes the account
over through the password-recovery flow. The import warns once per
account that had a password and never prints the hash. The write is
audited as `user.import` with the addresses and `usable_password:
false`.

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

## From Mailman 3 (`listmngr import3`)

A Mailman 3 core is read either through its REST API while it runs, or
straight from its database and `var_dir` once it is stopped, and written
here. Both give the same plan.

### Over REST

A running core is read through its REST API:

```sh
printf '%s' "$MAILMAN_REST_PASSWORD" > /protected/path/rest.pass
listmngr import3 --rest http://127.0.0.1:8001/3.1 \
    --user restadmin --password-file /protected/path/rest.pass --dry-run
listmngr import3 --rest http://127.0.0.1:8001/3.1 \
    --user restadmin --password-file /protected/path/rest.pass
```

The core is only read (`GET`), never changed, and the password is taken
from a file so it stays out of the shell history. `--list <list_id>`
imports one list; `--dry-run` prints the plan. Everything that is
already here is left alone, so the command can be run again — for
instance to catch up a site that kept running during the migration.

### From the database

With the core stopped, the same site comes from its own storage:

```sh
listmngr import3 --db sqlite:////var/lib/mailman/data/mailman.db \
    --var-dir /var/lib/mailman --dry-run
listmngr import3 --db postgres://mailman:…@127.0.0.1/mailman \
    --var-dir /var/lib/mailman
```

`--db` takes the `[database] url` of the core's `mailman.cfg` (its
`postgresql://` spelling is accepted); `--var-dir` is its `var_dir`,
where `messages/` holds the held messages — without it they are reported
and left behind. The tables are decoded into what the REST API would
have answered: enums by the integers of `mailman.interfaces`, the three
intervals as whole days, the nonmember and DMARC lists from their pickled
columns, the pending subscriptions and held-message metadata from
`pended`/`pendedkeyvalue`, and each held message from the pickled
`email.message.Message` in the store, rendered back as Python would. A
database nobody can open exits `11`, and the URL's password never reaches
the output.

### What is read

| Mailman 3 resource | Here |
| --- | --- |
| `/domains` | domains (`mail_host`, `description`, `alias_domain`) |
| `/bans` | site-wide bans |
| `/users`, `/users/<id>/addresses`, `/users/<id>/preferences`, `/users/<id>/preferred_address` | accounts with their addresses, preferences and preferred address (Mailman has an account behind every address, so plain subscribers bring one too) |
| `/lists?advertised=false` | every list, advertised or not |
| `/lists/<id>/config` | the list configuration (below) |
| `/lists/<id>/roster/{member,owner,moderator,nonmember}` | the rosters, by address |
| `/members/<id>/preferences` | that member's own preferences (Mailman answers only what is set on the member itself) |
| `/lists/<id>/bans` | the list's bans |
| `/lists/<id>/header-matches` | header matches (`header`, `pattern`, `action` → chain) |
| `/lists/<id>/uris` | reported as warnings: Mailman keeps a URI, not the text |
| `/lists/<id>/held` | messages waiting for a moderator, kept whole with the core's hold date |
| `/lists/<id>/requests` | subscription changes waiting for a moderator (a confirmation the subscriber owes stays behind, with a warning) |

### Settings

Most of the configuration carries the same name, because this project
follows Mailman 3's model. The conversions:

| Mailman 3 | Here |
| --- | --- |
| `autoresponse_grace_period`, `bounce_info_stale_after`, `bounce_you_are_disabled_warnings_interval` (`7d`, `90d`) | whole days; a duration with hours or minutes left over is a warning |
| `bounce_score_threshold`, `digest_size_threshold` (`5` or `5.0`) | a number |
| `acceptable_aliases` (a list, or one multi-line string) | one alias per line |
| `preferred_language` | only a language this site has (`en`, `vi`); else a warning |
| `moderator_password` | not imported (a hash); a warning asks for a new one. Its value never reaches a message or a log |
| `bounces_address`, `join_address`, `leave_address`, `no_reply_address`, `owner_address`, `posting_address`, `request_address`, `fqdn_listname`, `list_name`, `mail_host`, `created_at`, `last_post_at`, `digest_last_sent_at`, `post_id`, `volume`, `usenet_watermark` | derived here too: read, never written |
| `max_days_to_hold`, and any setting a newer Mailman adds | a warning naming it, unless it is at Mailman's own default |
| everything else (75 settings: the moderation actions, archives, bounces, digests, DMARC, content filtering, the Usenet gateway, the autoresponders, the reply/personalize/roster-visibility policies, …) | the same name and value |

### Rosters

Members are subscribed with the display name Mailman had, the member's
own preferences (`acknowledge_posts`, `hide_address`,
`receive_list_copy`, `receive_own_postings`, `delivery_mode`,
`delivery_status`, `preferred_language`) and the member's own
`moderation_action` when Mailman set one. A member Mailman subscribed as
a *user* (`subscription_mode: as_user`) is subscribed the same way here,
against the account the import brought over; one whose account the
import did not see is subscribed as the address, with a warning.

### What was waiting

A message the core was holding comes over with its bytes, its sender,
its subject, the core's reason and the date it was held, and waits for a
moderator here; accepting it later delivers exactly what was held.
Importing the same message twice is refused, so the command can be run
again. A subscription change waiting for a moderator becomes a request
here, in `pending_moderation`, with no token issued.

**An import mails nobody.** No hold notice, no confirmation, no welcome:
whoever needed telling was told by the site being left behind. A
confirmation the subscriber still owed is *not* imported — the token was
issued by the other site and cannot be honoured here — and each one is
reported so the operator can ask those people to join again.

### The archive (HyperKitty)

HyperKitty keeps each post's text, not the message, so the posts come
over as an mbox — HyperKitty's own export
(`/archives/list/<list>/export/<name>.mbox.gz`) or the mbox it was
imported from — imported with `listmngr archive import <list_id>
<file.mbox>`. The Message-ID-Hash computed here is
HyperKitty's, and a thread is named by its first post's hash as
HyperKitty names it, so the URLs stay the same.

What an mbox cannot carry — what readers left on the archive — comes
from HyperKitty's own database, after the site and the mbox:

```sh
listmngr import3 --hyperkitty sqlite:////var/lib/mailman-web/mailman-web.db --dry-run
listmngr import3 --hyperkitty postgres://mailmanweb:…@127.0.0.1/mailmanweb
```

`--hyperkitty` takes the Django `DATABASES` of the Mailman web
installation as a URL, and may be given alone or together with `--rest`
or `--db` (the site is imported first). `--list` limits it to one list.

| HyperKitty | Here |
| --- | --- |
| `hyperkitty_vote` (`email`, `user`, `value` ±1) | a vote on the post with that Message-ID-Hash |
| `hyperkitty_tagging` → `hyperkitty_tag.name` | a tag on the thread, by the reader who tagged it |
| `hyperkitty_thread.category` → `hyperkitty_threadcategory.name` | the thread's category (the category is created on the list; its colour is not kept) |
| `hyperkitty_favorite` | a favourite thread of that reader |

A reader is matched by the email address of their Django account
(`auth_user.email`) to an account imported here; a row whose reader has
no account here, or whose post or thread the mbox did not bring, is
counted as `skipped`, never invented. Each list prints one JSON line
(`votes`, `tags`, `categories`, `favorites`, `skipped`), one
`archive.import_interactions` audit event records it, nothing is mailed,
and running it again writes the same rows, not more. A list HyperKitty
archives that is not a list here is a warning.

### Not imported yet

Passwords (see above); HyperKitty's Django profiles, social logins,
per-reader "last read" markers, thread `starting_email` overrides and
category colours.

## URLs after the move

HyperKitty's and Postorius's URLs redirect (`308`) to the pages here, so
links in old mail, bookmarks and search engines keep working; nothing
needs rewriting in a proxy unless HyperKitty was served under a prefix
other than `/archives/` or `/hyperkitty/`.

| Mailman 3 URL | Here |
| --- | --- |
| `/archives/list/<list>@<domain>/message/<hash>/` (and `/hyperkitty/…`) | `/web/lists/<list>.<domain>/archive?message=<hash>` |
| `/archives/list/<address>/thread/<hash>/` | `/web/lists/<id>/archive/thread/<hash>` |
| `/archives/list/<address>/<year>/<month>/` | `/web/lists/<id>/archive/threads/<year>/<month>` |
| `/archives/list/<address>/`, `…/latest` | `/web/lists/<id>/archive` |
| `/archives/`, `/hyperkitty/`, `/postorius/`, `/postorius/lists/` | `/web` |
| `/postorius/lists/<id>/` | `/web/lists/<id>` |

The hashes are HyperKitty's `Message-ID-Hash`, which this archive
computes the same way (`P5-ACCEPTANCE`), so a permalink HyperKitty minted
finds the same post. Postorius's other pages (settings, members) are not
mapped.
