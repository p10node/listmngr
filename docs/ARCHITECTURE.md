# Architecture

## Webhooks: the store — bounded local acceptance verified

Migration `0054_webhooks.sql`: `webhooks` (`id`, `url`, `description`,
`events` as a JSON array of patterns, `list_id` nullable and `RESTRICT`
to `mailing_lists`, `enabled`, `secret_hash`, `secret_salt`,
`created_at`/`updated_at` in milliseconds) and `webhook_deliveries`
(`id`, `webhook_id` `RESTRICT` to `webhooks`, `event`, `list_id`,
`payload` JSON, `state` `pending`/`delivered`/`failed`, `attempts`,
`next_attempt_at`, `leased_until`, `last_status`, `last_error`,
`created_at`, `finished_at`; indexed on `(state, next_attempt_at)` for
the runner and `(webhook_id, created_at)` for the API). Both are
deleted explicitly with their list in `ListRepo::delete_tx` and with
their webhook, as the other list-owned tables are.

`listmngr_db::webhooks` (`crates/db/src/webhooks.rs`):
`WebhookRepo::create_with_context` validates the URL (`https://` unless
`[webhooks] allow_http`; no userinfo, no whitespace, at most 2048
bytes), the events (`*`, `name.*` or `name` of `[a-z0-9_.]`, at most 64,
deduplicated) and the description, derives the secret and stores its
SHA-256 and the salt; `get`, `list(Option<&ListId>)`,
`update_with_context` (a from → to diff per changed field),
`delete_with_context` (deliveries first), `rotate_with_context` (a new
salt, the old fingerprint audited), `secret` (derived again for the
runner), `ping_with_context` (a `ping` delivery outside the audit path),
`deliveries`/`delivery`. The secret is HKDF-SHA256 by hand over the
workspace's `hmac`: extract with the salt over the signing key, expand
one block with `"listmngr webhook " || id`, 64 hex digits. `Database`
carries the key and `allow_http` through `with_webhooks`, set beside
`with_base_url` in the CLI and the API; `WebhooksConfig::resolve`
reads `signing_key_file` (owner-only permissions, like the other secret
files), insists on 32 characters, and on a key whenever `enabled`.

`fan_out(tx, context, at, action, target_type, target_id, diff)` runs at
the end of `Database::record_tx_with_context` and of moderation's own
`audit_held` (target type `held_message`), inside the caller's
transaction: `SELECT … FROM webhooks WHERE enabled=1`, `event_matches`
per hook, the event's list resolved once (`event_list`: the target when
it is a list, else the diff's `list_id`, else the member's or held
message's list by id), a list-bound hook skipped for another list's
event, then one `pending` delivery per match with the payload above and
`next_attempt_at = created_at`. A hook subscribed to `webhook.*` hears of
its own creation, since the row exists when its audit event is written.
The queue's, bounce processing's and DSN issuance's own audit inserts do
not fan out. `TaskRepo::sweep` gained `collected_webhook_deliveries`:
`state <> 'pending'` and `finished_at` older than the retention.

## Read-only operator diagnostics (`P6-DOCTOR`) — bounded local acceptance verified

The CLI dispatches `doctor` before the ordinary database-opening path.
`listmngr_db::doctor::inspect` forces SQLite mode=ro, or opens a PostgreSQL
read-only transaction, then compares applied migration versions/checksums
and dirty state to the embedded migrator. It reads at most 1000 mail domains;
an excessive inventory fails closed. This validates the ledger, not every
physical table/index. No raw connection error or DSN is returned.

The DB deadline is 3 seconds. An enabled mail role adds a 3-second relay TCP
greeting probe (32 lines maximum, 512 bytes per line), sending zero application
bytes. DNS has a shared 10-second budget, absolute names, explicit MX/address
or RFC 5321 implicit-MX checks, null-MX rejection and a 32-exchanger cap.
Every advertised exchanger must resolve; this is deliberately stricter than
SMTP's ability to deliver through one working alternative. Skipped TLS/AUTH,
delivery and DNS authentication are exposed in JSON, never promoted to passed
checks. Exit 12 carries the existing CLI's redacted `CLI-DOCTOR` error.
The tests drive the real binary against loopback fixtures (a UDP DNS server
of the test's own, TCP SMTP greeters, SQLite files whose bytes are compared
before and after), which establishes the contract, not a deployed MTA or
public DNS; the evidence is the `P6-DOCTOR` row of `docs/FEATURE_PARITY.md`.

## Notice languages from Mailman's translations — bounded local acceptance verified

The interface and the notices now have separate language sets.
`listmngr_i18n::SUPPORTED` (`en`, `vi`) is what the interface negotiates
(`supported_match`, `negotiate`, `choose`), unchanged. `notice_languages()`
is those plus the languages of the generated `crates/i18n/src/mailman.rs`,
negotiated by `notice_match`/`negotiate_notice`/`choose_notice` with the same
exact-then-primary-subtag rule; `message()` negotiates over them, so a
subject resolves in Mailman's translation where one was imported and in
English otherwise, per message. Every bundle also carries the generated
`languages.ftl`, the endonyms behind `NOTICE_LANGUAGE_OPTIONS`, which the
list and member language pickers use. `notices::recipient_language` and
`delivery` choose with `choose_notice`.

Bodies: `listmngr_mail::templates::builtin_language(name, language)` →
`(body, served language)` tries this project's `vi`, else
`templates_mailman::builtin(language, name)` (generated, one `include_str!`
per `crates/mail/catalog/mailman/<lang>/<name>.txt`), else English; the
resolved template's `source` says `builtin:<served>`. Only the names in
`templates_mailman::IMPORTED` have Mailman translations, and
`crates/mail/tests/mailman_catalog.rs` holds each one's English here equal,
word for word, to the vendored `catalog/mailman/en/<name>.txt` — rewording
one of them breaks that test, which is the signal that its translations no
longer fit. `tests/compat/import_mailman_templates.py` is the only writer:
it keeps a body that is non-empty, differs from the English and uses a
subset of the English's placeholders, and a subject whose `msgid`, with the
placeholders renamed by its table (`${mlist.display_name}` → `display_name`
or `listname` as the English here uses, positional `{}` by place,
`${digmode}` dropped), is exactly the Fluent English, from a non-fuzzy
`msgstr` with the same placeholders; then it runs `rustfmt` on the two
tables. `import3::site_language` maps Mailman's `pt_BR` to `pt-BR`.

## HyperKitty interactions import — bounded local acceptance verified

`listmngr_import::hyperkitty::fetch(url, only)` reads a HyperKitty
database (SQLite or PostgreSQL through `sqlx::Any`, one connection, the
URL's userinfo scrubbed from errors like `db3`) into one `Archive` per
`hyperkitty_mailinglist` row: the votes as `(message_id_hash, reader
address, ±1)` joined through `hyperkitty_email` and `auth_user`, the
taggings as `(thread_id, tag, address)`, each thread's category name,
and the favourites, plus the message and thread counts for `--dry-run`.
HyperKitty's `message_id_hash` is the hash this archive computes and its
`thread_id` is the hash of the thread's first post, which is how threads
are named here, so no mapping table is needed.

`ArchiveRepo::import_interactions` (`crates/db/src/archive_interact_import.rs`)
writes them in one transaction with one `archive.import_interactions`
audit event carrying the report: a reader is resolved to an account
through `addresses.user_id`, a vote needs a visible post with that hash,
a tag, category or favourite a visible thread; anything else is
`skipped`. Each row is deleted and rewritten on its primary key, so a
second run changes nothing; a category is created on the list when it is
missing. Nothing is queued. `listmngr import3 --hyperkitty URL` runs it
after the site (`--rest`/`--db`) when both are given, prints one JSON
line per list, and warns for a list that is not here.

## Mailman 3 import from the database — bounded local acceptance verified

`listmngr_import::db3::fetch_db(url, var_dir)` reads a Mailman 3 core's
own database (SQLite or PostgreSQL through `sqlx::Any`, Mailman's
`postgresql://` accepted) into the same `Site` the REST client gives, so
`import3::plan` and `apply` are shared and the two sources are checked
against each other on the same core. What REST spells out, the tables
keep as storage: enums as the integers of `mailman.interfaces` (the
`ACTION`, `FILTER_ACTION`, `ARCHIVE_POLICY`, … tables), booleans that
SQLite's driver cannot type (`CAST(… AS INTEGER)`), the three intervals
as a datetime counted from the epoch on SQLite and a real interval on
PostgreSQL (both asked for in seconds), the nonmember and DMARC lists as
pickled `MutableList`s (the crate's own pickle reader, which now also
resolves `STACK_GLOBAL` and SQLAlchemy's mutable collections), the user
id as a UUID spelt with hyphens on PostgreSQL, the pending
subscriptions and each held message's metadata as JSON key/values under
a token (`pended`/`pendedkeyvalue`, the `_pck_*` values pickles inside
JSON strings), and each held message as a pickled
`email.message.Message` under `var/messages/<path>`, which
`render_message` writes back the way Python's generator would (headers,
`Header` chunks, preamble, boundaries, epilogue). A held message the
store cannot give — no `--var-dir`, no file — is a warning on the `Site`
and left behind. `listmngr import3 --db URL [--var-dir DIR]` is the
command; `--rest` and `--db` exclude each other, and a database nobody
can open exits `11` with the URL's userinfo scrubbed.

## Mailman 3 import over REST — bounded local acceptance verified

`listmngr_import::rest3::Rest` is a read-only client of a Mailman 3
core's REST API: Basic authentication, a thirty-second timeout, `GET`
only, and a `Debug` that shows the root and the user but never the
password (a URL carrying userinfo is replaced in any error message).
`import3::Source` is the one thing the importer needs — a path in, JSON
out — so the tests can drive the mapping from the answers a real core
gave without a server.

`import3::fetch(source, only)` walks the site: `domains`, the site
`bans`, `users` — each with `users/<id>/addresses`,
`users/<id>/preferences` and `users/<id>/preferred_address` through
`Source::get_optional`, which turns Mailman's `404` into `None` — then
`lists?advertised=false` (or one list), and for each list its
`config`, the four rosters, each member's own `preferences`
(`members/<id>/preferences`, which Mailman answers sparsely), the list's
`bans`, its `header-matches`, its `uris`, its `held` and its `requests`.
`collection` reads
Mailman's envelope (`entries`, `start`, `total_size`) and pages with
`count`/`page` until it has them all. The result is a `Site` of
`Domain3`/`List3`/`Member3`, Mailman's own names and values.

`import3::plan(site)` turns that into `Plan3`: the `SETTINGS` table (75
Mailman settings this site has, with `Kind3::Same`, `Days` for the `7d`
timedeltas, `Float` for the two thresholds Mailman may write as integers
and `Lines` for `acceptable_aliases`), the `DERIVED` list read and
dropped, and a warning for everything else — a setting this site does not
have (unless it is at Mailman's own default), a language the catalog does
not have, the moderator password hash (never its value), each template
URI, and each member Mailman subscribed as a user.
`import3::apply(db, plan, context)` writes the domains, the accounts, the
site bans and each list (create, configure, ban, header-match, subscribe
with preferences and moderation action), skipping and counting what is
already there, and records one `site.import3` audit event with the
report. An account goes through `UserRepo::create_imported_with_context`
(`crates/db/src/user_import.rs`): one transaction holding the
preferences row, the user, a credential row with a random password
marked `usable=0`, each address (a bare one adopted with its
registration date and verification, a mailbox Mailman had verified
verified here, an address owned by another account a `Conflict` that
writes nothing), every membership of those addresses given the account,
the preferred address, and one `user.import` audit event that names the
addresses and records `usable_password: false` — never hash material.
What the core was still holding goes through
`crates/db/src/moderation_import.rs`:
`ModerationRepo::hold_imported_with_context` stores the message bytes
(`message_blobs`/`messages`, context `imported: true`), inserts the
`held_messages` row with the core's own `hold_date`, refuses a message
already held on that list, audits `moderation.import` — never
`moderation.hold` — and queues no notice, so nobody is mailed by a
migration; `WorkflowRepo::import_request_with_context` inserts a
`subscription_workflows` row in `pending_moderation` with a token hash
nobody holds, refuses a banned address, and audits
`subscription.import`. `import3::plan` keeps only the requests whose
`token_owner` is `moderator` and reports the confirmations the
subscriber still owed, whose tokens belong to the other site.
A member Mailman had `as_user` is then subscribed `SubscriptionMode::AsUser`,
so `Member::user_id` carries the account and the user preference layer
applies as it does for an account made here.
`crates/cli/src/import3.rs` is `listmngr import3`, whose
`--password-file` keeps the core's password out of argv; a source that
cannot be read exits `11` (`CLI-IMPORT-SOURCE`).

## Mailman 2.1 import — bounded local acceptance verified

`crates/import` (`listmngr-import`) is the migration crate.
`pickle::read` is a reader for the pickles Python 2 wrote — protocols
0–2 with the protocol 3–4 opcodes a Python 3 `pickle.dumps` adds — with
memoised values in one arena (a container filled after its `PUT` reads
back full wherever it is referenced, a reference cycle reads as `None`)
and old-style instances (`INST`/`OBJ`/`BUILD`) as `Item::Instance`; it
exists because `serde-pickle` 1.2.0 pops the class of an `OBJ` twice and
so cannot read a 2.1 `config.pck` whose `bounce_info` holds
`_BounceInfo` instances. `config21::Config21` is the list dictionary in
Mailman's terms (`bytes_to_str`, tuples as lists, instances and classes
as `None`) with `text`/`int`/`float`/`bool` (Python truth)/`text_list`/
`dict`/`text_dict`/`list`.

`import21::plan(config, list) -> Plan` is Mailman 3's
`mailman/utilities/importer.py`: the `SETTINGS` table (2.1 key → setting
→ `Kind`: text, bool, int, float, text list, seconds→days, an enum by
index, Mailman's autorespond triple), then the conversions Mailman does
by hand — `subject_prefix` with its space, the language only when the
site has it, `default_member_action` from `default_member_moderation` +
`member_moderation_action`, DMARC from the larger of `from_is_list` and
`dmarc_moderation_action` (unconditional when `from_is_list` wins),
`archive_policy` from `archive`/`archive_private` — plus `topics`,
`aliases` (each line anchored, the list's own name added), `bans`,
`header_matches` (Mailman's line parsing and `action_to_chain`),
`templates` (`convert_to_uri` names, `convert_placeholders`, nothing
written when the text equals the built-in default) and `rosters`
(members, digest members, owners, moderators, the `*_these_nonmembers`
addresses; the option bits, `delivery_status` codes, languages and
display names). Everything left out is a warning on the plan.

`import21::apply(db, list, plan, context)` writes it: the settings in
one audited `lists().update_with_context`, then each ban, header match,
template and member in its own audited write (`subscribe_with_context`
by address, `preferences().set_member_with_context`, the member's own
moderation action), what is already there skipped and counted, and one
`list.import21` audit event with the report.
`AuditRepo::record_with_context` is the standalone audit write it uses.
`crates/cli/src/import21.rs` is `listmngr import21 <list_id> <path>
[--dry-run]`.

## News to mail — bounded local acceptance verified

`listmngr_mail::nntp::Client` gained a session (`connect`: greeting,
`MODE READER`, `AUTHINFO`) shared by `post` and by `reader()`, which
returns a `Reader` (`group` → the `211` bounds, `head`/`article` → `Some`
of the multi-line block with its dot-stuffing undone as CRLF bytes, `None`
when the server answers `420`/`423`/`430` — no such article — and an error
for a closed, timed-out or otherwise broken session, which a caller must
not read past; `quit`; every command under the client's timeout). `nntp::inbound(article,
list)` is Mailman's per-article logic: `None` for an article whose
`List-Id` ends with `<list_id>` or that has no `From` address;
otherwise the article with `To` moved to `X-Originally-To` (any earlier
`X-Originally-To` dropped) and `To` set to the posting address, plus the
`From` address. `listmngr_runners::nntp::gate_news(db, config)` (public,
`GateReport` per list with `to_json`): the lists with `gateway_to_mail`
and a group, one reader session, `group` then `poll` — a `None`
watermark catches up to the group's last, else `max(watermark + 1,
first)..=last`, each article read and, when `inbound` gates it, turned
into the `in` job (`gated_message`: `{list_id, envelope_sender,
fromusenet: true}`, `external_id` the article's `Message-ID` or a minted
one), then `UsenetRepo::advance_watermark(list, from, to, gated)`: one
transaction that moves the watermark by compare-and-set (`WHERE
usenet_watermark = from`, or `IS NULL` for the first poll), queues the
gated article through `mail_queue::enqueue_tx`, and records
`usenet.watermark` with `from` and `gated`; `Ok(false)` — the watermark
was no longer `from` — is `PollStop::Contended` and ends that list's
poll with an error saying another poller moved it. An article the server
has not got (`None`) or whose header block `inbound` cannot read is
passed with a warning; a session error is `PollStop::Server`, which ends
the poll with the watermark where it was, and `gate_news` then reports
the remaining lists as `not polled` rather than driving a dead session.
The server being unreachable or a group unknown lands in the entry's
`error`. `set_watermark` remains for the settings surface. `nntp::run` calls `poll_and_log` at start and whenever
`gatenews_every_secs` has elapsed between claims (`0` never); `listmngr
nntp gate` (`crates/cli/src/nntp.rs`) refuses without a host and prints
the report.

## Mail to news — bounded local acceptance verified

`listmngr_core::NntpConfig` (`[nntp]`: `host`, `port`, `user`,
`password`/`password_file`, `remove_headers`, `rewrite_duplicate_headers`
as `"Source Target"` pairs; `validate` checks the port, the exclusive
password sources and the header names; `enabled()` is a non-empty host;
`credentials()` reads the file at use). The pipeline gains
`Target::Nntp` (snapshot at `to-usenet`), `FanOut::Nntp`,
`PipelineContext::from_usenet`/`Admission::from_usenet` and the
`ToUsenet` handler, in `default-posting-pipeline` after `to-digest`;
`listmngr_mail::handlers::plan_with(raw, list, id, from_usenet)` is what
the `in` runner plans with (`Posting::from_usenet` from the context's
`fromusenet`), and `Effect::Enqueue(FanOut::Nntp)` becomes a `Queue::Nntp`
child job (`processor::consumer`, five tries). `listmngr_mail::nntp`:
`prepare(raw, list, config)` (Mailman's `prepare_message` over a parsed
header block, kept fields copied as folded: `Approved`, the subject
through `without_prefix`, `newsgroups_header`, `message_id_header`,
`Lines`, `remove_headers`, `rewrite_duplicates` writing targets as
configured), `with_message_id`, `list_message_id`, and `Client` (`post`:
connect, `MODE READER`, `AUTHINFO USER`/`PASS` when configured — a
refusal reported without the password —, `POST`, `dot_stuffed` CRLF
article, `240` → `Outcome::Accepted`, `4xx` → `Outcome::Refused(reply)`,
anything else or a closed connection → an I/O error; thirty-second
timeout). `listmngr_runners::nntp::run` claims `Queue::Nntp` (idle
without a host), `process` runs `gate` under the lease heartbeat —
message, context, list, `Gated::Skipped` when the list no longer
gateways, `cook_with(Target::Nntp, …)`, `prepare`, the context's
`nntp_message_id` applied when present, `Client::try_new(config).post` —
and transitions: `ack` on `Posted`/`Skipped`, `refused` on a refusal
(`441` without a replacement yet: `MailQueueRepo::set_context_value`
records `nntp_message_id` and the job retries in a second; otherwise
`shunt` with the reply), `retry` with a doubling minute-based delay on an
error, nothing on a lost lease. `serve_mail_role` spawns it with the
other runners.

## Usenet gateway settings — bounded local acceptance verified

`listmngr_core::UsenetSettings` (flattened into `MailingList` like the
other groups) carries the five settings and `usenet_watermark:
Option<i64>`, with `NewsgroupModeration` a `string_enum`;
`is_newsgroup_name` is the validator (dot-separated components of
letters, digits, `+`, `-`, `_`, at most 255 bytes). Migration
`0053_usenet.sql` adds the six columns with Mailman's style defaults
(`usenet-schema.snapshot` joins the corpus). `ListRepo`: the keys route
through `USENET_SETTINGS` to `patch_usenet`, `persist_alter_messages`
writes the five settings and never the watermark, `usenet_from_row` reads
all six; `usenet_watermark` falls to the read-only refusal like any
unknown key. `UsenetRepo::set_watermark` (`Database::usenet()`) is the
gateway's write: the row and its `usenet.watermark` audit event in one
transaction. REST: `ListConfigInput` gains the five, the form
normaliser their booleans, and the `PUT` replacement drops
`usenet_watermark` with the other read-only fields. Pipeline:
`ListChecks::newsgroup_moderation`, the `news-moderation` rule
(`rules::NewsModeration`, "Post to a moderated newsgroup gateway") and
its `Defer` link between `max-size` and `no-subject` in
`default-posting-chain`; `policy_facts` copies the setting from the list.

## ARC sealing — bounded local acceptance verified

`listmngr_mail::arc` is the sealing side; `authenticity::Verifier` the
validating side. `Verifier::verifying_arc(true)` (set from
`mta.arc.enabled`) runs `verify_arc` at intake beside DKIM, SPF and
DMARC, appends `arc=` to the results when a client IP is known, and
reports `Verdict::arc_chain` (`Chain::None`, `Pass` or `Fail`, from
`Chain::of`); the `in` runner records it on the message context as
`arc_chain` next to `authentication_results` (`AcceptEffects::arc_chain`,
`Submission::arc_chain`). A recorded verdict is what makes a delivery
sealable: `prepare_post` sets `Admission::keep_arc` from it, and
`cleanse-dkim` (`cook::strip_signature_headers(raw, keep_arc)`) then
keeps the `ARC-*` fields it would otherwise drop, so the chain can be
extended. `Sealer::load` (an `ArcSealer` over an RSA key read like the
DKIM keys, relaxed/simple, `SIGNED_HEADERS` = Mailman's `sig_headers`
default plus `DKIM-Signature`, `From` oversigned) lives on
`MailRoleConfig::arc`; `Sealer::seal(bytes, results, chain)` works
without DNS: it parses the delivery, rebuilds the `ArcOutput` from the
sets found (`Set`/`Header::new`) with the recorded verdict as its result,
declines (bytes unchanged) when the last set says `cv=fail` or the
message does not parse, writes the intake's results whole as the
`ARC-Authentication-Results` value, and prepends the set mail-auth
produces. The out runner seals in `recipient_copies` and `sign_delivery`
after `SigningKeys::sign`, from `Prepared::arc` (`ArcFacts::of` the
context); owner mail, notices and digests never carry the facts.
`MtaConfig::validate` refuses `arc.enabled` without `authenticity_checks`
or without domain, selector and key; `webui_domains::dkim_records` adds
the sealing key's record on its domain.

## DMARC `wrap_message` — bounded local acceptance verified

`listmngr_mail::munge` now holds both delivery-time mitigations over a
post's bytes. `block` reads the header block once — every field
(lowercase name, unfolded value) and every raw line with the field it
belongs to, under the same limits and refusals as before — so `rewrite`
(`munge_from`) and `wrap` (`wrap_message`) share `author` (exactly one
safe `From` mailbox, else `UnsafeHeaderContent`) and `identity` (the
`Name (address) via list` `From` and the `Reply-To` that keeps the author
reachable). `rewrite` copies every raw line but `From`, `Sender` and
`Reply-To` and appends the generated pair; `wrap` returns the post
unchanged when its `From` is already the list, and otherwise copies only
Mailman's keepers (`kept_outside`: `mailman/handlers/dmarc.py` `KEEPERS`
plus `Cc` and `X-BeenThere`), adds `MIME-Version`, a `Message-ID`
derived from the post's SHA-256, the generated pair, and then either
`Content-Type: message/rfc822` (inline) with the post as the body, or a
`multipart/mixed` whose boundary is derived from the same hash: the
`dmarc_wrapped_message_text` through `digest::wrap` at seventy columns
as an inline `text/plain` part, then the post as an inline
`message/rfc822` part, 7bit or 8bit as its bytes are. The post's own
line endings are kept, the outer headers follow them, and the line break
before a boundary is written beside the post so the post keeps its own
last one. The `dmarc` handler (`listmngr_mail::handlers::Dmarc`, last
before `to-outgoing`) dispatches on `dmarc_mitigate_action` after the
same anonymity and conditional checks as before; the
`dmarc-mitigation` rule treats `wrap_message` as it treats
`munge_from`: tag, no hit. `DmarcMitigateAction::WrapMessage` in
`listmngr_core`; migration `0052_dmarc_wrap.sql` recreates
`mailing_lists.dmarc_mitigate_action` with the wider CHECK, as 0034 did
(the semantic schema snapshot is unchanged); the DMARC settings page's
select gains `web-ls-dmarc-wrap`.

## Subscription by mail, end to end — bounded local acceptance verified

`join_and_leave_by_mail_round_trip_their_confirmation_tokens` in
`crates/cli/tests/mailpath_e2e.rs` drives the flow through the fixture's
real sockets: `send_command` delivers a one-line command over LMTP to a
`-join`, `-confirm` or `-leave` address and requires the durable `250`;
`next_mail_to` waits on the SMTP sink for the next mail to an address
with a given subject, counting from the deliveries already seen, and
names every delivery (envelope, recipients, subject) when it does not
come; `challenge_token` reads the token from `Subject: confirm TOKEN` and
checks the challenge's `Reply-To`, `To`, null reverse path and
`Auto-Submitted`; `roster` reads `/3.1/lists/{id}/roster/member`. The
test then asserts the sequence — challenge, no member; reply, member,
receipt and welcome; a post delivered with the prefix; a second
challenge with a different token; reply, no member, goodbye; a spent
token replayed, nothing.

The one change it forced is in `WorkflowRepo::request_owned`
(`crates/db/src/workflows.rs`): the per-address hourly cooldown query now
carries `consumed=0`, so a request whose token has been spent no longer
counts against the next one. The bound it exists for is unchanged — one
outstanding challenge or moderator-queue row per address per hour — and
`global_and_address_notice_limits_are_durable_and_bounded` still holds
(a leave while a join waits is dropped).
`a_confirmed_join_does_not_block_the_leave_that_follows` pins the new
case on SQLite and, as `postgres_confirmed_join_then_leave_contract` on
an `IsolatedSchema`, in `scripts/test-postgres.sh`.

## Digest snapshot — bounded local acceptance verified

`listmngr_mail::digest` is a port of Mailman's two digesters
(`mailman/runners/digest.py`, 3.3.10) rather than a layout of its own.
`build` wraps the masthead (`wrap`, Mailman's
`mailman.utilities.string.wrap`: unindented paragraphs filled by a port of
Python's `TextWrapper` with `break_on_hyphens=False` and
`fix_sentence_endings=True` — tabs expanded to eight columns, two spaces
after a sentence end, a word longer than the column broken at it —
indented paragraphs copied verbatim), applies `decorate`'s rule to the
header and footer (a whitespace-only template is nothing), and builds the
table of contents as `add_to_toc` does: the subject through `oneline`
(RFC 2047 decoded and unfolded, via `mail_parser`'s public
`parse_unstructured`), the list's `subject_prefix` removed by the same
`(re:? *)?(prefix)` match, the first `From` mailbox's name or address in
parentheses on the last line when it fits in seventy columns, the line
wrapped at sixty-five. `plaintext_body` is `RFC1153Digester`'s sequence
of `print` calls: for each post `Message: n`, then each header of
Mailman's default `plain_digest_keep_headers` in that order, wrapped at
seventy with `\n\t` continuations, then `scrub` — every leaf part in
document order (nested `message/rfc822` parts descended), `text/plain`
as text, anything else as the `A message part incompatible …` note with
the attachment name, lowercased type, decoded size and description, the
parts joined by the `next part` line. `mime_body` is `MIMEDigester`: the
texts as `text/plain` parts with `Content-Description`, the posts as
`message/rfc822` parts with `Message: n` inserted after their own
headers (`with_message_header`), no closing part. `Digest` carries
`subject_prefix`; `DigestIssue` and `load_settings` in
`listmngr_db::digests` read it from the list row with the display name.

The evidence is `crates/mail/tests/digest_snapshot.rs` over fixtures that
`tests/compat/generate_mailman_digest.py` produced by running Mailman
core itself (`uv run --python 3.12 --with mailman==3.3.10`): a
`ConfigLayer`-style test configuration with an in-memory database and a
temporary var directory, a domain and a list made through Mailman's own
interfaces, the header and footer set as `file://` template URIs, the two
posts added to the list's `digest.mmdf` mailbox, and the digesters'
`finish()` results written as bytes. The test parses both sides with
`mail_parser` and compares text, so the volatile `Date`, `Message-ID` and
boundaries stay out of it, and names the three Mailman quirks it does not
copy (its 8-bit scrubbing, the joined `next part` line, the single
encoded word for an 8-bit `From`).

## Real-client suite — bounded local acceptance verified

`tests/compat/mailmanclient_suite.py` is a straight-line script — the
doctest's calls in the doctest's order, each `check(label, actual,
expected)` counted, each place listmngr differs a `deviation(...)` that
asserts listmngr's behaviour and is printed at the end — run by
`scripts/test-mailmanclient.py` after the Phase 1 flow and the held flow
(the held flow counts deliveries on the SMTP sink, so it goes first). The
gate keeps the fixture server's stderr in the temporary directory and
prints its tail when a flow fails.

On the compatibility flavour the REST layer gained, in
`crates/api/src/lib.rs`: `user_value`, `address_value` and
`resolve_user_id` (an id or an address); `JsonOrForm` on `users`,
`users/{id}` PATCH (with `cleartext_password` set through
`set_password_with_context`), `users/{id}/addresses` POST, every
preferences writer, `lists/{id}/config/{attr}` and `members/{id}`, with
`preference_bool` reading `True`/`False`; `members_list`,
`roster_mass_unsubscribe` (`mass_unsubscribe_emails` reads the repeated
form key or a JSON list), `lists_find`, the `role_routes!` handlers for
owner, moderator and nonmember, `list_archivers_set`,
`user_preferred_address{,_set,_unset}` and `address_delete`; Mailman's
shapes for styles, pipelines, chains, the configuration index and the
archivers; and `domain_lists` honouring `?advertised`. `crates/api/src/
requests.rs` adds `when`. In the database crate: `AddressRepo::
add_to_user_with_context` (create, adopt, or absorb) and
`delete_with_context` (a conflict while memberships use the address);
`UserRepo::set_preferred_address_with_context` and `create_with_context`
adopting a bare subscriber's address; `MemberRepo::all` and a patchable
`moderation_action`; `ListRepo::delete_tx` removing the list's
`moderation_log` and `held_messages`; and `ResponseAction` renamed to
Mailman's `RespondAndContinue` on the wire while the row keeps migration
0037's `respond` under its CHECK constraint (`stored_response_action`,
`response_action_from_stored`). `listmngr_pipeline::rules` names the two
moderation reasons as constants with Mailman's texts.

Two general repairs came out of the gate. `ApiError`'s response for a
server error now logs `correlation_id` and the error before answering, so
the id a client is told to quote leads somewhere. And SQLite contention:
`Database::connect_with_security` runs `PRAGMA foreign_keys = ON`, `PRAGMA
busy_timeout = 5000` and, for a file, `PRAGMA journal_mode = WAL` on every
pooled connection through `after_connect` (the pragmas are per connection;
before, only the connection that happened to run them had foreign keys
on), and `Database::write_tx` — now behind every repository transaction —
issues `BEGIN IMMEDIATE` on SQLite, as `browser_write_tx` already did,
because a deferred `BEGIN` that reads and then writes is refused at once
when another writer committed in between, however long the busy timeout.

## Phase 5 acceptance — bounded local acceptance verified

`crates/archive/tests/hyperkitty_parity.rs` is the external check the
phase asked for. Its fixture,
`crates/archive/tests/fixtures/hyperkitty/mailman-users-2025-03.json`, was
built from HyperKitty's own mbox export of `mailman-users@mailman3.org`
for March 2025 (the reference headers of each message) joined with the
record HyperKitty's REST API returns at `/api/list/<list>/email/<hash>/`
for the hash this archive computed (the answers: `message_id_hash`, the
thread, the parent). `every_message_id_hash_agrees_with_hyperkitty` checks
`listmngr_mail::message_id_hash` against all 138;
`threading_agrees_with_hyperkitty_after_a_real_import` rebuilds the month
as an mbox with stub bodies, imports it through `mbox::import`, reads it
back through `read_browser` and compares each post's thread and visible
parent, counting a difference as explained only when HyperKitty's root or
parent is a February post the month does not hold.

The divergence it found is closed by two helpers in
`crates/db/src/archive.rs` shared by `ArchiveRepo::complete` (the runner)
and `import_batch` (the importer). `resolve_thread` returns the thread of
the first archived candidate among the provisional root and the parent,
else the provisional root when posts are already filed under it (data
archived before this change), else the post's own hash. `adopt_orphans`
runs after the insert: two `UPDATE`s move to the new post's thread every
row filed under its hash as a provisional root (`archive_thread`) and
every row whose thread is the hash of a reply to it that had been its
own root (`archive_parent`, then `archive_thread`) — that reply's whole
subtree, since a self-rooted thread's rows all carry its hash. They are
two statements because one `OR` between the two conditions made SQLite
scan the table per archived post, which turned the hundred-thousand-post
import from seconds into more than the ten minutes the phase allows.
`crates/archive/tests/archive.rs::verify_orphans_adopted_by_a_late_parent`
pins the rule on both backends.

## Remote archivers — bounded local acceptance verified

`ArchiversConfig` (`[archive] archivers`: `mail_archive_address`,
`mhonarc_command`, `prototype_path`) is empty by default, and an empty
field switches its archiver off however `list_archivers` is set. The
address is carried on the database handle by
`Database::with_mail_archive_address`, beside `with_base_url`, and applied
where the handle is built in `crates/cli/src/main.rs` and
`crates/api/src/lib.rs`; the command and the path reach the archive runner
as `listmngr_archive::archivers::Settings`, taken from the configuration in
`serve_mail_role` before it moves into the `in` processor.

`ArchiveRepo::complete` queues the `mail-archive` copy itself:
`queue_mail_archive` checks `archiver_on` and, for a list whose policy is
`public`, calls `insert_child_job` for `Queue::Out` with the configured
address and records `archive.archiver` — both inside the transaction that
stores the archived post and acks the lease, so the copy cannot exist
without the post or the post without the copy.

`listmngr_archive::archivers::run` covers the two that act outside the
database. It reads the list's toggles through `Database::archivers` and the
published bytes through the new `ArchiveRepo::archived_copy` (no policy,
`hidden_at IS NULL`), then runs `mhonarc` — `tokio::process::Command` over
the configured argv with `expand` substituting `$listname`, `$hostname` and
`$hash` per argument, the message on standard input, output discarded,
`kill_on_drop` — and `prototype`, which writes `<root>/<list>/tmp/<hash>`
and renames it to `new/<hash>`. A failure of either is logged and reported
as an archiver that did not run. The archive runner's `archive_and_index`
calls `forward_to_archivers` after a post is stored and indexed;
`archived_post` resolves the list and hash from the queue message through
`index_row_for_message`, which already reports nothing for a post the
archive did not store.

## Archive administration — bounded local acceptance verified

Migration `0051_archive_admin` adds one nullable column,
`archive_messages.hidden_at`. Every reading path filters `hidden_at IS
NULL`: `read_selection` and the REST/browser read in `browser_archive.rs`,
the thread aggregate and its three correlated subqueries, the overview's
counts, months and top posters, the sender list, a sender's posts, the
recent posts, the vote and thread existence checks in `archive_interact`,
`export_rows`, and both search-index row queries (`INDEX_ROW_SQL`,
`INDEX_ROWS_SQL`). The two message reads also report `parent_hash` only
when the parent is itself visible, through a correlated `EXISTS` over the
same table, so no page links to a post the reader may not open. The
importer's thread lookup and the reattach helpers are deliberately not
filtered: they work on structure, not on what is shown.

`crates/db/src/archive_admin.rs` holds the owner's operations, each one
taking `Database::browser_owner_tx` inside a `browser_write_tx`,
re-checking the session with `browser_user_tx` before commit and writing
its audit event in the same transaction. `browser_hide` sets or clears
`hidden_at` for a `Scope::Message` (`hash`) or a `Scope::Thread`
(`thread`) and audits `archive.hide`/`archive.unhide`. `browser_delete`
routes to `delete_message` — the replies take the deleted post's parent,
`forget_posts` removes the attachments, votes and the row, and when the
root goes the oldest survivor becomes the new thread with
`move_thread_marks` carrying the tags, category, favourites and views over
— or to `delete_thread`, which takes every post of the thread and drops
its marks; both audit `archive.delete`. `browser_categories` applies a
`CategoryChange` (add, rename, remove) over `archive_categories` and
`archive_thread_categories` and audits `archive.category`;
`normalize_category` is `normalize_label` at sixty characters, the
function `normalize_tag` now shares. `browser_administration` reads the
categories with their thread counts and the hidden posts for the page.

`crates/api/src/webui_archive_admin.rs` serves `GET …/archive/admin` and
the three CSRF-checked forms `POST …/archive/categories`,
`…/archive/hide` and `…/archive/delete`, each behind `load` and
`privileged` with the owner check left to the repository. Hiding redirects
to the administration page, because the post it hid can no longer be read;
showing redirects to the post; deleting redirects to the thread list,
whose `PageQuery` gained a `saved` field for the notice. `links_for` adds
the administration link for an owner, and `admin_forms` builds the
`AdminForms` that `archive.html` renders on each post and in "About this
thread" — for an owner with a session and for nobody else.

## Archive import and export — bounded local acceptance verified

`listmngr_archive::mbox` is the `mboxrd` codec and the importer.
`write_message` writes one message under a fixed `From ` separator and
quotes every body line that would otherwise end it; `Reader` streams a
`BufRead` back into messages, consuming the separator that opens each
message exactly once (the line that ends one message is held in `pending`
and opens the next) and unquoting `>From ` by one `>`, so an archive of a
hundred thousand posts never sits in memory. `prepare` parses a message
once, outside the database, into the `ImportItem` the batch inserts:
`listmngr_archive::identity` for the hash, provisional thread, parent and
date, `listmngr_db::archive::sender_of` for the sender, and
`listmngr_mail::attachments::stored` for the attachments; a message with
no usable `Message-ID` is given `<import.<sha256 of its bytes>@<mail
host>>`, prepended to the stored copy so the hash and the headers agree.
`mbox::import` checks the list's policy through
`ArchiveRepo::ensure_importable` before it reads a line, then fills a
batch and hands it over.

`ArchiveRepo::import_batch` (`crates/db/src/archive_import.rs`) stores one
batch in one transaction: the archive policy re-read inside it
(`importable`), a thread lookup per message so a reply joins its root's
thread, `INSERT … ON CONFLICT(list_id,hash) DO NOTHING` (a conflict counts
as skipped), the attachments, an `UPDATE` re-rooting replies that arrived
before their parent, and one `archive.import` audit event — the rows and
the event commit together. `export_rows` pages by `(created_at, hash)`
with an optional thread or `[from_ms, until_ms)` date window and projects
each stored copy through the same `publication` (`handlers::cook_with`
with `Target::Archive`) the archive pages use, so anonymising and the
list's prefix apply to an export as well. `browser_export_authorize`
applies the archive policy for the session once, before the first page.

`crates/api/src/webui_archive_export.rs` serves `…/archive/export.mbox`
and `…/archive/export.mbox.gz`: `selection` turns `?thread=`/`?month=`
into an `ExportSelection` and the file name's middle part (both together,
an unknown parameter, a malformed month or an over-long thread id are
400), the session is authorized once, and a spawned task streams pages of
two hundred messages into an `mpsc` channel that becomes the response
body, through a `flate2` encoder for the `.gz` address. `crates/cli/src/
archive.rs` gains `archive import` (a `.gz` file read through
`flate2::read::GzDecoder`, progress on standard error every ten thousand
messages) and `archive export` (the same selection from `--thread`,
`--month`, `--gzip` and `--output`), neither of which touches the search
index.

## Posting from the web — bounded local acceptance verified

`listmngr_mail::web_post::compose` builds the message a web post becomes
(`mail-builder`: `From` the member's address with the account's display
name unless it repeats the address, `To` the posting address, `Subject`,
`Date`, `Message-ID`, `In-Reply-To` and `References` for a reply,
`User-Agent: listmngr-web`, a `text/plain; charset=utf-8` body). The
database's `browser_poster` (`crates/db/src/web_post.rs`) picks the
signed-in reader's posting address: their verified addresses in primary
order, the first that holds a membership (member, owner or moderator) on
the list, refused when none or when banned. `webui_archive_post.rs`
(`form`, `submit`; the route alone carries a 96 KiB body limit inside
`archive_routes`) checks the archive policy for the session, reads the
parent for a reply through `read_browser_message` (quoting its body and
stripping the list's subject prefix), validates inline, composes, and
enqueues into `Queue::In` through the same `enqueue` as LMTP intake and
REST injection, with the context `{version, list_id, envelope_sender,
message_id_hash, web_post: {user_id, address, reply}}`.

In the runners, `policy_facts::WebPost::from_context` reads that object
and `approve_web_post` sets `SenderChecks::is_approved` — the `approved`
rule's fact — only when the envelope sender is the recorded address, the
address is not banned, and the member's effective moderation action
(their own override, else the list's `default_member_action`) is `defer`
or `accept`; `processor::admit_post` applies it after `gather_context`,
so the posting chain, the pipeline, delivery and archiving are otherwise
unchanged. The `Approved:` header path and its moderator-password check
are untouched.

## Archive interactions — bounded local acceptance verified

`ArchiveRepo::interact` (`crates/db/src/archive_interact.rs`, migration
`0050_archive_interactions.sql` adding `archive_votes`, `archive_tags`,
`archive_categories`, `archive_thread_categories` and `archive_favorites`)
owns readers' actions. Each write opens a `browser_write_tx`, takes the
live user behind the session and admits them by the archive's policy
(`reader_tx`: any signed-in reader of a public archive, a verified member
of a private one, an owner through `browser_owner_tx` for categories and
for removing another reader's tag), checks the post or thread exists,
applies the change (delete-then-insert, so one row per reader and post, or
per thread and tag), records the audit event (`archive.vote`,
`archive.tag`, `archive.category`) and re-checks the session before the
one commit. Favourites are a reader's bookmarks and carry no audit event.
`normalize_tag` folds a tag to lowercase letters, digits and hyphens.
Reads (`browser_votes`, `browser_thread_meta`, `categories`,
`label_threads`) are plain bounded queries; the thread aggregate in
`archive_browse.rs` takes three more filters (`ThreadSelection::Favorites`,
`Tagged`, `InCategory`) as `IN (SELECT …)` clauses on the same query.

In the API, `webui_archive_interact.rs` holds the four `POST` handlers
(`vote`, `tag`, `category`, `favorite`), each a CSRF-checked form from a
signed-in session redirecting to the post's permalink or the thread page
with a `saved=` notice; `webui_archive.rs` gathers the page's scores and,
on a thread page, the thread's meta (`interactions`), renders the vote
form on every post for a signed-in reader, the "About this thread" section
(`meta_view`: tags with remove buttons for their tagger or an owner, the
owner's category select, the favourite toggle), and the three new thread
lists (`favorites`, `tagged`, `in_category`) through the one
`threads_page` and its `Listing`. The archive routes moved into
`archive_routes()`.

## Archive browsing — bounded local acceptance verified

`ArchiveRepo::browse` (`crates/db/src/archive_browse.rs`, migration
`0049_archive_views.sql`) owns the reading views behind the archive's
browsing pages. `browser_authorize` applies the list's policy once per
request (public to anyone; private to a verified member checked inside a
`browser_write_tx` against the live session; `never` is not found).
`browser_overview` counts posts, threads and distinct senders, buckets the
post dates into months in Rust (portable across SQLite and PostgreSQL;
capped at 200 000 dates), and lists the latest threads, the threads with
most posts in the last thirty days, and the senders with most posts in that
window. `browser_threads` pages one bounded query grouped by thread
(`ThreadSelection::{Latest, Between, Active}`, twenty-one rows so the page
knows whether a next one exists), joining the reader's rows of
`archive_thread_views` to mark threads with posts newer than their last
visit; `browser_mark_viewed` records the visit (a per-reader bookmark, not a
business write, so it carries no audit event). `browser_senders` and
`browser_sender_posts` back the sender page, whose address is found by
comparing SHA-256 digests in the handler so the address never appears in a
URL; `browser_latest_posts` feeds the feeds. Every post these views return
passes through `read_browser_message`, so rendering and attachment metadata
are the same as on any page.

In the API (`crates/api/src/webui_archive.rs`) the browse handler is split
into `browse_with` (shared by `?thread=` and the canonical
`/archive/thread/{hash}` page, which also refuses absent threads), a
`Viewer` (language, address visibility, rendering mode, ownership, CSRF)
built once per request, and the new handlers `overview`, `threads`,
`threads_month`, `sender`, `feed_atom` and `feed_rss`. The search page
passes the index's `Results::total` to the template and marks the query's
words through `listmngr_archive::render::highlight`, a scanner over the
renderer's own safe HTML that copies tags and character references
verbatim and wraps whole-word matches in text nodes with `<mark>`. Feeds render
through the askama templates `feed_atom.xml` and `feed_rss.xml` (the
crate's escaper now also covers `.xml`), with absolute links from
`site.base_url`, sender names only and obfuscated 500-character summaries,
and are served with `nosniff`; no handler builds markup by hand, as
`webui_shell.rs` enforces. Templates `archive_overview.html`,
`archive_threads.html`, `archive_sender.html` and the shared
`archive_macros.html` (the sub-navigation and the thread table) live in
`crates/web/templates`.

## Archive search — bounded local acceptance verified

`listmngr_archive::search` owns the tantivy index: a fixed schema (`key`
= list + NUL + hash for replacement, `list`/`hash`/`thread` as raw terms,
`subject`/`body`/`sender` as tokenised text, `date` as a fast i64),
`SearchIndex::open` (create or open, refusing another schema), a reader on
`ReloadPolicy::OnCommitWithDelay`, `search` (a `BooleanQuery` of the list
term, the lenient conjunctive parse of the reader's words, and optional
thread and date-range clauses, collected with `TopDocs` and `Count`), and
`Writer` (add-or-replace by key, remove, remove-list, clear, commit,
`commit_if_due`). `ArchiveRepo::index_row_for_message` and
`index_rows_after` read what the index stores without policy;
`listmngr_archive::index_document` and `reindex` convert them. The archive
runner (`crates/runners/src/archive.rs`) receives the one `Writer` behind an
`Arc<Mutex>` from `serve_mail_role`, adds each stored post after its job
completes (in `spawn_blocking`), commits when the batch is due and on
shutdown; the API opens a reader lazily (`AppState::search_index`) once
`meta.json` exists and otherwise searches the database. `listmngr archive
reindex` (`crates/cli/src/archive.rs`) clears and rebuilds through
`reindex`. The `ordered-float` transitive dependency is pinned to 5.4.0 in
`Cargo.lock` for the 1.88 toolchain.

## Archive rendering — bounded local acceptance verified

Migration `0048_archive_render` adds `sender_name`, `sender_email`,
`message_date` and `parent_hash` to `archive_messages` and the
`archive_attachments` table (bytes as `BYTEA`, keyed by list, hash and
position). `listmngr_archive::identity` derives the hash, provisional
thread, parent and date from the reference headers (the runner used to do
this inline); `ArchiveRepo::complete` stores the sender and date from the
cooked copy and the attachments through the new
`listmngr_mail::attachments::stored` projection, and `render_rows` reads
the new columns with attachment metadata joined afterwards.
`listmngr_archive::threading::order` is the pure tree order (roots and
siblings by date then hash, cycles placed once) the browser applies to
`read_browser_thread` (500 posts); `listmngr_archive::render` produces every
byte of a body's HTML — text with folded quotes and linkified URLs, or
Markdown through `pulldown-cmark` events written into a fixed safe subset —
and obfuscates addresses for visitors. `browser_reattach` moves a post and
re-roots its descendants in one audited transaction under the owner's
authority. `webui_archive.rs` serves attachments from their own path with a
type deny-list, proxies Gravatar through a bounded, cached `reqwest` fetch
when `[archive] gravatar` is on, and widens the CSP to `img-src 'self'` for
those pages only.

## Phase 4 acceptance — bounded local acceptance verified

`chromium_acceptance_journey` in `crates/api/tests/webui.rs` starts the
router on a loopback port with a disposable in-memory database, launches
`scripts/test-webui-journey.py` under the Playwright venv, and runs a bridge
loop beside it: every token the site mails (the `Token:` line of a list
confirmation, the token after "enter this token:" of a verification mail)
is written once to `.journey/token-<n>` in the evidence directory; the
journey account is seated as a domain owner once its address is verified
(`owner-granted`); a nonmember post is held on the new list once the friend's
subscription is confirmed (`post-held`, through the shared `held_on` seed).
The script waits for those files, never for time. After the browser exits
the harness asserts the database state. Lighthouse runs as a subprocess of
the script with `CHROME_PATH` set to Playwright's Chromium and the session
cookie passed as an extra header for the signed-in page; the JSON report is
rewritten to the score alone so no cookie reaches the evidence folder. The
CI `browser` job pins Playwright, Chromium, axe-core (by SHA-256) and
Lighthouse and runs both harness tests.

## Data export and erasure — bounded local acceptance verified

`crates/db/src/web_gdpr.rs`: `export_user` reads the account, its
preferences rows (through the owner rows' `preferences_id`), addresses,
memberships, `api_tokens` metadata columns only, `domain_owners`, the
`web_sessions` count and up to a thousand `audit_log` rows where the
account is actor or target, as one JSON document with a `format` marker.
`erase_user` (command line, administrator) and `browser_erase_user`
(server owner under live session authority) share `erase_tx`: the
last-server-owner guard, `web_delete::remove_everything` (now
crate-visible) and one `user.delete` event with `"by": "administrator"`,
all in the caller's transaction. `crates/api/src/webui_gdpr.rs` serves the
downloads with `Content-Disposition: attachment` and the erase form; the
CLI's `user export` and `user erase` call the same two functions.

## Cross-list moderation — bounded local acceptance verified

No new persistence: `webui_moderation::index` walks the moderated lists of
the page (`browser_moderated_lists`) and, for each with something waiting,
reads its first held page (`browser_held_queue`) and its requests
(`browser_requests`), capped at fifty of each, so authority is the list's on
every read as on every write. The held and request articles are Askama
macros (`held_article`, `request_article` in `macros.html`) shared by the
list queues and the cross page; the macros take the CSRF token, the
decisions, whether a bulk form exists and a `back` marker. The decision
handlers accept `back=moderation` and redirect to `/web/moderation` instead
of the list's queue; nothing else about them changed.

## System page and audit log — bounded local acceptance verified

`crates/db/src/web_system.rs` holds two read projections under
`server_owner_tx`: `browser_runner_status` (the queue statistics `/metrics`
and `queue stats` use, plus `locked_by` counts of leased jobs) and
`browser_audit` (a page of `audit_log` rows newest first, `LIKE` on an
escaped action prefix and target substring). `crates/api/src/webui_system.rs`
flattens `Config::redacted_json` into dotted keys (the same redaction the CLI
and REST use, so a secret never reaches a page), reads the MTA map writer's
`current` link for the published generation, and renders the two pages
through `SystemPage` and `AuditPage`.

## Domains and accounts — bounded local acceptance verified

`crates/db/src/web_domains.rs` and `web_users.rs` hold the server owner's
site-wide operations. `server_owner_tx` (from `web_list_settings.rs`, now
crate-visible) is the one authority check. Reads run on the pool after the
check commits; writes that a repository already owns — domain create and
delete, owner add and the new `DomainRepo::remove_owner_with_context` — check
authority first and let the repository commit the business row with its
audit event; the rest (`browser_user_update`, `browser_user_address_verify`,
the domain template set and delete through `TemplateRepo::set_body_tx` and
`delete_tx`) are one browser write transaction each. Domain template
resolution for the editor is `templates::resolve_domain_tx` (domain scope,
then site, then built-in), a sibling of the list and site resolvers.
`listmngr_mail::dkim::SigningKeys::dns_record` derives the public key of a
configured signing key (`rsa` PKCS#8 or PKCS#1 DER after the same file
checks as `load`) and returns the `<selector>._domainkey.<domain>` name with
its `v=DKIM1; k=rsa; p=…` value. `crates/api/src/webui_domains.rs` and
`webui_users.rs` are the handlers; the domain editor reuses the
`TemplateEditor` view with a two-link group navigation.

## List creation and the directory — bounded local acceptance verified

`crates/db/src/web_lists.rs` holds the directory read, the reader's standing
and list creation. `browser_directory` is one bounded query per scope: a
visitor's advertised-only listing, a signed-in reader's advertised listing
with their roles joined afterwards, or — with `show_all` — a listing that
also admits lists the reader has a verified-address role on (every list for
a verified server owner); the search is a `LIKE … ESCAPE '!'` pattern over
the id and the lowercased display name and description, the domain filter is
an exact `mail_host`. `browser_standing` and `browser_list_summary` decide
whether an unadvertised list is a page for this reader. `browser_creatable_domains`
runs under the browser write transaction and live session authority: every
domain for a server owner, otherwise the `domain_owners` rows of the reader.
`browser_create_list` is one transaction: domain existence (`NotFound`),
authority (`Forbidden`), a free id (`Conflict`), `ListRepo::create_tx` (new:
the former `create_with_context` body inside a caller's transaction, the
domain checked there), `ListRepo::update_tx` for `advertised` and
`description` so the REST validator applies, the first owner row (the address
created when unknown, as `subscribe_with_context` would) and the three audit
events, then the session re-checked before commit.
`crates/api/src/webui_lists.rs` holds the directory, summary and create
handlers; the directory issues no cookie, the summary checks visibility
before it issues one, and the create handler maps `Conflict`/`NotFound`/
`Validation` to inline refusals and `Forbidden` to a 403. `Pagination::filtered`
keeps an encoded query string on page links. The create form renders through
a `control` macro shared with no other page yet.

## Moderator queues — bounded local acceptance verified

`crates/db/src/web_moderation.rs` holds the moderator's reads and writes.
`moderator_tx` is the one authority check (server owner, or owner/moderator
of the list through a verified address), shared with the older
`browser_review`. `browser_held_queue` reads a page of held rows joined with
their bounded source, parses each with `mail_parser` for the decoded address
headers and the first text body, and looks up the sender's row and ban.
`browser_review_many` runs `ModerationRepo::review_tx` per selected post in
one transaction, skipping posts already decided or not on the list;
`browser_moderate_sender` sets the member's override or creates a nonmember
row; `browser_ban_sender` uses `BanRepo::create_tx`; `browser_decide_request`
uses the new `WorkflowRepo::decide_tx` after checking the request belongs to
the list. The page's script is the second first-party script
(`Shell::with_script`), served like the passkey one; the shell now records
one script path rather than a flag.

## Member management — bounded local acceptance verified

`crates/db/src/web_members.rs` holds the owner's member operations.
`browser_roster` reads one role's page with the member-level preferences
joined in; `browser_member_options` updates the member row and its
preference row (`PreferencesRepo::set_tx`) with one `member.update` event;
`browser_member_bounce_reset` clears the score and re-enables delivery that
bounces disabled; `browser_mass_remove` resolves ids and addresses to this
list's rows and deletes each through `workflows::delete_member_with_goodbye`
with its own `member.delete` event, all in one transaction;
`browser_export_members` is a bounded read. `browser_mass_subscribe` is the
one operation that is not a single transaction: after the authority check it
calls the registrar (`WorkflowRepo::subscribe` with `AdminSubscription`) per
member address, so the list's policy and the `pre_*`/invitation flags mean
what they mean on the REST API, and `MemberRepo::subscribe_with_context`
for the other roles; each address commits on its own and reports its
outcome, so a refused address never rolls back the accepted ones.

The roster page is the first progressive enhancement: `Shell::with_htmx`
loads the vendored `htmx.min.js` (and, through `scripted`, widens the CSP
to `script-src 'self'`), the search form carries `hx-get`/`hx-target`/
`hx-push-url`, and the handler renders only the `Roster` sub-template when
the request carries `HX-Request: true`. A `<meta name="htmx-config">`
switches off htmx's injected indicator stylesheet, which the `style-src
'self'` policy would otherwise refuse. The bulk-removal checkboxes belong to
a form outside the rows through the `form` attribute, so the per-row policy
forms stay valid HTML. The mass form accepts both encodings: the handler
reads `multipart/form-data` through axum's `Multipart` (the `multipart`
feature is new) and falls back to the urlencoded `Form`.

## List settings — bounded local acceptance verified

`crates/api/src/webui_list_settings.rs` describes the nine settings groups as
data: each group is a list of fields with a kind (`text`, `textarea`, one entry
per line, a select over wire values, a moderation action with `default` for
none, a whole number, a decimal). One template (`settings_group.html`) renders
any group from that description; one handler turns a submitted form into a
JSON patch keyed exactly as the REST configuration is, so `ListRepo`'s patch
engine is the only validator (`validate_patch` runs it on a copy for the
preview; `browser_update_list_settings` runs it on the locked row for the
save). A refused value is mapped back to its field by name and the form is
rendered again as a 400 carrying an internal marker header, which the browser
error-page middleware honours by leaving the body alone (it strips the header;
every other client error still shows the generic page). The diff a preview
shows compares the patch with the list's serialized wire values.

`crates/db/src/web_list_settings.rs` holds the owner's other writes. Each one
takes the browser writer reservation, checks the owner's live authority, and
calls a transaction-scoped variant of the repository operation the REST API
uses — `HeaderMatchRepo::edit_tx`, `BanRepo::{create,delete,site_create,
site_delete}_tx`, `TemplateRepo::{set_body,delete}_tx`,
`ListRepo::{set_archiver,delete}_tx`, `DigestRepo::bump_tx` — so the rows and
the audit event commit together and the REST and browser paths share one
implementation. Reads check authority in a short transaction and then query the
pool, because the SQLite test pool has one connection. Header-rule tests
evaluate the stored patterns with the pipeline's own compiler
(`compile_header_pattern`), so what the page says matches what the chain does.

## OpenID Connect sign-in — bounded local acceptance verified

`crates/api/src/oidc.rs` is the relying-party client: discovery cached per
provider, Authorization Code with PKCE (S256), `state` and `nonce` minted per
ceremony, the token exchange with `client_secret_post` and the code verifier,
and ID-token verification against the provider's JWKS (RS256 through `rsa`,
ES256 through `p256`; one key refresh on an unknown `kid`) for issuer,
audience, `exp`, `iat` and a constant-time nonce comparison. It is stateless:
the ceremony a browser is in the middle of is JSON on
`web_sessions.oidc_state` (migration `0047`), parked by
`browser_oidc_park` and taken-and-cleared by `browser_oidc_take`, so a
callback answers one ceremony once and a stray or replayed callback is a 400.
`crates/db/src/web_oidc.rs` resolves a verified identity in one transaction:
the account linked to `(provider, subject)` in `user_oidc`; else, for a
verified email only, the account owning that verified address (linked on the
spot) or a just-in-time account whose credential row holds a random Argon2
hash and `user_credentials.usable = 0` (set back to 1 by a reset or password
change). Like the password, a provider sign-in is half a login for an account
with TOTP enrolled (`LoginOutcome::SecondFactor`), and it audits `web.login`
or `web.login.password` with `method: oidc`. Linking runs the same ceremony
with purpose `link` from a CSRF-protected POST; unlinking re-verifies the
password when the account has a usable one and refuses the last way in
(no usable password, no passkey, no other link). Tests run a mock provider
(`crates/api/tests/webui/mock_oidc.rs`) on a loopback port that checks the
PKCE challenge and client secret and signs ES256 ID tokens.

## Passkeys — bounded local acceptance verified

`crates/db/src/web_passkeys.rs` drives `webauthn_rp` 0.3 (pure Rust, no
OpenSSL, so the static musl build is unchanged). The relying party is derived
from `site.base_url`, which `listmngr_api::router` now carries into the
`Database` handle along with the site name and owner. Migration `0046` adds
`user_passkeys` (credential id, `StaticState` and `DynamicState` in the
library's binary encoding, name, timestamps), `users.webauthn_handle` (the
64-byte handle a discoverable login returns) and `web_sessions.webauthn_state`,
where a started ceremony waits — prefixed by purpose, cleared as it is read,
so a response answers one challenge once and the library's five-minute expiry
applies. Registration requests `residentKey: required`,
`userVerification: required` and credential protection without enforcement;
verification pins the origin to `site.base_url`. A login rotates the anonymous
session into a signed-in one in the same transaction as the counter update and
the `web.login` audit event, and `browser_second_factor_missing` counts a
passkey as a second factor. The browser side is `crates/web/assets/passkeys.js`
using `PublicKeyCredential.parseCreationOptionsFromJSON` and `toJSON()`; the
handlers read the CSRF token from an `X-CSRF-Token` header and apply the same
Origin check as forms. Tests drive the ceremonies with a software ES256
authenticator (`crates/api/tests/webui/soft_authenticator.rs`) and Chromium
through CDP's virtual authenticator.

## Two-step sign-in — bounded local acceptance verified

`crates/db/src/totp.rs` implements RFC 6238 over HMAC-SHA1 with the RFC 4648
base32 alphabet, checked against the RFC's own test vectors; `verify` accepts
the steps `now-1..=now+1` that are later than the last accepted step, so a
code is used once. `crates/db/src/web_totp.rs` holds enrolment (a pending
secret in `user_totp` that only counts once a code confirms it), the ten
recovery codes (`user_recovery_codes`, digests only), the second step and the
password-guarded regenerate/disable. Migration `0045` adds the two tables and
gives `web_sessions` a `pending_user_id` and a failure counter: a correct
password on an enrolled account writes a session that is anonymous to every
page (`user_id` NULL) but names whom it is for and expires in ten minutes;
`browser_second_factor` rotates it into a full session in the same transaction
as the step or recovery-code consumption and the `web.login` audit event.
`webui::privileged` enforces `security.require_2fa_for`: a server owner without
a confirmed factor gets 403 from administration, moderation and token minting.
The QR code is an inline SVG from the `qrcode` crate, so the enrolment page
still loads nothing and runs no script.

## Account deletion — bounded local acceptance verified

`crates/db/src/web_delete.rs` re-verifies the password with Argon2 before the
writer reservation, rebinds the session under `browser_write_tx` +
`browser_user_tx`, refuses the last server owner with a verified address, and
then removes in one transaction: memberships through the user or any of their
addresses (with their preferences and one `member.delete` audit event each),
address rows (pending tokens cascade) and their preferences, API tokens,
credential, sessions, domain ownerships; `held_messages.moderator_id` and
`moderation_log.moderator_id` are set to NULL because those references are
`ON DELETE RESTRICT`; then the user row and its preferences, and one
`user.delete` audit event with counts. The REST `DELETE /users/{id}` keeps its
older semantics (unlink addresses, keep memberships) and is a separate
administrative surface.

## Own API tokens — bounded local acceptance verified

The API authorizes on a token's scopes and bounds alone, never on the roles
of the user behind it, so self-service minting had to be bounded by authority
or it would have been an escalation. `crates/db/src/web_tokens.rs` computes
`TokenAuthority` inside the writer transaction — a verified server owner, and
the lists the reader owns by the same `OWNED_LISTS` predicate the admin pages
use — and lets a non-owner mint only `LIST_SCOPES` bound to one of those lists,
while a server owner may mint `ALL_SCOPES` unbound (`admin` still unbound
only). `insert_token_tx` is the one place a token row and its `token.create`
audit event are written, shared with the CLI path; the secret exists in the
result and nowhere else. Revocation is an ownership-guarded `UPDATE` with its
`token.revoke` audit event.

## Own addresses — bounded local acceptance verified

`crates/db/src/web_addresses.rs` manages the reader's addresses under
`browser_write_tx` + `browser_user_tx`. Adding shares `issue_verification`
with signup (token row, hourly cooldown, site notice) and links an address
nobody owns while clearing any earlier `verified_on`, because verification is
proof for *this* account; an address another account owns commits nothing.
Removal unlinks (`user_id = NULL`) and clears `verified_on`, deletes the
address's pending tokens and keeps memberships, which reference the address
row with `ON DELETE RESTRICT` anyway. Signup's `claim_account` applies the
same rule to a verified address nobody owns: it is linked and must be proven
again, so an administrative verification can never turn into an account for
someone who did not read the mailbox.

## Password reset — bounded local acceptance verified

`crates/db/src/web_reset.rs` mirrors signup. `browser_reset_request` looks the
address up joined to its user and credential under `browser_write_tx`, commits
nothing when there is no verified account, and otherwise issues a
`password_reset` row in `account_tokens` (one per address per hour) and mails
`site:user:action:reset` in the account's own language through
`site_notices().enqueue_tx`, auditing `user.reset.request`.
`browser_reset_confirm` validates and hashes the new password before the
writer reservation, then consumes the token with one guarded `UPDATE …
RETURNING`, requires the address to still be verified and owned, replaces the
credential (bumping `password_updated_at`, which every session's
`credential_version` is checked against), deletes the account's sessions,
retires sibling tokens and audits `user.password` with the cause; the audit
layer redacts the `password` value as it does for every credential event.

## Signup and mailbox proof — bounded local acceptance verified

`crates/db/src/web_signup.rs` owns self-service account creation. Argon2 runs
before the writer reservation; inside `browser_write_tx` the anonymous session
that anchors the CSRF token is re-checked on the writer's own connection, the
address row decides what the signup is (`claim_account`: create; link an
address nobody owns; re-arm an account that was never proven; or, for an
address a verified account owns, do nothing), a 32-byte token is issued into
`account_tokens` (migration `0044`, digest only, 24-hour expiry, one per
address per hour) and mailed through `site_notices().enqueue_tx`, and one
`user.signup` audit event commits with all of it. `browser_verify_address`
consumes the token with a single `UPDATE … RETURNING` guarded on purpose,
expiry and `consumed_at`, verifies the address, retires sibling tokens and
audits `address.verify`. Enumeration resistance is structural: the silent
branches commit an empty transaction and the handler renders the same 202
page.

## Site notices — bounded local acceptance verified

`crates/db/src/site_notices.rs` produces mail the site sends to a person
outside any list. The envelope is the configured `site.site_owner` (carried by
`Database::with_site`, applied by the CLI from `[site]`), the Message-ID domain
is the owner's mail host, and `crate::templates::resolve_site_tx` resolves the
body at the site scope then the built-in, in the recipient's negotiated
language. The bytes go through the same `enqueue_raw_notice` path every
generated notice uses — store, message, `out` job, `workflow_notices` row — in
the caller's transaction, with a message context of `{"site": true,
"site_mail_host": …}` and no `list_id`.

The out runner already sends a notice's bytes as they are on the strength of
its `workflow_notices` row; the one list-coupled step, DKIM, now signs for
`site_mail_host` when the context has no `list_id` **and** the notice row
exists — a forged site context on a non-notice job is still invalid. DSN
binding and bounce recording were already no-ops without a `list_id`, so a
site notice binds no list delivery authority.

## Profile and interface language — bounded local acceptance verified

`crates/db/src/web_profile.rs` holds the one validator for a user's display
name, interface language and time zone (`Profile::validate`): non-empty,
bounded, control-free names; a language a shipped catalog serves; an IANA zone
name from `chrono-tz`. The browser form and the REST user patch both go through
it, so neither surface can store what the other would refuse. The browser write
runs under `browser_write_tx` + `browser_user_tx` with one `user.profile` audit
event in the same transaction.

`webui::reader_language` decides the language of a page a signed-in reader
opens: the profile locale when it names a shipped catalog, else what
`webui::language` negotiates for the browser alone. Anonymous pages, the
failure page and the compatibility archive path keep the browser negotiation.

## Session inventory — bounded local acceptance verified

`web_sessions` rows carry an opaque `id` and a `created_at` since migration
`0043`, so a reader can be shown their own sessions and name one for
revocation without the page ever carrying the token digest that authenticates
it. `crates/db/src/web_session_inventory.rs` reads them under a live-session
check and revokes under `browser_write_tx` + `browser_user_tx`, the same
authority every other browser write uses, with one `web.session.revoke` audit
event committed in the same transaction. Revoking the session making the
request also clears its cookie, so the browser stops presenting a credential
the server has deleted. The migration deletes existing rows instead of
backfilling an id from `token_hash`.

## Browser shell and one rendering path — bounded local acceptance verified

`crates/web` is the presentation crate: Askama templates (`crates/web/templates`)
with compile-time auto-escaping, the view models handlers fill in
(`crates/web/src/pages.rs`), the `Shell` that owns document language, title and
navigation state, the design-token stylesheet and the vendored htmx asset.
`crates/api/src/webui*.rs`, `unsubscribe.rs` and the compatibility archive page
build view models only; a test reads their sources and fails on any string
literal containing markup, so the two rendering paths ADR-0004 warned about
cannot come back.

Escaping is a project escaper (`listmngr_web::HtmlEscaper`, wired in
`crates/web/askama.toml`) that writes `&amp;`, `&lt;`, `&gt;`, `&quot;` and
`&#39;` — the exact bytes the hand-written escaper produced — so the migration
is byte-compatible and the existing browser assertions are the parity evidence.

Language is negotiated per request in `webui::language`: the reader's ordered
`Accept-Language` preferences (q-values respected, bounded to 16 tags), then
`site.default_language`, then English. The failure page rendered by the
`security_headers` middleware cannot reach the database, so it negotiates on the
request's own preferences alone. Nothing infers a language from the list, the
`Host` or a forwarded header.

htmx 2.0.10 is vendored and served from `/web/htmx.min.js`; a test pins its
SHA-384 and asserts no shipped asset refers to another origin. No page includes
it yet, so pages keep `default-src 'none'` and contain no script element.

`/api/docs` is part of the same guarantee since `P1-API-DOCS-ORIGIN`:
`crates/api/src/api_docs.rs` renders the API reference from the same OpenAPI
value `/openapi.json` returns, into a template, so the page cannot drift from
the API and an operator who opens it is not made to fetch code from a CDN. It
carries the browser security headers and the same strict CSP; the source-level
markup ban now covers every module of the `listmngr-api` crate.

## Aggregate final-audit authority — bounded acceptance verified

`DigestRepo::collect` preserves list-before-queue reservation order, captures the
persisted lease deadline under the queue lock before ACK clears the lease, then
uses the shared final clock check after the awaited `queue.ack` audit and directly
before commit. Expiry rolls back the digest post and queue/audit transaction;
heartbeat renewal is resolved from stored authority rather than the stale caller
snapshot. Unleased issue publication/bump semantics are unchanged. Focused
SQLite and actual PostgreSQL audit-wait controls pass. Leased workflow completion
uses its common commit helper after the earlier queue reservation, covering
child-producing and no-child early returns. Moderation hold captures authority
before ACK, then validates after held insertion and the later `moderation.hold`
audit. Unleased workflow APIs and moderation review remain unchanged.
Frozen `target/aggregate-authority-gates-20260910-045720/` passed all 25 gates,
workspace615/0/48 and mandatory PostgreSQL31/0/0; all 390 source/harness hashes
matched. The new PG matrix observes 8 digest, 24 workflow and 8 moderation waits.
Fresh-binary native digest/CLI/restart/SMTP evidence covers both database engines;
expiry injection remains a separate DB-layer fixture, not a running-service test.
No commit/fsync-latency guarantee or DSN issuance authority is implied.

## P2-DSN-PRODUCER-PREREQUISITE — isolated transport, no issuance authority

The default-off `mta.smtp_single_recipient` config is propagated to the mail
role's real outbound consumer. After cooking/signing once and reserving every
pending recipient through the existing audited `begin_delivery`, non-null list
mail uses sequential one-recipient SMTP sessions with the configured TLS/AUTH
policy on each connection. Default batching and null-envelope batching remain.
Ordered transport outcomes are committed together through existing fenced
`finish_delivery_with_smtp`; known transient outcomes alone are eligible for
retry. Cancellation/failed final commit retains conservative ambiguity for all
reserved recipients, even a recipient whose SMTP session has not begun. There
is no new DB schema, issuance record, HMAC, token intake or DSN mutation authority.
Frozen local acceptance passed 23/23 gates, including actual API→LMTP→SMTP
default/enabled/disabled modes across process restarts on SQLite and PostgreSQL.
Exact socket/restart scope, costs and remaining durable-authority
requirements: [DSN_PRODUCER_PREREQUISITE.md](DSN_PRODUCER_PREREQUISITE.md).

## P2-DELIVERY-AUTHORITY — final-audit lease fencing

Shared queue methods re-sample the live clock after their last audit/write and
before commit. Existing authorities use the current deadline read under the
queue lock, not the caller's stale lease snapshot; claim and heartbeat use the
newly granted/renewed deadline. At exact expiry, the complete transaction rolls
back. The outbound runner returns without SMTP commands when reservation fails.
This is a pre-commit boundary, not a promise about subsequent commit/network
latency. Archive's separate producer increment captures the current deadline
under the queue lock before ACK clears authority, then invokes the shared final
check after ACK's audit and immediately before commit. List-before-queue lock
order, explicit-time fixtures, live production clock propagation, thread repair,
idempotent indexing and the current `never` policy are preserved. Exact expiry
rolls back inserted archive rows, existing-reply thread updates, index/ACK audits
and queue completion. Digests/workflows/moderation free-ACK callers still need
their own final-audit increments.

Archive-only evidence in `target/archive-final-audit/`: behavioral SQLite and
observed native PostgreSQL 14 RED→GREEN; eight audit-wait cases covering normal
and renewed deadlines, exact expiry and valid controls, public/never policies.
Focused shared/sibling queue tests, archive/runner tests, fmt and DB strict Clippy
pass. Final `target/archive-authority-gates-20260910-034840/` passes 25/25 gates,
workspace612/0/45 and mandatory PostgreSQL28/0/0, with 382 stable paths.
Both databases pass native API→LMTP→SMTP/restart with public→never→public archive
policy and distinct retained bodies. These process positives supplement, not
replace, the DB-level adversarial audit-wait tests. Limited static review found
no new P1/P2; all six reviewed source hashes match the final candidate.
Earlier shared-only `target/delivery-authority-gates-20260910-024940/` passes all 23 gates:
workspace611/0/44, mandatory PostgreSQL27/0/0 and 378 stable source/harness paths,
including native API→LMTP→SMTP/restart regressions on both backends.
See [DELIVERY_AUTHORITY.md](DELIVERY_AUTHORITY.md). No HMAC/ledger/intake is added.

## P2-DSN-INSPECTION — read-only claims, not bounce authority

Opt-in CLI queue inspection reads the retained bounce bytes through a bounded
mail-crate RFC3464 inspector. Only typed recipient/Action/Status claims are
printed, always marked untrusted. There is no queue acknowledgement, score,
notification, member change or new intake route. Returned original-message
headers are never used as recipient authority. SMTP failure scoring remains
separate; outbound-issued correlation must precede any future incoming DSN
mutation. Corrected frozen gates passed 20/20 with 372 stable paths:
workspace598/0/43 and mandatory PostgreSQL26/0/0. Strict raw boundary framing
must agree with every direct MIME part's offsets to prevent partial inspection.
Supported syntax,
budgets and evidence boundaries: [DSN_INSPECTION.md](DSN_INSPECTION.md).

## Unicode-prefix serialization — bounded composed acceptance verified

Non-ASCII prefixes now use a dedicated Subject-field path: collect continuations,
decode using mail-parser, compare the exact decoded prefix, then emit bounded
UTF-8-aligned RFC2047 Base64 words. No other retained field or MIME body is rebuilt;
ASCII/empty/absent prefix behavior remains unchanged. This avoids relying on
SMTPUTF8 for this producer alone, without adding SMTP capability negotiation.
The parent transferred the verified cook delta and new tests from the dirty seed
worktree, then reconciled the shared DB consumer test with the new wire contract
without weakening storage/rollback/ASCII assertions. The corrected frozen run
`target/unicode-prefix-gates-20260909-153157/` passed 18/18 steps, workspace
588/0/43 and mandatory PostgreSQL26/0/0, with 367 unchanged hashes. Native browser
→LMTP→SMTP passed seven cases per backend with independent Python decoding of
received Unicode subjects. The earlier failed, moving snapshot is not acceptance.
See [UNICODE_SUBJECT_PREFIX.md](UNICODE_SUBJECT_PREFIX.md).

## P4-WEB-SUBJECT-PREFIX — owner settings adapter

`SettingsForm.subject_prefix: Option<String>` admits a supplied string, including
empty, without trimming or Unicode normalization. Only `Some` enters the JSON
patch passed to `browser_update_list_settings`; omission never replays the GET
snapshot. Existing live owner/session authority, locked config validation and
attributed atomic audit remain the mutation boundary. The common DB validator
rejects CR/LF, rolling back the complete patch. The UI escapes the stored value
inside a labelled text input; help describes empty/whitespace and future outgoing
composition. No schema, dependency, mail composer or transport behavior changed.

Sequential historical tests: POST failed 422 versus 303, then passed before the
missing-label RED; escaped input then passed (`target/web-prefix-red-post.log`,
`web-prefix-green-post.log`, `web-prefix-red-input.log`, `web-prefix-green-input.log`).
Supplementary controls first ran GREEN: exact Unicode/plus/space/tab and empty,
omitted audit keys, CR/LF and duplicate rejection, wrong CSRF, cross-list denial,
moderator demotion and lost address verification, full list/config-audit snapshot
preservation, forced audit failure with a changed prefix and successful retry.
The PostgreSQL matrix owns its schema/router and is explicitly registered in
`scripts/test-postgres.sh`; no extra logins were appended to shared feature routers.
The old stale-omission `[new]` test is unchanged.

Fresh frozen run `target/web-prefix-gates-20260909-143205/`: **17/17**, workspace
**582/0/43**, PostgreSQL **26/0/0**, **362 source/harness hashes stable**. All
CLAUDE.md gates, strict all-feature Clippy, deny and online audit passed (existing
RUSTSEC-2023-0071 waiver unchanged). Native Chromium saved, reloaded and cleared
an escaped Unicode prefix with zero page/console errors. Separate parent native
browser → LMTP → SMTP tracers passed seven ASCII-prefix cases on each database:
default, changed literal `%d`, sibling isolation, enabled restart, empty, empty
restart and exact spaces. They compare exact Subject/body bytes and delivery
multiplicity; they do not demonstrate RFC2047 or SMTPUTF8 interoperability.
The SMTP sender does not negotiate SMTPUTF8; Unicode configuration roundtrip is
therefore not a Unicode wire-compatibility claim. See [WEB_SUBJECT_PREFIX.md](WEB_SUBJECT_PREFIX.md).
Historical invalid rows, automatic repair, new prefix-specific in-flight
revocation races and whole Mailman parity remain outside this increment.
The worker snapshot is in `target/web-prefix-handoff.md`; the parent's later
closure documentation changes no production or test source.

## Subject-prefix producer/consumer validation

`Lists::update_tx` rejects CR/LF only when `subject_prefix` is supplied, preserving
the existing `cook_headers` defense. The common transactional path protects both
config API flavors, JSON/form and PUT/PATCH, without truncation or silent repair.
Other text fields keep their multiline semantics; legacy invalid stored values
are not scanned or modified by this increment. No schema, dependency, transport
or browser-control change is introduced.

`P1-SUBJECT-PREFIX-VALIDATION`: frozen 17/17 gates passed, workspace579/0/42,
mandatory PostgreSQL25/0/0. Evidence includes producer/consumer RED→GREEN,
whole-patch validation rollback on both engines, SQLite audit-failure rollback
and both-flavor router regressions. Existing browser/SMTP probes were rerun as
regressions, not as a new prefix-specific end-to-end tracer. Details:
[SUBJECT_PREFIX_VALIDATION.md](SUBJECT_PREFIX_VALIDATION.md).

## P3-DUAL-BACKEND-CI — bounded acceptance verified

`listmngr_db::test_support::IsolatedSchema` is the one way a test gets a
PostgreSQL schema: `create(prefix)` reads `TEST_POSTGRES_URL`, refuses a
non-PostgreSQL URL, creates `prefix_<uuid7>` over a one-connection `Any`
pool and returns a URL with `options=-csearch_path%3D<schema>`; `drop()`
removes it with `CASCADE`. It replaced fourteen tests that each demanded a
fresh empty database through their own `WEBUI_*_POSTGRES_URL` /
`ARCHIVE_THREAD_POSTGRES_URL` variable, so `scripts/test-postgres-all.sh`
can run the whole ignored PostgreSQL population (`cargo test … -- --ignored`
minus the browser and `postmap` fixtures) in parallel on one server; CI runs
it after the chosen contract set. The helper is library code because
integration tests of every crate use it; production never calls it.

The e2e harness (`crates/cli/tests/mailpath_e2e.rs`) gained a `child_env`
builder, `Fixture::start_with(dkim, extra)` (an OpenSSL-generated RSA key
written as `[[mta.dkim_signing]]` TOML and passed through `LISTMNGR_CONFIG`,
plus arbitrary `LISTMNGR__*` overrides), and typed-API helpers for the list
configuration, inline templates and bans. DKIM is verified in-process with
`listmngr_mail::authenticity::Verifier` over a seeded `TxtCache` holding the
fixture's `<selector>._domainkey` record, i.e. the same verifier the inbound
`validate-authenticity` rule uses, so the assertion is `dkim=pass` in a real
`Authentication-Results` value rather than the presence of a header. Two
runtime semantics surfaced and are asserted as configured rather than
changed: VERP on personalized copies follows Mailman's
`verp_personalized_deliveries` (default off), and the RFC 8058 one-click pair
is emitted on per-recipient copies only, since the URL is per member.

`ModerationRepo::review_forwarding(id, context, action, reason, forward_to,
now)` is `review` with Mailman's `forward`; `review` delegates with `None` and
the browser review path passes `None`. Inside `review_tx` the address is
canonicalised and validated before any write (`forward_address`: a
`safe_mailbox` under 255 bytes that parses as an `Address`), the decision is
applied as before, and when the address does not point at the list
(`points_to_list`) `workflows::enqueue_moderated_forward` loads the stored
original, renders `notice-forward-subject`/`forward-moderated-body` in the
recipient's language, wraps the original with `notices::serialize_with_message`
from the list's bounces address and enqueues it through the shared
`enqueue_notice`; `moderation_log.forward_to` (migration `0042`) and the
`moderation.<name>` audit diff record the address. The REST handler maps
`forward`/`forward_to` (form booleans via `mailman_bool`) onto it and refuses
`forward` without an address before touching the repository.

`listmngr_api::queues` mounts `/queues`, `/queues/{name}` and
`/queues/{name}/{id}` on both prefixes. `Queue::ALL`, `Queue::name` and
`Queue::from_name` give the wire names; `MailQueueRepo::pending_ids` and
`pending_count` read the `ready`/`leased` rows of one queue in id order (the
`queue_jobs_due` index covers the predicate). Injection reuses the CLI's
intake exactly — `parse_message_id`, `message_id_hash`, the `version: 1`
routing context with `list_id`, `envelope_sender` and `message_id_hash`, five
attempts, `MailQueueRepo::enqueue` — with the envelope sender taken from the
`From` mailbox instead of `--sender`; it is refused for every queue but `in`
because the other queues' contexts are produced by the runners, not by
callers. Reads use the same unbound-token rule as the site-wide bans; the
job route checks that the job belongs to the named queue so a job id cannot
be probed across queues.

`listmngr_api::digest` mounts `/lists/{id}/digest` on both prefixes. `GET`
projects `volume` and `next_digest_number` from the list row. `POST` parses
`send`/`bump`/`periodic` through the same `mailman_bool` form deserializer
the workflow inputs use, then runs `DigestRepo::bump_with_context` (the
existing bump, now attributed to the caller) and `DigestRepo::live().flush`
with `force` for `send` and without for `periodic`. The digest renderer
(`render` and the list-header helper) moved from `listmngr_runners::digests`
to `listmngr_db::digests` — its inputs were already the repository's
`DigestIssue`/`DigestOutput` types and `listmngr_mail`/`listmngr_pipeline`,
both dependencies of `listmngr_db` — so the REST layer needs no runner
dependency; the runner re-exports it. Publication keeps its single
transaction: the issue, the outgoing job and its recipient snapshot commit
together or not at all, and the response's count is what committed.

`mailing_lists.admin_notify_mchanges` (migration `0041`) is read and written
like `admin_immed_notify` (patch map, `PUT` reset, row projection, form
boolean normalisation on the config resource). The two membership hooks that
every insert and delete of a `member` row already pass through —
`workflows::welcome_new_member` after the insert and
`workflows::delete_member_with_goodbye` around the `DELETE … RETURNING` —
now read the flag with the list row and call
`notify_administrators_of_membership_change`, which selects the owners' and
moderators' addresses inside the transaction, skips list-pointing ones, and
enqueues `list:admin:notice:subscribe`/`unsubscribe` per recipient through
`enqueue_templated_notice` (recipient language, `$member` placeholder, the
list's `-owner` as sender, no `Reply-To`). The welcome keeps its own switch
and its ban suppression; the administrator notice depends on neither.

`BanRepo` gained `site_get`, `site_list`, `site_count`, `site_create` and
`site_delete` over the rows whose `list_id IS NULL`; the list-scoped methods
and their callers are unchanged. Because both engines treat `NULL` as
distinct under `UNIQUE(list_id, email_or_regex)`, `site_create` takes the
site-wide `subscription_rate` reservation, selects the value inside the
transaction and returns `Conflict` before inserting; `site_delete` reports
not-found from the affected-row count. Both record `ban.create`/`ban.delete`
against target `site`/`bans` in the same transaction. Matching is untouched:
`is_banned` already read `list_id=$1 OR list_id IS NULL`. `listmngr_api::bans`
mounts `/bans` and `/bans/{email}` next to the list routes; `authorize_site`
authenticates the scope and refuses a token bound to a list or domain, the
way domain creation does.

`listmngr_api::header_matches` mounts Mailman's `/lists/{id}/header-matches`
collection, `/{position}` item and `/find` on both prefixes over
`listmngr_db::header_matches::HeaderMatchRepo`. Every mutation — `append`,
`update(position, HeaderMatchPatch)`, `remove(position)`, `clear` and the
older `replace` — goes through one private `edit`: take the list's writer
reservation (`lock_list_for_patch`), read the current rows inside the
transaction, apply the change to the in-memory set, validate every row and
refuse a repeated header (case-insensitive) and pattern, rewrite the set with
gap-free positions, and record one `list.header_matches` audit row carrying
the change kind, the position it touched and the resulting count. A patch
that fails validation or names a position past the end leaves the stored set
as it was. `HeaderMatchPatch` fields are `Option` for header/pattern and
`FieldEdit::{Keep, Clear, Set}` for chain and tag, so a `PUT` (clear what is
absent) and a `PATCH` (keep what is absent) share one path. The REST layer
lower-cases the header, accepts `action` (Mailman) or `chain` (typed) and
rejects a disagreeing pair, renders the compatibility entry without absent
optional fields and with `http_etag`, and treats a non-digit position segment
as not found the way Mailman's `{position:int}` route does. `find` filters the
list's rows in memory and keeps their real positions.

`listmngr_db::tasks::TaskRepo::sweep(now, retention)` is Mailman's task
runner as a repository call: eight steps, each a loop of bounded batches
(100 rows, at most 100 batches per step per sweep) in their own transaction
with a `task.sweep` audit row naming the step and count, so a large backlog
never holds one lock for long and an interrupted sweep loses only the batch
in flight. The expiry steps are single `DELETE … WHERE key IN (SELECT …
ORDER BY … LIMIT n)` statements (workflows outside `pending_moderation`,
probes, help cooldowns, autoresponse records against the list's grace
period in bigint arithmetic). Job collection selects `done` jobs acknowledged
before the cutoff that no unexpired `dsn_issuances` row references, deletes
their issuances and the jobs (the recipient snapshots, notice provenance,
digest and owner delivery rows cascade); held collection removes disposed
rows and their `moderation_log` after the same retention; message
collection takes messages older than the cutoff that no job, held row or
issuance references, with their delivery bindings, and the blob when it was
the last message on it (the same rule list deletion uses). The stale-bounce
step walks members with a positive score by keyset, parses
`last_bounce_received` in Rust, and resets fenced on that very value, so a
bounce scored in between is a fresh score, audited `bounce.stale_reset`.

`listmngr_runners::tasks::run` sleeps `MailRoleConfig::task_interval`
(`[mailman] run_tasks_every_secs`) between sweeps, stops on shutdown like
the other runners, and logs the summary only when something changed;
`serve_mail_role` spawns it beside the digest and archive runners.
`TaskRepo::notify_list` counts what moderators owe (`PendingSummary`: held,
subscriptions, unsubscriptions in `pending_moderation`), renders
`list:admin:notice:pending` per recipient language with `$count` and a
`$data` block of sections (`notify-held-messages` and friends, fifty entries
each, then `notify-more`) through the shared `enqueue_templated_notice`,
and audits `list.notify` in the same transaction. The CLI exposes both as
`tasks run` and `notify`.

## P3-DIGEST-SETTINGS — bounded acceptance verified

`DigestRepo::flush` reads its policy from the list under the publication
lock (`load_settings`, migration `0040`) instead of the constants it had:
the size trigger is `digest_size_threshold` KiB (zero disables it), the
daily trigger applies only with `digest_send_periodic`, the thousand-post
cap and `force` remain. Before rendering it compares the calendar period of
`digest_last_sent_at` with now under `digest_volume_frequency` (`period`
maps a timestamp to a year, month, quarter, ISO week or day) and, when the
period changed, advances `volume` and resets `next_digest_number` in the same
transaction, audited `digest.bump` exactly as the manual bump is. The three
digest templates are resolved there too — the flush has the transaction and
the list snapshot, the renderer is pure — and travel on `DigestIssue` as
`masthead`, `header`, `footer`, with `$volume` and `$issue` added to the list
placeholders and the list's own language chosen, since an issue is shared.

`listmngr_mail::digest::build` gained the templates and writes the
plain-text issue in RFC 1153's shape (`plaintext_body`: `Today's Topics:`
with authors, the seventy-hyphen rule, `Message: N` header blocks separated
by thirty hyphens, `Subject: Digest Footer`, `End of …` and its underline)
and the MIME issue with the templates as text parts around the
`multipart/digest` (`mime_body`). `to-digest` consults
`ctx.list.digests_enabled` and otherwise pushes no effect, so a list without
digests never collects. The settings ride the usual patch, persist, row and
REST paths, with the form normalizer parsing the float and the two booleans.

## P3-BOUNCE-PROBES — bounded acceptance verified

The threshold branch of `smtp_bounces::score` now forks on
`Database::bounce_probe_lifetime_ms()`, which `serve` sets from
`[mailman] bounce_probes` / `bounce_probe_lifetime_secs` (and the
`[mta] verp_format` the probe's sender follows); a `Database` built without
it keeps the immediate disable, so the existing contracts stand. `send_probe`
draws 20 bytes of OS randomness, keeps their hex as the token (case cannot
survive an MTA and the intake's canonicalization, so base64url cannot be
used), stores its SHA-256 in `bounce_probes` (migration `0039`, cascading
from the member) with the lifetime, resets the score, and queues Mailman's
`list:user:notice:probe` through `enqueue_probe` — a notice that, uniquely,
carries an envelope sender: `workflow_notices.mail_from`, which
`prepare_delivery_full` now reads through `WorkflowRepo::notice_sender`, so
the probe leaves the out runner as `MAIL FROM:<list-bounces+probe=TOKEN@host>`.
The sender is built by `verp::encode` with `PROBE_LOCAL` as the local part
and the token in the domain's place, so every existing MTA route admits it.

At intake, `submission_context` recognizes the decoded `probe@TOKEN` shape
(40 hex characters) and stores `probe_token` instead of `verp_recipient`;
`bounce_processing` then goes to `disable_after_probe`, which deletes the
row by hash and list, refuses an expired one, spends every other probe for
the member, and disables through the shared `disable_member` (score to zero,
warning cycle reset, owner notice, `bounce.disable`). The `bounce_probes`
table is the only new state; the disable path is the one the SMTP-time
failure always used, now a function.

## P3-BOUNCE-DETECTORS — bounded acceptance verified

`listmngr_mail::bounce::detect` is the heuristic layer under the bounce
runner, after the model of `flufl.bounce`: a standards report is read
exactly by `dsn::parse_report` (failed → addresses, only delays →
temporary); otherwise the decoded `text/plain` parts — bounded to 64 KiB —
are handed to the MTA-family detectors in order of specificity (Postfix,
qmail, Exim, Sendmail, Yahoo, Exchange in both its shapes, then a generic
permanent-failure phrase matcher that takes the addresses within a few lines
of the phrase), and finally a delay/warning matcher decides `Temporary`. Each
detector keys on the phrases its MTA writes and the way it lists addresses
(`<address>: reason` lines, Exim's indented list, Sendmail's `-----`
sections), and every address is canonicalized, capped at a hundred and never
a daemon's own. The result is a claim, not authority: `bounce_processing`
still requires a member of the list, which is what keeps a quoted address in
somebody's out-of-office from scoring anyone. `tests/fixtures/bounces/` is
the corpus, one synthetic message per family plus the negatives, and the
test refuses a fixture without an expectation.

## P3-BOUNCE-RUNNER — bounded acceptance verified

`BounceProcessingRepo::process` is the consumer the bounce inbox lacked. It
runs under the lease's fenced clock, locks the list row, and asks
`named_recipients` who the report is about: the VERP recipient the intake
already decoded; else a `dsn::Report` (the parser now keeps the per-message
`Original-Envelope-Id`) whose ENVID is looked up in `dsn_issuances` and
verified with the same `Issuer` that minted it — the issuance's recipient
then replaces whatever the report claims; else the report's failed
`Final-Recipient`s. Each member named is recorded as a `bounce_events` row
(migration `0038` widens `source` to `verp`, `dsn`, `dsn_envid`) and scored
through `smtp_bounces::score`, now shared, so the day rule, the stale-after
reset, the threshold, the disable and the owner notices are one code path
for every bounce origin. A report that names nobody is forwarded as an owner
delivery (sanitized by `owner::cook`, null reverse path) to the
administrators, the site owner or nobody, per
`forward_unrecognized_bounces_to`. The job is acknowledged in the same
transaction, audited as `bounce.process`.

`runners::bounces` claims `Queue::Bounces` under lease renewal, feeds
`listmngr_bounces_total`, and retries with the delivery backoff on error; it
is spawned by `serve_mail_role` next to the other queue processors, with the
site owner's address from `[site]`.

## P3-AUTORESPONDER — bounded acceptance verified

`AutomaticResponses` is a flattened settings struct on `MailingList`, stored
by migration `0037` next to its `autoresponse_records` table (one row per
list, writer and kind, pruned as the grace period is checked, so it never
accumulates). `AutoresponseRepo::respond` reads the action and text for the
kind, refuses to answer a null, malformed or list-owned writer, applies the
grace period, and — in one transaction — records the reply, composes it in
the writer's language (the owner's text through the placeholder expander,
or the built-in `list:user:notice:autoresponse` template), queues it with
`Auto-Submitted: auto-replied` and audits `list.autoresponse`. It always
returns the configured action, so `respond_and_discard` discards even when
the grace period kept the reply back, which is Mailman's behaviour.

The `in` runner calls it first, before owner forwarding, the command bot or
the posting chain, deriving the kind from the stored context; a discard goes
through `ModerationRepo::refuse` under the handler name `replybot`, so it is
audited like every other discard. `Envelope` gained an `auto_submitted`
field for this, validated to RFC 3834's two values.

## P3-EMAIL-COMMANDS — bounded acceptance verified

`EmailCommand` gains `Echo(String)` and `End`. The parser reads `echo`'s
argument from the rest of the line rather than the word iterator (it is the
only verb with free text) through `echo_text`, which bounds it to
`MAX_ECHO_CHARS` and rejects control and bidirectional characters, so a reply
can never be steered by what it quotes; `end`/`stop` take no argument and
stand for Mailman's halt, which in a one-command-per-message runtime means
"this message carried no command".

`help_owned` becomes `bot_reply`, taking a `BotReply` that names the subject
key, template and audit action. Both verbs therefore share one durable
budget — the `email_help_requests` row per list and address per hour, plus
the site-wide `subscription_rate` window — so adding a verb adds no new way
to make the server send mail. `End` commits the lease with no notice, no
workflow row and no audit event, finishing the job rather than retrying it.

## P3-SUBSCRIPTION-POLICY — bounded acceptance verified

`subscription_workflows` gains `state`
(`pending_confirmation` → `pending_moderation` → `closed`), which is the
request's own lifecycle; `consumed` stays the confirmation token's flag. The
expiry sweep skips `pending_moderation`, so a request outlives the 24-hour
token it may never have had. `request_owned` reads the list's policy column
for the action and branches: `open` applies the membership change inside the
same transaction (`apply_membership`, idempotent in both directions, so the
welcome and goodbye notices come from the existing insert/delete paths),
`moderate` parks the row, and the two confirming policies keep issuing a
token. Rows that never issue one still store the hash of fresh OS randomness
that is discarded, so the unique column stays populated by a secret nobody
was given. `confirm_owned` re-reads the policy: under
`confirm_then_moderate` the spent token moves the request to
`pending_moderation` instead of the roster.

`WorkflowRepo::pending` and `decide` are the moderator's side —
accept/reject/discard/defer, each committing the membership change, its
notices and the audit event together, and refusing a request that is not
waiting. The per-address hourly cooldown now applies only where a request
produces something (a mail, a queue row): an `open` list is exempt, because a
member who has just joined must be able to leave again — and, since
`P3-SUBSCRIPTION-E2E`, a request already answered (token spent) no longer
counts, for the same reason on a confirming list.

`TokenOwner` (Mailman's word) is derived from `state`: `pending_confirmation`
is the subscriber's move, `pending_moderation` the moderator's. `pending`
takes a `RequestFilter` on owner and action, `get` fetches one undecided
request, and `decide` accepts either state — a moderator accepting a
subscriber-owned request applies the change and spends the token, as Mailman
does — recording the optional `reason` in the audit event. `api/src/requests.rs`
projects this as `/lists/{id}/requests` (list, `count`, one by token,
decision by form or JSON), mounted through `requests::routes()` under both
prefixes; the request id is the `token` in the URL, and a request is only
addressable through its own list.

`WorkflowRepo::subscribe` is Mailman's registrar for the administrative
path. It reads `subscription_policy`, works out whether the address still
owes a confirmation (`!pre_verified`, or a confirming policy without
`pre_confirmed`) and whether a moderator still owes a decision (a moderating
policy without `pre_approved`), and either subscribes inside the transaction
or stores a request in the state that says whose move it is. Migration `0036`
adds `display_name` (carried onto the member row when the subscription
completes, and shown in the moderator queue) and `pre_approved`, which is
what lets an approval given up front survive the confirmation the subscriber
still owes — an invitation sets it, because inviting *is* the approval, and
reuses Mailman's `list:user:action:invite` template. `members_create` returns
`202` with `{token, token_owner, http_etag}` when the subscription is held,
and keeps the immediate `201` for role records (owner, moderator, nonmember),
which are not subscriptions and carry no flags.

## P2-LMTP-PARAMETERS — bounded acceptance verified

`parse_path` now returns the ESMTP parameters after the `<path>`, and
`check_mail_parameters` validates them against exactly what `LHLO` announces:
`SIZE` (RFC 1870) and `BODY` (RFC 6152). The three failure modes map to
distinct replies — `552 5.3.4` for a declared size over
`max_message_bytes`, `501` for an unusable value, `555 5.5.4` for anything
unannounced — and all of them are decided before the transaction opens, so a
refusal leaves no half-built state and the next `RCPT` is correctly out of
sequence. `RCPT TO` carries no announced parameter at all, so any is a `555`.

On the client side `negotiate` reads `8BITMIME` from the relay's `EHLO` (the
authenticated path reads it in `auth::authenticate` and hands it over in
`Capabilities`, alongside DSN). An 8-bit payload adds `BODY=8BITMIME`; a
relay without the extension resolves every recipient as a transient failure
before `MAIL FROM`, the same shape as the existing "DSN capability required"
refusal. The in-crate SMTP sinks now announce 8BITMIME, as every real relay
does.

## P2-METRICS — bounded acceptance verified

`listmngr_core::metrics` is a hand-rolled Prometheus registry: `LabeledCounter`
(one label, a fixed value set declared up front so every series exists from
the first scrape) and `Histogram` (fixed bounds, cumulative buckets, a
microsecond sum) over atomics, behind one `global()` instance. It has no
dependency and no locking. The runners feed it at the points where an outcome
is final: the LMTP handler after the batch enqueue, `process_one` after each
disposition committed (`accept_post` returns `accepted`/`filtered`), the
runner loop on a processing error, and the out runner per transaction
(duration, completed/failed) and per delivery (recipient outcomes, and the
acceptance-to-relay latency from `messages.created_at`). The API renders the
registry plus queue gauges from `MailQueueRepo::stats`, cached five seconds
in `AppState` because `/metrics` is unauthenticated.

## P2-VALIDATE-AUTHENTICITY — bounded acceptance verified

Authentication needs DNS, so it lives in the `in` runner rather than the
pure pipeline. `MailRoleConfig::authenticity` holds a
`listmngr_mail::authenticity::Verifier` (a `mail-auth`
`MessageAuthenticator` over the system resolver) when
`[mta] authenticity_checks` is on; `process_one` runs it before the chain,
feeds `SenderChecks::dmarc_policy_restrictive` from its verdict, and — on
acceptance — writes the `Authentication-Results` value and the
`dmarc-mitigation` tag into the stored message context through
`AcceptEffects`. Consumers read them back into
`listmngr_mail::handlers::Admission`, so the out runner, the digest copy
and the archive copy cook with the same facts; `cook_for_site` is the
no-admission special case tests and planning use. The `Verifier` accepts a
seeded `TxtCache` implementing `mail-auth`'s `ResolverCache`, which is how
the tests exercise real DKIM verification of an owned key and the RFC 9989
tree walk without a network.

The chain now starts with Mailman's `dmarc-mitigation` link. The rule
reads three list facts and one sender fact, tags `EvalState.tags` with
`dmarc` for the handler, and jumps to the `DmarcMitigation` chain kind only
for the terminal actions; `Outcome.tags` carries the tag to the runner.
Migration `0034` is the first column rebuild: SQLite cannot change a
`CHECK`, so the two DMARC columns are copied aside, dropped, re-added with
the wider vocabulary and restored — one statement sequence valid on both
engines, with the schema snapshot corpus unchanged.

## P2-DELIVERY-POLICY — bounded acceptance verified

`crates/runners/src/delivery_policy.rs` holds the two decisions the out
runner used to hard-code. `Backoff::delay_ms(attempts)` doubles from the
initial delay per attempt already made, caps at the maximum, and jitters by
±20% (never below one second); every transient transition in the out runner
and the `in` processor's retry path take their delay from it through
`MailRoleConfig::backoff`, so the queue's `retry` primitive keeps its
caller-supplied-delay contract and the policy stays testable without the
database. `chunk_by_domain` returns index chunks over the pending roster —
a stable sort by domain, then fixed-size slices — and the transaction loop
writes each outcome back to its recipient's index, which keeps the
per-recipient ENVIDs, personalized copies and VERP senders (all only
meaningful for one-recipient chunks) aligned with the roster the database
reserved. `MtaConfig::validate` now owns the `[mta]` invariants that
`Config::load` enforces, including the new sizing and retry bounds.

## P2-PERSONALIZE-VERP — bounded acceptance verified

`prepare_post` now decides how a subscriber delivery is split. It resolves
the decoration templates once (`decoration_templates`) and, for a
non-personalized list, expands and applies them immediately as before; for
a personalized list it returns the undecorated bytes with a `PerRecipient`
plan carrying the list, the unexpanded templates and the `personalize`
mode. `recipient_copies` then builds each recipient's copy: the member
profile from `DeliveryRepo::recipient`, `personalize::placeholders` for the
`$user_*` values, `decorate`, the `To:` rewrite for `full`, the one-click
pair, a DKIM signature, and — when the VERP policy applies — the envelope
sender from `listmngr_core::verp::encode`. `send_transactions_with_envid`
takes a copy's own `MAIL FROM` over the shared one, so VERP and
`smtp_single_recipient` compose with the existing ENVID fencing.

The `VerpPolicy` mirrors Mailman: `verp_personalized_deliveries` and
`verp_delivery_interval` (on the list's `post_id`, which `after-delivery`
bumps at acceptance). Intake decodes `list-bounces<delim>local=domain`
before the reserved-suffix scan and records `verp_recipient` in the queued
context; the bounce runner (C3) will prefer it over detector output. The
Postfix map generator emits one regexp per list for these addresses so the
MTA needs no `recipient_delimiter` (the snippet leaves it empty on purpose).

## P2-ONE-CLICK-UNSUBSCRIBE — bounded acceptance verified

The out runner now has a per-recipient path. `prepare_delivery_full`
returns `Prepared { cooked, mail_from, personalized }`; when the list is
personalized, `signed_copies` builds one copy per pending recipient
(`personalize_for`: the one-click pair spliced by
`listmngr_mail::personalize::one_click_unsubscribe`), signs each with the
list's DKIM key, and `send_transactions_with_envid` takes those `variants`
and forces one transaction per recipient — the same width-1 loop the
`smtp_single_recipient` setting already used, so ENVID issuance and
per-recipient outcome fencing are unchanged. Everything else still cooks
and signs once.

Tokens are `listmngr_core::one_click::Signer` MACs over
`(list_id, member_id, expiry)` with length-delimited domain separation like
the DSN issuer; the key is a random 32-byte site secret generated into the
`site_secrets` table on first use (`OneClickRepo::signer`), so no
configuration is needed and a deployment with several nodes shares one
key through the database. The URI carries the token in the query string
because the request logger records only the path. `OneClickRepo::redeem`
verifies, checks the membership still belongs to the list, and reuses
`delete_member_with_goodbye` plus an audit event in one transaction. The
HTTP surface (`crates/api/src/unsubscribe.rs`) is outside both the bearer
API and the browser session router: the token is the only credential, the
page is zero-JS under `default-src 'none'`, and the `POST` body must be
exactly RFC 8058's pair.

## P2-COOK-HEADERS — bounded acceptance verified

The header handlers now carry Mailman's full output. `rfc-2369` is a pure
builder in `crates/mail/src/list_headers.rs` fed by the list settings and the
site base URL that `PipelineContext::base_url` carries; `cook_for_site` is
the entry point that passes it, `cook_for` (tests, planning) passes none.
The archive URL shapes — `/archives/list/{list_id}/` and
`/archives/list/{list_id}/message/{hash}/` — are HyperKitty's, and the hash
is the same base32 SHA-1 the archive already keys on, so a message's
`Archived-At` stays valid across a Mailman migration and the SSR archive
must serve exactly these paths. `List-*` headers are emitted before the
`to-archive`/`to-digest` snapshots, so the archive copy, the digest copy and
the subscriber copy agree.

`cook-headers` gained the `Sender` rewrite, the `Reply-To` policy
(`crates/mail/src/reply_to.rs`, which parses the inbound addresses with
`mail-parser`, applies `first_strip_reply_to` and `reply_goes_to_list`,
deduplicates by address and writes one header back through
`cook::format_mailbox`/`cook::phrase`, the `formataddr` equivalents), the
`X-Mailman-Version` marker and the `Message-ID-Hash` pair. `Database` now
carries `site.base_url` next to `default_language`, so the out runner and
both archive renderers (queue processing and read-time rendering) cook with
the same URLs the CLI was configured with.

## P2-HANDLERS-DECORATE — bounded acceptance verified

Decoration happens where Mailman does it: at delivery. `cook_for(Target::Out)`
still ends at `to-outgoing`; the out runner then resolves the two
`list:member:regular:*` templates through `TemplateRepo` (so operator
overrides and the language fallback apply), expands the list placeholders
(`listmngr_mail::templates::list_placeholders`, now shared with the notice
producers) and calls `listmngr_mail::decorate::decorate`, which chooses
between concatenation, splicing and wrapping exactly as
`mailman/handlers/decorate.py` does but over the stored bytes. Generated
text parts use `listmngr_mail::encoding::text_body` — `7bit` or
quoted-printable — the same encoder the HTML conversion of `mime-delete`
now uses. Archive and digest copies never see the decoration because it is
applied after their snapshots.

`tagger` splits into a pure matcher in `listmngr_pipeline::topics` (pattern
compilation shared with the header-match rules, the `scanbody` rule) and a
message reader in `listmngr_mail::topics`; topics live on the list as a
validated JSON column. `after-delivery` and `acknowledge` are effect
handlers like the fan-out ones: they push `Effect::RecordPost` and
`Effect::Acknowledge`, and the `in` runner hands both to
`MailQueueRepo::complete_accepted`, which bumps `post_id`/`last_post_at`
and enqueues `list:user:notice:post` inside the transaction that acknowledges
the inbound job — one commit for fan-out, counter and receipt.

## P2-MIME-DELETE — bounded acceptance verified

`crates/mail/src/mime_delete.rs` is the content filter. It parses the stored
message once and builds a tree whose nodes are the original byte spans
(`mail-parser` part offsets: header block with its blank line, leaf body
without the closing boundary), so pruning a part is dropping a node and
serializing copies survivors verbatim inside regenerated multipart framing.
The steps and the disposal reasons mirror `mailman/handlers/mime_delete.py`
in order: outer type, outer extension, recursive `filter_parts`, alternative
collapse (`reset_payload` for an outer alternative: message headers kept,
content headers taken from the first alternative), HTML to text, and the
`X-Content-Filtered-By` marker. `crates/mail/src/html_text.rs` is the
built-in replacement for Mailman's `lynx -dump`: a small tag scanner that
never allocates more than a few times the input and never runs a process.

The handler is the first entry of `default-posting-pipeline`, so the `in`
runner's planning pass and every consumer's cook see the same filtered bytes.
`HandlerError` now carries a `Refusal` (`Shunt`, `Discard`, `Reject`,
`Forward`, `Preserve`); `plan`/`cook_for` surface a non-shunt refusal as
`listmngr_mail::Error::Refused` and the runner hands it to
`ModerationRepo::refuse`, which acks (or, for a preservable `preserve`,
shunts) the inbound job, records the `post.*` audit event and enqueues the
rejection or forward notice in the same lease-fenced transaction. The
forward notice is the first generated message with an attachment, built by
`notices::serialize_with_message` (`multipart/mixed`, text part plus the
unmodified original as `message/rfc822`, boundary derived from the generated
`Message-ID`). The chain's `reject`/`discard` dispositions reuse the same
path, so both now audit and a chain rejection notifies the author.

## P2-LIST-SETTINGS — bounded acceptance verified

`MailingList` carries Mailman's Alter Messages and Member Policy groups as
two flattened value types (`AlterMessages`, `MemberPolicy`) next to the
existing flattened `DmarcSettings`, which gained `dmarc_addresses`,
`dmarc_moderation_notice` and `dmarc_wrapped_message_text`; the bounce
group gained `forward_unrecognized_bounces_to`. Enumerations are
`string_enum!` types with Mailman's wire names, so the REST resource, the
audit trail and the database column all carry the same string. Migration
`0031` adds the columns with `CHECK` constraints on every enumeration and
Mailman's defaults, and the schema snapshot corpus pins them on both engines.

Writes go through the same `apply_patch_key` path as every other list
setting: each key is validated into the in-memory list, the whole row is
then rewritten and the `list.config` audit event recorded in one
transaction, so a rejected key means nothing persisted. MIME types and
extensions are validated as printable ASCII tokens and stored lowercase
because that is how the content filter compares them; free text is bounded
at 64 KiB. The list configuration resource accepts mailmanclient's form
encoding through `FormAwareObject`, a deserializer that folds repeated form
keys into arrays so `filter_types=a&filter_types=b` and Python's
`True`/`False` round-trip, and `PUT` seeds every omitted setting from
`MailingList::new` before applying the supplied keys.

## P3-I18N — bounded acceptance verified

`crates/i18n` is the message catalog layer: Fluent (`fluent-bundle`) resources
for `en` and `vi` embedded at build time, one concurrent bundle per language
built once, and a small API — `negotiate` maps a BCP 47 tag to a shipped
catalog by exact match and then primary subtag (`vi-VN` → `vi`, `fr` → `en`),
`choose` takes the first preference a catalog serves, and `message` renders an
id with arguments, falling back from the requested language to `en` and
finally to the id itself so a missing translation never fails a notice.
Bidi isolation marks are switched off because the output lands in
`Subject:` headers, not HTML.

The recipient's language is decided once per notice inside the producer's
transaction (`listmngr_db::notices::recipient_language`): the member's own
`preferred_language` (member, then address, then user preference layer, the
same precedence `resolve_member` uses), then the list's `preferred_language`,
then the site default that `Database::with_default_language` carries from
`site.default_language`. The chosen tag drives both the subject
(`listmngr_i18n::message`) and the body: template resolution now tries each
scope in that language and then `en`, and the built-in catalog itself is
served in Vietnamese (`listmngr_mail::templates::builtin_in`) before English.
Owner and moderator notices are rendered per recipient, so two owners with
different preferences receive different languages for the same hold. The
confirmation subject is deliberately identical in every catalog because the
reply-to-confirm parser reads it. Runners and the browser UI resolve member
preferences against the configured site default instead of a hardcoded `en`.

## P2-TEMPLATES — bounded acceptance verified

Generated notices render from Mailman-named templates. The pure engine in
`crates/mail/src/templates.rs` holds the catalog of names, the built-in
English bodies, `$placeholder` expansion with Python `string.Template`
semantics (unknown placeholders are left as written and substituted values
are never re-scanned), and the URI loaders: `mailman:///name` selects the
catalog, `file:///` reads a bounded UTF-8 file, and `https://` is parsed and
stored but not fetched by this runtime. `crates/db/src/templates.rs` resolves a
name for a list by trying the list, then its domain, then the site, each in
the requested language and then `en`, before falling back to the built-in; a
stored template that cannot be loaded is logged by name and scope only and
skipped, because a broken template must never block the subscription or hold
that needs it. The site scope stores an empty `scope_id` rather than NULL so
the unique constraint and upserts behave on both engines.

`crates/db/src/notices.rs` is the one serializer every notice uses: the list
snapshot is read inside the producer's transaction, Mailman placeholders are
filled from it, and the message is emitted with ASCII-validated `From`/`To`/
`Reply-To`/`Message-ID`/`Date`, an RFC 2047 subject when non-ASCII, and a
CRLF-normalized body that travels 7bit when ASCII with short lines and base64
otherwise — so an operator-written template can neither inject headers nor
exceed the transport line limit. The confirmation challenge keeps its
`confirm TOKEN` subject because replying with it intact is the email
confirmation path.

Holding a post now also produces notices inside the hold transaction: the
poster gets `list:user:notice:hold` when `respond_to_post_requests` and the
envelope sender is a safe, non-list mailbox (a null or list-owned reverse path
never produces backscatter), and every owner and moderator gets
`list:admin:action:post` when `admin_immed_notify`. Mailman's `/uris`
resource manages template URIs per list, domain and site; inline bodies with a
language are a listmngr extension on `/lists/{id}/templates/{name}`.

## P2-PIPELINE-HANDLERS — bounded acceptance verified

Accepted posts run a Mailman-named handler pipeline. The engine
(`crates/pipeline/src/handlers.rs`) holds the `Handler` trait, the registry,
the pipeline definitions and the pure fan-out handlers; the handlers that need
MIME and header surgery (`crates/mail/src/handlers.rs`) are registered into one
shipped registry that `listmngr_mail::handlers::builtin_registry()` exposes.

This runtime keeps one immutable stored message and cooks lazily per consumer,
so the same pipeline definition is executed two ways. The `in` runner runs it
with `Target::Plan`: every handler runs and the recorded effects —
`member-recipients`, `to-archive` (skipped when `archive_policy=never`),
`to-digest`, `to-outgoing` — become the recipient plan and child jobs bound in
the transaction that acknowledges the inbound job; a pipeline that enqueues
outgoing mail without resolving recipients is refused and shunted. Each
consumer then runs the pipeline with its own target and stops at its fan-out
handler: the archive and digest copies are the bytes as of `to-archive` and
`to-digest`, the delivery copy as of `to-outgoing`. That is what places the
delivery-only `dmarc` mitigation after the archive and digest copies — exactly
the snapshot Mailman's `to-*` handlers would have taken at those positions —
without storing three copies.

`cleanse` removes list controls, private recipients, moderator fields and the
`Approved:` body line; `cleanse-dkim` removes the original DKIM/ARC signatures;
`cook-headers` applies anonymous-list identity, `Precedence: list` and the
`X-BeenThere` history read from the original bytes; `subject-prefix` and
`rfc-2369` follow; `dmarc` rewrites `From`/`Reply-To` when unconditional
mitigation is enabled and refuses conditional mitigation. Handler failures
carry the handler name and are shunted, never delivered. The list setting
`posting_pipeline` selects a registered, executable pipeline that both resolves
recipients and delivers; `virgin` and the declared owner pipeline are refused
for posts. `/api/v1/system/pipelines` projects the registry as it is.

## P2-CHAIN-RULES — bounded acceptance verified

The `default-posting-chain` follows Mailman 3's built-in chain (the
`dmarc-mitigation` link came with `P2-VALIDATE-AUTHENTICITY`, the
`news-moderation` link with `P6-NNTP-SETTINGS`): `dmarc-mitigation` →
`no-senders` → `approved` → `emergency` → `loop` → `banned-address` →
`member-moderation` → `nonmember-moderation` → deferred `administrivia`,
`implicit-dest`, `max-recipients`, `max-size`, `news-moderation`,
`no-subject`, `suspicious-header` → `any` (jump `moderation`) → detour
`header-match` → `accept`. Rules read the immutable `PostingContext` and share a mutable
`EvalState` (Mailman's `msgdata`): hits, misses, reasons, tags, effects and the
moderation action a member or nonmember rule recorded. Several deferred hits
therefore yield one hold whose reason lists each of them in chain order, an
explicit `accept` bypasses the deferred checks as in Mailman, and the
`moderation` chain holds when nothing recorded an action.

Facts are gathered once by the `in` runner (`crates/runners/src/policy_facts.rs`)
from the list row, member/nonmember rows, bans, the list's `header_matches`
rows, site `[antispam]` config and the raw message: unfolded header fields, a
bounded preview of the first plain text part, and canonical To/Cc mailboxes
come from `listmngr_mail::facts` and `visible_recipients`, so the pipeline
never parses MIME. `Approved:` keys are verified in the runner against the
list's Argon2id `moderator_password` and stripped by `cook_post` — the shared
path for delivery, archive and digest — from both the header spellings and an
unencoded first body line; an encoded body-line key is ignored rather than
risk leaking it.

The `header-match` chain is dynamic: it evaluates the list's rows in position
order with the same bounded, case-insensitive regex compilation the repository
validated them with, jumps to the row's terminal chain (site `jump_chain`
default `hold`), records the row's tag, and fails closed to a hold naming the
row when a stored pattern no longer compiles or names an unknown chain.

Deliberate deviations from Mailman are recorded in `FEATURE_PARITY.md`:
stricter `no-senders`, owners/moderators as explicitly accepted senders, and
the retained `moderation policy` reason text.

## P2-CHAIN-ENGINE — bounded acceptance verified

Inbound moderation runs on a chain/rule engine in `crates/pipeline/src/chain.rs`
using Mailman 3 concept names, so operators reading Mailman documentation
recognize this runtime. A chain is an ordered list of links; each link names a
rule and the action taken when that rule hits: `jump` transfers without
returning, `detour` runs a sub-chain and resumes only if that chain did not
terminate, `defer` records the hit and continues, `run` records a named effect,
and `stop` ends evaluation. Terminal chains (`accept`, `hold`, `reject`,
`discard`) produce the disposition and carry the reason of the rule that jumped
to them; the `moderation` chain maps the resolved member or nonmember action the
way Mailman reads `msgdata['moderation_action']`.

The engine stays pure: rules read only the `PostingContext` the `in` runner
gathered, so no database or network access happens during evaluation. Evaluation
is bounded by a hop limit, and every failure mode — unknown chain, unknown rule,
a chain that ends without a disposition, or an exceeded hop limit — is an error
that `decide_posting` converts into a hold. A mis-wired chain therefore cannot
produce an accept.

The shipped `default-posting-chain` reproduces the previous flat early-return
precedence exactly, and a characterization test holds it to that over the whole
input domain rather than over sampled cases. The rule set is unchanged by this
increment; `header-match` and `dmarc-mitigation` are declared as empty chains and
reported `executable: false` rather than pretending to enforce anything.
`/api/v1/system/chains` now projects the live registry, including each chain's
real link order. `/api/v1/system/pipelines` remains a name catalog because no
handler registry exists yet.

## P4-WEB-EMERGENCY — bounded local acceptance verified

The owner settings adapter accepts optional `emergency: bool` and patches only
supplied values. The existing locked list validator, owner/session checks and
atomic attributed audit remain unchanged. The labelled Yes/No control displays
current state and explains hold-versus-shutdown semantics. The existing posting
policy handles null senders, bans and loops before emergency holds; no policy
ordering, delivery pause, mass release or schema change is introduced.

Two sequential RED→GREENs cover POST admission and rendering. The final frozen
run passed 17/17 gates, workspace575/0/41 and mandatory PostgreSQL24/0/0, plus
browser→LMTP→held/SMTP on both engines. A dedicated PostgreSQL schema/router
avoids coupling independent feature matrices through their login rate bucket.
See [WEB_EMERGENCY.md](WEB_EMERGENCY.md) for evidence and limits.

## P4-WEB-NOTICES — bounded local acceptance verified

The owner list settings form exposes `send_welcome_message` and
`send_goodbye_message` using labelled Yes/No selects. Optional boolean extraction
keeps false distinct from omission; only supplied fields enter the shared locked
configuration patch. The existing validator, live authority checks and attributed
audit transaction remain unchanged. Settings updates never enqueue notices;
the existing successful subscription/removal transaction owns publication.
No schema, delivery defaults, templates or recall behavior changed.

Three sequential behavioral RED→GREENs cover goodbye POST, welcome POST and UI.
Frozen acceptance passed 17/17 gates, workspace574/0/40 and PostgreSQL23/0/0.
Both-backend native-browser configuration followed by actual membership APIs
and SMTP verifies default-off, enabled/disabled, sibling survival and restarts.
See [WEB_NOTICE_CONTROLS.md](WEB_NOTICE_CONTROLS.md) for exact evidence and
separate router-publication, browser, PostgreSQL and SMTP boundaries.

## P4-WEB-POSTING-LIMITS — bounded local acceptance verified

The owner settings form exposes `max_message_size` and `max_num_recipients` as
labelled integer inputs with explicit units, inequalities and disable semantics.
POST extraction uses `Option<u32>`; only supplied values enter the JSON patch.
The existing `browser_update_list_settings` path checks live owner/session
authority, locks the fresh list row, invokes the common validator (0..2147483647)
and commits the configuration and attributed audit together. No new schema,
policy engine, permissions or delivery defaults are introduced.

Two sequential behavioral RED→GREENs cover POST admission and UI rendering.
Omission, zero/max/overflow, invalid/duplicate values, authority, CSRF/Origin
and audit rollback controls pass, with the owned-schema PostgreSQL matrix
registered in `scripts/test-postgres.sh`. The frozen 17-gate run passed workspace
571/0/40 and PostgreSQL 23/0/0; native browser→LMTP→held/SMTP runs cover both
backends and restart. See [WEB_POSTING_LIMITS.md](WEB_POSTING_LIMITS.md) for exact
evidence and remaining boundaries. This is not full administration parity.

## P4-LIST-COPY — bounded local acceptance verified

The account form exposes **Receive list copies when directly addressed** as a
labelled Yes/No select. Strict optional `receive_list_copy=true|false` updates only
the membership override, in the existing authority/policy/update/audit transaction.
Omission preserves stored NULL/false/true and the legacy audit shape; legacy
browser preference wrappers remain available. Shared and unrelated fields are unchanged.

Regular accepted posts and transactional held review now suppress a member with
effective `receive_list_copy=false` only when the original uncooked To/Cc contains
that canonical mailbox. Repeated/folded fields and groups are included; Bcc, body,
substrings and display-name text are not mailbox matches. Malformed headers do not
authorize suppression. The shared conservative parser also serves digest collection
and retains recipient-limit admission's unknown/fail-closed behavior. Original SMTP
spelling, delivery mode/status and independent own-post controls remain intact.
This opt-in duplicate-suppression heuristic is **not authentication or proof of
prior delivery**. The low-level explicit-snapshot `ModerationRepo::accept` primitive
is unchanged; production REST and browser review use the transactional resolver.

Sequential behavioral RED→GREEN evidence covers regular processing, held review,
POST 422→303, missing→present UI, then repeated digest headers. Supplementary
SQLite and isolated PostgreSQL controls cover headers, inheritance, preference
preservation, authorization, strict forms and audit rollback. Exact commands/logs
and owned SHA inventory are in `target/list-copy-handoff.md`. The completed frozen
parent run `target/list-copy-parent-gates-20260909-100711/` passed 63/63 gates:
workspace 568/0/39, mandatory PostgreSQL 22/0/0, actual browser/SMTP and restart
on both engines. All 349 frozen paths remained stable and were independently
matched before the later posting-limits increment. Bounded static review found
no P1/P2. The historical handoff's pending full-gate status is now superseded;
this does not establish authenticated authorship, exhaustive races or full parity.


## P4-WEB-OWN-POSTINGS — bounded local acceptance verified

The existing `/web/members/{id}/preferences` POST accepts a strict optional
`receive_own_postings: Option<bool>` form field; malformed or duplicate values
are rejected by form extraction. The account form renders the effective layered
value with an explicitly labelled true/false select, avoiding checkbox omission.
`Database::browser_preferences` retains its public signature as a wrapper into
`browser_preferences_with_own_postings(..., Option<bool>)`. Both use the same
`browser_write_tx` reservation, live session and verified member ownership checks,
and unchanged mode/status restrictions. The narrow SQL update uses integer-backed
`COALESCE($4,receive_own_postings)` so omission retains the current stored value,
not a pretransaction snapshot. Shared user/address layers and other preference
columns are not replayed. Explicit values add a boolean `receive_own_postings`
to the same atomic `preferences.update` audit; omission retains its two-key shape.
No new schema, dependencies, sender authentication, suspension bypass or delivery
policy is introduced. Worker evidence: `target/web-own-postings-handoff.md`.
Frozen `target/web-own-postings-parent-gates-20260909-090105/` passed 58/58 gates:
workspace561/0/37 and mandatory PostgreSQL20/0/0, including the registered
PostgreSQL router/rollback controls. Actual Chromium→SMTP on both backends proves
false/true persistence, peer and sibling delivery, restart in both states,
omission preservation and exact actor-attributed audits. All 339 source/harness
paths remained stable; independent bounded review found no causal P1/P2.
Receipt: `target/web-own-postings-final-receipt.json`. The documented audit
exception remains. New concurrency/late-expiry guarantees are not claimed;
saving the displayed inherited value intentionally creates a member override.

## Automatic warning/removal scheduler — bounded local acceptance verified

`P3-BOUNCE-SCHEDULER` explicitly opts the real `serve_mail_role` into a new owned
`JoinSet` child before inbound/outbound consume their config/database values.
`MailRoleConfig::from_core` projects default-false enable, default60-second interval
and default100-member batch. Core loading rejects interval outside 1..86400,
batch outside 1..1000 even when disabled, and enable without `mta.enabled`.
Existing supported SMTP transport validation remains mandatory. All actual
runtime constructors use `from_core`; no list flag implicitly starts the child.

The scheduler calls existing real-clock `Database::bounce_maintenance().sweep`
once per completion-based delay. First page is delayed too. Pages never overlap
within this instance; there is no catch-up and no detached page task. Repository
SQL LIMIT/keyset ordering and per-list `process_bounces` gating are reused without
schema changes. Each returned cursor includes failed/non-due scanned members;
empty resets to None for the next delayed cycle. Page errors preserve the cursor
and defer retry. Only fixed generic failure text or bounded summary counts are
logged, never cursor/member identifiers, raw errors, DSNs or message content.

Both sleep and sweep are raced against owned shutdown, including already-true
and closed channels. Dropping the sweep cancels its future; SQLx transaction
drop initiates rollback of an uncommitted transaction, not prior member commits.
An in-flight commit acknowledgement can remain uncertain. Existing supervisor
drains and then aborts/reaps owned children; no whole-page atomicity is claimed.
No leader election or new DB clock is introduced; `sweep_at` stays a deterministic
repository testing facility, not a production scheduler/CLI clock override.

README contains safe TOML/environment defaults and explicit activation policy.
Focused evidence includes real serve→private warning publication, real SQLite
multi-page progress and positive/zero list intervals, plus separately spawned
paused-time cursor/error/no-catch-up/cancellation controls. Frozen parent run
`target/bounce-scheduler-parent-gates-20260909-063657/` passed 50/50 gates:
workspace554/0/35, mandatory PostgreSQL18/0/0, six actual no-CLI scheduler cases
per backend with held notices across graceful restart and exact SMTP readback.
Build/fmt/strict Clippy/client/browser/DKIM/TLS/AUTH/security passed with the
documented advisory exception. Source/harness stable; bounded independent review
no P1/P2. Receipt `target/bounce-scheduler-final-receipt.json`; acceptance prose
checked separately afterward. A held-pool cancellation test is not a direct
held-SQL-transaction/earlier-commit cancellation test; no full replacement claim.
Older manual-only assertions below describe the prior explicit-CLI increment
and are superseded only for this opt-in scheduler.

## Explicit warning/removal transaction — bounded local acceptance verified

`P3-BOUNCE-MAINTENANCE` is an operator-triggered CLI vertical, not a `serve`
scheduler. `listmngr bounce sweep [--limit 1..1000] [--after UUID]` enumerates one
SQL-limited, UUID-keyset page of Member candidates with own `by_bounces` status
and processing enabled. It never materializes the full disabled roster. Every
scanned ID advances the cursor, including failures and non-due candidates; the
next empty page terminates a pass. Restart without a cursor for a new pass.

`Database::bounce_maintenance()` exposes `sweep(limit, after)` using a live UTC
clock and `sweep_at(limit, after, now: DateTime<Utc>)` for deterministic database
tests. There is no public CLI clock override. Each member transaction starts
with a write reservation on that Member before reads, reuses the scorer's
address/preference association locks, locks current list configuration, and
rereads current eligibility after waits. Production UTC is sampled after these
locks and before mutation. Status/config changes that win their lock ordering
are respected, including explicit reenable and non-bounce reasons.

Warning eligibility is count below configured maximum and null/elapsed receipt;
removal is count at/above maximum and elapsed receipt, or configured maximum zero.
The exact positive interval boundary is inclusive. A first null receipt is due
immediately. One sweep takes one action per member; interval zero allows another
warning in the next serialized invocation rather than claiming time-wide
exactly-once behavior. Winning direct-RCPT scoring disable resets
`total_warnings_sent=0,last_warning_sent=NULL` in its existing transaction.

The warning increments the existing count, stores its timestamp, queues private
MIME and records `bounce.warning` with member ID, warning number and configured
count in the same transaction. Its subject is `Membership disabled warning` and
its human restoration contact/Reply-To is the real owner address. Removal uses
existing `DELETE ... RETURNING` authority plus optional goodbye, followed by the
shared safe admin renderer for `Member removed by bounces` and `bounce.remove`.
Only the ordinary Member row is removed: other roles, identities, users, addresses
and other-list associations survive. `bounce.removal_notice` records recipient
count including zero for an empty enabled roster. Admin roles deduplicate by
canonical address while delivery retains original transport spelling.

Migration 0025 adds warning maximum 3 (0..100), whole-day interval 7 (0..36500),
and default-true removal notification; processing stays default-off. Both frozen
semantic corpus consumers compose the additive snapshot without changing old
migrations. Native/compat config, form booleans, `Nd` interval compatibility,
PATCH/PUT semantics and OpenAPI share the canonical persisted settings.

Each member commits or rolls back independently; failure does not starve later
page members. CLI returns a bounded aggregate JSON summary and fails its exit
status on any failed item, without logging raw errors/secrets. Each generated
MIME is bounded at 4096 bytes and reuses workflow provenance/null-sender/no-loop
outgoing handling. Publication counts are not SMTP-delivery guarantees. Unsafe
admin rosters roll back the entire member operation (including a queued goodbye);
existing admin roster enumeration is not an aggregate fanout quota. PostgreSQL
row waits, pool pressure or aggregate teardown may cause a member-level failure;
no universal deadlock-free/automatic retry guarantee is asserted. Published
snapshots are not revoked by later roster changes. No incoming DSN, probes,
recovery tokens, localization or automatic maintenance schedule is introduced.

Frozen `target/bounce-maintenance-parent-gates-20260909-054944/` passed 46/46 gates:
workspace 546/0/35, mandatory PostgreSQL 18/0/0, and seven real client/CLI/LMTP/SMTP/
restart cases on each engine. PostgreSQL executed the owned-schema atomicity,
positive-interval concurrent winner and observed preference-lock reenable test.
The process oracle verified warning rejection before member deletion, followed
by goodbye/admin rejection, with actual attempts and no extra bounce events.
Build/fmt/strict Clippy, browser/client/DKIM/TLS/AUTH/security passed with the
documented advisory exception. Source/harness stable; bounded independent static
review no P1/P2. Live-clock due-crossing and held-list-config race tests remain
unclaimed; placement after locks is source-reviewed, not universal concurrency
proof. Receipt: `target/bounce-maintenance-final-receipt.json`. Acceptance prose
was checked separately after the frozen run. Focused RED/GREEN provenance:
`target/bounce-maintenance-handoff.md`. Earlier sections are
historical evidence and do not imply acceptance of this candidate.

## Increment notice transaction — bounded local acceptance verified

`P3-BOUNCE-INCREMENT-NOTICE`: additive migration 0024 persists default-false
`bounce_notify_owner_on_bounce_increment`; constructor, legacy deserialization,
native/compat config, PATCH preservation, PUT reset and OpenAPI agree.
`process_bounces` stays false. After the existing member/address/preference locks
and effective-status read, fresh ordered UTC-day eligibility (not numeric delta)
authorizes the private increment producer. It receives the post-stale score before
threshold reset. In this direct-RCPT/no-VERP implementation, threshold crossings
also notify, independently of the existing disable flag and unchanged disable body.
Same-day refresh, replay/out-of-order, disabled delivery, config-only changes,
non-RCPT/nonmember/internal failures do not authorize publication.

The shared bounded admin renderer snapshots owners plus moderators once per
notice type, deduplicates canonical addresses, preserves original transport
spelling and excludes other-list/ordinary-member recipients. Subject:
`Member bounce score increased`; body: member/list/pre-reset score, without raw
post, diagnostic or secret; complete MIME ≤4096 bytes. Existing private
`workflow_notices` job provenance authorizes the unchanged null-envelope/no-loop
consumer. Score/receipt/event-processed/outcome, optional disable, both notice
types and all audits remain in the same fenced completion transaction. Distinct
`bounce.increment_notice` audits count recipients, including zero; false emits
neither notice nor notice audit. Unsafe rosters inherit transactional rejection;
later roster edits do not revoke materialized snapshots, and aggregate roster
cardinality/memory is not newly bounded. No locks, preference resolution or lease
checks were weakened. No warning/removal/probe/incoming-DSN claims are added.

Frozen run `target/bounce-increment-parent-gates-20260908-203556/` passed 42/42
gates: workspace 531/0/34, mandatory PostgreSQL 17/0/0 (including new audit sabotage
and retry), nine actual client/LMTP/SMTP/restart cases on each engine, strict
Clippy/build/fmt, browser/DKIM/TLS/AUTH/security with the documented advisory
exception. Parent supplemental assertions distinguish exact pre-reset score 1
from 2, not only job counts. Source/harness fingerprints stayed stable and bounded
independent static review found no P1/P2. Receipt:
`target/bounce-increment-final-receipt.json`. Acceptance text was updated after
the frozen run and checked separately; historical receipts below remain historical.


## Owner disable notice — bounded local acceptance verified

`P3-BOUNCE-DISABLE-NOTICE` adds `bounce_notify_owner_on_disable` with canonical
**true** default (including legacy JSON and migration 0023); `process_bounces`
remains **false**. Native/compat JSON and form config, Python `True`/`False`,
PATCH omission preservation, PUT omission reset, attribute reads and OpenAPI
are covered by focused tests. This does not introduce warning/probe/recovery,
incoming DSN/VERP, removal, templates, authentication or migration/cutover parity.

Only a winning automatic disable publishes fixed, bounded private MIME naming
the member and list. Within the existing fenced completion transaction, one
roster SELECT snapshots current owners plus moderators, deduplicated by canonical
address identity. Each receives a separate persisted job with `workflow_notices`
provenance; ordinary members and other-list admins are excluded. Subsequent roster
changes do not revoke materialized recipients. There is no local `-owner` relay
hop. Empty rosters still disable and record `bounce.disable_notice` with
`recipient_count: 0`, creating no job. False configuration creates no notice.
Blob/message/job/provenance, score reset, disable, event, audits and delivery
outcome commit or roll back together. Replay/config edits do not notify.
Generated notices use null reverse paths and existing no-bounce-recursion
provenance; their SMTP failure neither re-enables nor scores recipients. No
subscriber post, raw diagnostic or secret is included; MIME is capped at 4096
bytes per admin. Unsafe recipient rosters fail transactionally, not by loopback.

Compatibility `/3.1/members` distinguishes owner/moderator role assignment from
subscriber confirmation. The existing list-scoped `members:write` authorization
still precedes mutation; missing flags are accepted only for these two compat
roles. The repository receives the actual `pre_verified` value, never implicit
true. Native creation, ordinary member/nonmember confirmation and invitation
guards are unchanged. Parent router RED→GREEN and the real SQLite client/process
probe cover this integration repair without synthesizing verification state.

Frozen candidate acceptance: 38/38 gates PASS, workspace 524/0/33 and mandatory
PostgreSQL 16/0/0; real client→LMTP→SMTP→restart on both engines, build/fmt/strict
Clippy, browser/client/DKIM/TLS/AUTH and security with the documented advisory
exception. Source/harness stable; bounded read-only review found no concrete
P1/P2, not universal concurrency proof. Run:
`target/bounce-notice-parent-gates-20260908-195604/`; receipt:
`target/bounce-notice-final-receipt.json`. Acceptance documentation changed after
the frozen run, with separate artifact/fmt/diff checks. Whole replacement remains
open; prior no-notice statements and counts refer to earlier increments.


## Threshold-triggered suspension — bounded local acceptance verified

Migration 0022 adds `bounce_score_threshold` as bounded positive DOUBLE PRECISION
(default 5). Native and compatibility config expose numeric values, including
fractions, with PATCH preservation and PUT reset. `process_bounces` stays off by
default. A fresh eligible UTC-day observation compares its post-stale score with
the threshold; numerical change alone is not eligibility (stale 1→1 still counts).
At threshold, only the Member preference becomes `by_bounces` and score resets
to zero, within the existing fenced event/outcome/audit transaction. New regular
recipient selection excludes disabled members; existing snapshots are not revoked.

The scorer locks member, address and sorted preference rows before rereading
effective status; address preference creation shares the address lock. Member
PATCH updates only requested preference fields, never a stale full snapshot.
Two PostgreSQL barrier tests cover member/address/user and first-time address
preference changes plus metadata/mode PATCH preservation. They do not prove all
browser, identity/deletion interleavings or universal deadlock freedom.
Final frozen gates passed: workspace 518/0/32, PostgreSQL 15/0/0 and all 34 checks.
HTTP/client→LMTP→SMTP→restart probes passed on both backends with independent
healthy-recipient and exact DATA/recipient checks. Warnings, notifications,
probes, removal and incoming DSN/VERP processing remain separate.

## Bounded direct SMTP bounce scorer

Migration 0021 persists default-off `process_bounces` and a 1–3650-day
`bounce_info_stale_after` (default seven). Existing member score/timestamp columns
are projected read-only by native and compatibility member APIs. Compatibility
config maps integer storage to `Nd` duration strings; native config remains typed
integer days. This does not implement arbitrary Mailman duration expressions.

Only a newly inserted job/recipient SMTP failure event with RCPT stage, permanent
5xx and a current Member can score when the list flag is on. The scorer acquires
a member write lock before reading its score and receipt timestamp. Event, score,
processed marker and audit remain inside the existing lease-fenced recipient
transaction. Duplicate events do not score again. The first eligible event per
UTC day adds one; at or beyond the stale duration since the most recent receipt,
a new eligible event resets to one. Later same-day receipts refresh the timestamp
without increasing score, and out-of-order timestamps are ignored. The accepted
scoring-only slice did not change preferences; the suspension candidate above
extends that boundary. Existing unprocessed historical events are not replayed.

Static locking/transaction inspection is not a production contention proof.
Direct relay RCPT errors are not authenticated external DSNs; broader bounce
classification, VERP, disable/warning/probe policy and cutover remain open.
Bounded local acceptance is recorded in `P3-DIRECT-BOUNCE-SCORE`: workspace
512/0/30, PostgreSQL 13/0/0, plus actual SMTP scoring/readback across process
restart on SQLite and owned PostgreSQL. This does not prove PostgreSQL contention
or expand the explicitly excluded policy and DSN scope.

## Transactional removal notices

`send_goodbye_message` is a portable default-off list boolean (migration 0020).
The private DB deletion helper uses `DELETE ... RETURNING list_id,address_id,role`:
only an actual Member deletion can notify. It resolves the surviving address's
original transport spelling and the list flag inside the same transaction, renders
bounded fixed MIME, and publishes through job-bound `workflow_notices` provenance.
Direct deletion and shared bulk/sync/confirmed/browser deletion paths reuse this
helper; list teardown publishes while the list still exists. Preferences, removal,
notice rows and audit roll back together on failure. Durable notice context and
recipient snapshots do not depend on a surviving membership/list at SMTP time.
Completion receipts remain distinct from optional goodbye notices.

The email-address DELETE alias shared by native and compatibility APIs requires
`members:write`, list authorization and explicit `pre_confirmed=true` plus
`pre_approved=true`. It selects only a matching Member, not another role at the
same address, and forwards the immutable audit context to repository deletion.
Missing/false approval flags and unknown fields are rejected, not discarded.
This bounded alias does not implement Mailman's pending-unsubscribe policy matrix.

`P3-GOODBYE` distinguishes router/pool-reopen SMTP tests from the independent real
HTTP/mailmanclient/process-restart tracer and live PostgreSQL gate. Neither is a
deployed MTA/cutover test, and atomic publication is not exactly-once SMTP delivery.

## Optional verified-TLS SMTP AUTH PLAIN

`SmtpAuthSecret` redacts both Debug and serialization. `MtaConfig::smtp_auth_credentials`
validates both-or-neither nonempty <=255-byte/control-free credentials and REQUIRED
TLS, or reads a bounded private `smtp_auth_password_file` (one final LF/CRLF allowed).
Unix opens use O_NOFOLLOW/O_NONBLOCK, check the opened descriptor is regular and
reject group/other permissions. Inline+file ambiguity fails closed. `Config::load`
redacts extraction errors because parser diagnostics may echo input values.
`TransportSecurity::from_mta` repeats validation for direct callers and owns the
encoded secret privately; `authentication()` exposes only Disabled/Plain policy.
No configurable insecure certificate verifier exists.

After verified TLS, `smtp/auth.rs` performs fresh EHLO and exact case-insensitive
AUTH/PLAIN extension-token admission, excluding greeting and all pre-TLS state.
One bounded negotiation permits an initial response and at most one **empty** 334
challenge; exact 235 is mandatory. Authentication returns a local redacted I/O
error before the shared transaction, so 535/454 do not become mailbox failures.
RFC4954's SMTP command limit is enforced: oversized initial responses use bare
AUTH PLAIN then empty334. The state machine rejects235 unless credentials have
actually been sent; the continuation response is not the initial AUTH command.
The same `run_transaction` handles no-auth and authenticated envelope/DATA paths;
its optional EHLO config denotes whether authenticated EHLO already completed.
No duplicate EHLO is emitted after AUTH. Lease fencing, signing and final DATA
publication are unchanged. All authenticated outcome free text is replaced before
return, retaining typed stage/code and ambiguous-vs-known final-DATA disposition.

Focused same-stack Rustls loopback fixtures use OpenSSL-generated identities,
owned target TempDirs and memory SQLite. P2-SMTP-AUTH distinguishes observed REDs
from supplemental controls. Parent independent OpenSSL process and CLI file
matrices plus full candidate gates now PASS as recorded in FEATURE_PARITY.
These are not deployed third-party relay certification. No zeroization/core-dump protection, password
rotation without restart, LOGIN/XOAUTH2 or implicit/opportunistic TLS is claimed.

## Local outgoing failure classification

The message fetch and signing-authority job/message/list lookups distinguish
database dependency failure from invalid or missing context. A shared local
failure transition retries dependencies with the existing lease-fenced backoff,
while invalid context/signing is shunted. No transport reservation, SMTP connect
or mailbox bounce is authorized on these paths. Failure to persist the transition
is logged; normal lease recovery remains necessary if the database stays down.
The regression separates an actual closed-pool signing lookup after successful
preparation from deterministic retry/shunt disposition tests on an owned SQLite
fixture; it is not a full production outage/recovery simulation.

## REQUIRED SMTP STARTTLS transport

Parent bounded local acceptance now includes the independent real-process
OpenSSL matrix and the combined workspace/PostgreSQL/client/browser/security
gates recorded under `P2-STARTTLS` in `FEATURE_PARITY.md`. The retained original
failed receipt and separate PostgreSQL revalidation distinguish infrastructure
preflight repair from product behavior. This does not establish live MTA cutover
or a complete Mailman replacement.

`MailRoleConfig::from_core` constructs a private `TransportSecurity` policy from
`mta.smtp_tls`, optional `smtp_tls_server_name` and `smtp_tls_ca_file`. Unsupported
enabled modes fail closed; a disabled default role has a non-sending policy.
Required mode validates the explicit DNS/IP server name (or derives the numeric
relay IP), loads bundled public roots plus optional additional PEM CA certificates,
and builds an immutable Rustls client config with explicit ring provider. The
existing relay `SocketAddr` contract is unchanged. No configurable/dangerous
certificate verifier or global provider installation is exposed.

The runner still prepares/signs the final bytes and reserves the durable attempt,
then calls `mail::smtp::send_secure`. A single upgrade deadline bounds initial
220 greeting, EHLO writes/response, exact case-insensitive STARTTLS extension
**token** (excluding EHLO's greeting line), STARTTLS/220 and verified TLS 1.2/1.3.
It rejects unsolicited buffered plaintext at the upgrade boundary. Only after
TLS succeeds does the same `run_transaction` used by plaintext execute, beginning
with fresh EHLO. MAIL/RCPT/DATA, partial recipient preservation, dot-stuffing,
DATA ambiguity and immediate final DATA publication are not forked. Transport
errors before this transaction become safe transient outcomes, not remote SMTP
5xx metadata or bounce events. Post-TLS SMTP failures retain their ordinary typed
stage semantics. The subsequent AUTH increment above adds PLAIN only, never downgrade.

Locked existing versions: tokio-rustls 0.26.5, Rustls 0.23.43, pki-types 1.15.1,
webpki-roots 1.0.9. The direct tokio-rustls dependency disables defaults and selects
ring/tls12; safe default protocol versions are TLS 1.2 and 1.3. Trust is loaded at
role construction, not per delivery; no OS trust/keychain or live DNS dependency
is used for this transport. CA rotation needs restart and bundled-root updates
need rebuild. This does not add revocation/OCSP policy, mutual TLS, implicit TLS,
LMTP encryption or production MTA acceptance.

Real runner/owned SQLite/TCP→STARTTLS→Rustls tests assert exact envelope and cooked
bytes, post-TLS EHLO and pending/no-bounce negative controls. These are same-stack
Rustls peers with OpenSSL-generated owned certificates, not independent live-MTA
interoperability. See P2-STARTTLS. Earlier whole-candidate gates below are baseline
evidence only until parent re-verifies this source change.


## Visible recipient moderation

Migration `0019_recipient_limit.sql` adds default-zero `max_num_recipients`,
bounded to the signed portable SQL INTEGER range. The core record, SQL
projection/update, strict JSON/form config and OpenAPI share the contract;
business changes and audit insertion remain one transaction. PATCH omission
preserves the value; PUT omission resets it.

Inbound policy facts use parsed mailboxes from every top-level To/Cc occurrence,
including groups, not comma splitting or delivery roster length. Conservative
header completeness and delimiter checks prevent forgiving parser recovery from
silently undercounting malformed input. A nonzero limit holds on parse failure
or `count >= limit`, matching Mailman's documented equality boundary. The
existing policy/transaction path preserves original raw bytes and commits a held
message without outgoing/archive/digest children. Zero bypasses only this check.
This does not implement every Mailman acceptance rule or exhaustive RFC address
syntax. See `P2-RECIPIENT-LIMIT` for exact evidence.

## Outbound DKIM — bounded local acceptance verified

Fresh frozen-candidate verification passed workspace (480/0/30), canonical
PostgreSQL (13/0/0), a real PostgreSQL HTTP→LMTP recipient-policy tracer,
mailmanclient 3.3.5 and Chromium browser/DB-effect acceptance. Seven real SMTP
fixture profiles passed independent dkimpy verification and eight negative
controls each. No source changed during gates; subsequent edits are evidence
documentation only. Local acceptance does not establish production deployment
or the remaining Mailman replacement obligations.

The signer explicitly selects relaxed header/simple body canonicalization.
Independent RFC body hashes reproduced a mail-auth 0.12.1 relaxed-body defect
for trailing whitespace-only lines. Simple mode avoids that dependency path
without rewriting MIME content or weakening verification; it is less tolerant
of subsequent body whitespace changes. Regression expectations come from dkimpy
1.1.8 rather than the signer's own canonicalizer.

`mta.dkim_signing` holds operator-controlled domain, selector and key-file paths.
`MailRoleConfig::from_core` loads validated identities and RSA signers once;
configuration serialization itself does not read key bytes. Runtime Debug and
signing errors redact key material. Unix opens use `O_NONBLOCK` before checking
descriptor metadata, preventing FIFO startup hangs without a check/open race.
Only regular files are admitted; group/other permissions are rejected on Unix,
and reads are capped at 64 KiB. No deployment keys are used by tests.

The final outgoing signing hook reloads stored job/message/list authority and
selects the key by the stored list's mail host. It runs after ordinary cooking or
the existing producer-owned private/owner/digest preparation and before SMTP.
The library owns DKIM canonicalization and RSA-SHA256; local preparation only
normalizes SMTP line endings, rejects bare CR and ambiguous/missing From, and
requests From oversigning. The signed copy is not written back into the spool.
Local failure shunts without opening SMTP or recording a mailbox failure.
The hook does not change envelope recipients, null reverse paths, lease fencing
or SMTP uncertainty semantics.

The parent reproduced FIFO blocking with a subprocess-bounded integration test,
then repaired it with a nonblocking open. Test key permissions are explicitly set
to 0600 rather than depending on the installed OpenSSL's creation mode. Mail
and runners all-target tests pass (153/0/1), as does strict affected-package
Clippy. Read-only source review, workspace build/Clippy, fresh security checks and
PostgreSQL 14.24 canonical tests plus a serial CLI tracer also passed. The first
full run timed out in workspace tests and failed capture freshness; it is not
accepted. A resource-bounded full retry remains pending. Independent dkimpy
1.1.8 verification of seven synthetic TCP SMTP
captures now passes (ordinary, owner, private rejection, digest regular/plain/
MIME/summary). Each includes seven rejection controls. Capture exports have
separate names; final gates require new message/key bytes for every profile,
freeze copies, then verify those copies. This covers these concrete producers,
not every possible notice subtype or a production relay deployment.
No incoming authentication, ARC, DNS publication or live MTA cutover is claimed.

## Bounded transactional welcome producer

Migration `0018_welcome_notice.sql` adds portable integer-backed boolean
`mailing_lists.send_welcome_message`, NOT NULL, default 0, CHECK 0/1. Both semantic
schema compositors include an additive welcome snapshot; historical corpora are
unchanged. Core, REST GET/JSON/form write, PUT reset and OpenAPI use the same
default-off contract. Authorization stays in existing list-config handlers.

The two actual membership INSERT sites (`subscribe_with_context` and
`insert_mass_members`, also used by confirmed workflows) invoke
`welcome_new_member` with only the new member ID before transaction commit.
The helper reads Member-role list identity, stored address.original_email and
the current flag from DB, suppresses notices to banned mailboxes and renders a
fixed English text notice. This optional producer does not redefine administrative
admission; public/email workflow ban enforcement remains at its existing boundary.
It does not accept raw MIME, recipient input, display names or template content.
Transport mailboxes are checked; complete output is capped at 4096 bytes.
Configuration is sampled within the membership transaction; this is not a claim
of serialization with every concurrent policy/ban change on PostgreSQL.

Raw blob, message, outgoing job, exact private recipient and `workflow_notices`
job-bound provenance share the existing membership/audit transaction. Rollback
of audit, provenance or recipient publication removes all attempted effects and
permits retry. Unique membership constraints/no-op filtering and single-use
workflow consumption prevent duplicate welcomes for the same insertion. A
later genuine unsubscribe/re-subscribe may receive a new welcome.
The workflow's existing completion receipt is separate and unconditional;
pending challenges and receipts must be counted separately from welcomes.
Leased email completion retains its final expiry/ACK fencing. No runner cooking,
provenance bypass, delivery uncertainty or recipient-authority rule is widened.

Worker evidence: focused default/role/list/no-op/bulk/confirmed-join and sabotage
tests, plus real authenticated API enablement/subscription → pool close/reopen →
existing TCP SMTP sink, asserting null reverse path and only the stored mailbox.
This is production-router HTTP handling plus real SMTP, not a live MTA or
production deployment. Parent final acceptance passed all nine gates: workspace
465/0/30 ignored (81 summaries), build/fmt/strict Clippy/artifact/diff, fresh
official HTTPS audit and unchanged-policy deny, plus PostgreSQL 14.24 canonical
13 tests and serial CLI welcome tracer. All 28 required tests passed; source
fingerprints remained unchanged. Owned PostgreSQL cleanup was independently
checked. This does not certify PostgreSQL contention or live MTA/cutover.
See `P3-WELCOME` and `target/welcome-evidence/final.json`. Custom templates,
per-request overrides, localization, goodbye/admin notices and scoring are out
of scope. Durable exactly-one publication is not exactly-once external delivery.

## Untrusted incoming bounce inbox

Bare `-bounces` recipients resolve to `Queue::Bounces`, not the ordinary inbound
processor. The shared suffix table also produces Postfix map entries. Exact
posting-list names retain precedence; plus/VERP recipients remain unsupported.
Null reverse paths and automatic messages are appropriate for reports and bypass
only email-command reply eligibility, not bounded header validation or durable
enqueue. The existing `enqueue_batch` transaction commits raw blobs, per-list
message contexts, queue jobs and audit together before LMTP DATA success.

The optional Message-ID parser shares the strict bounded parser; only an absent
ID is permitted on bounce routes. A generated UUID-based identity is internal
metadata, not a modification of the report, sender authentication or outgoing
delivery correlation. Invalid/duplicate IDs are still rejected. Existing posts
and commands still require IDs. Inbox messages are untrusted: they neither
populate `bounce_events` nor mutate subscriber delivery state. Existing CLI
queue listing/metadata/raw export provides operator inspection after reopening
the database. `queue acknowledge-bounce JOB_ID --reason STRING` delegates to
`Database::acknowledge_bounce`: its first transactional statement is a conditional
UPDATE guarded by stored ID, queue=bounces and state=ready. The row/writer lock
serializes with claim (SQLite BEGIN IMMEDIATE / PostgreSQL row selection lock).
It changes only state to done; queue, attempts, scheduling timestamps, message,
context and raw remain unchanged. One `queue.acknowledge_bounce` audit records the
job target, ready→done and trimmed reason (nonempty, no controls, ≤2048 UTF-8 bytes).
No raw/context/token is copied into the audit. Audit failure rolls back; retry is
valid. Missing/wrong-queue/leased/shunted/done jobs conflict, including repeated
acknowledgements, without new audit or timestamp writes. CLI is trusted local
administration, not an HTTP authorization surface. `queue ls --state` validates
ready/leased/done/shunted and applies SQL filtering before the unchanged 1000-row
limit; no state option preserves retained-job listing. Normal show/raw still work.
No bounce worker, trusted DSN/VERP correlation, automatic forwarding,
quota or retention cleanup is added. Unacknowledged reports remain ready;
protected spool storage and monitored growth remain operator obligations.
`P3-BOUNCE-INBOX` records the disposable LMTP/SQLite/CLI acceptance scope.
Prior intake-only parent evidence is `target/bounce-inbox-parent-gates-1/`: workspace
452/0/30 ignored, all build/static/security gates and explicit real Postfix map
lookup pass; Rust/schema fingerprints remain unchanged. Independent read-only
review found no concrete regression within this bounded scope. PostgreSQL,
actual daemon delivery and full bounce processing remain unverified.
Acknowledgement evidence in `target/bounce-ack-evidence/`: focused DB tests 3/0,
real CLI tests 3/0, affected strict Clippy and fmt PASS. Audit sabotage/retry,
state/lease/reason rejection, retained bytes and filter-before-limit are exercised
on disposable SQLite. Parent strengthened the claim race to a file-backed database
with two prewarmed pool connections and a barrier, asserting exactly one winner
and the corresponding acknowledgement audit count; this is not PostgreSQL
lock-contention certification. Final workspace: 457 passed / 0 failed / 30 ignored
(79 summaries); all 20 required regressions, build/fmt/strict Clippy/artifact/diff
and fresh official HTTPS audit pass. Default deny fetch failed over SSH; online
retry with a fresh advisory directory and per-process Git-config isolation passed
the unchanged policy. The canonical `scripts/test-postgres.sh` gate passed 13 tests
on disposable PostgreSQL 14.24. A separate CLI/PG synthetic queue fixture passed
ready/default/done listing, acknowledgement, wrong-queue/replay rejection, exact
raw retention and one audit. Its first two attempts failed on tracer-only CLI
argument/exit-code assumptions; both failures are retained, not behavioral RED.
All three owned clusters were stopped/removed, and their ports verified closed.
Rust/schema fingerprints stayed frozen; see `target/bounce-ack-evidence/final.json`.
Other PostgreSQL versions, acknowledgement/claim contention on PostgreSQL,
actual daemon delivery and full bounce processing remain outside this evidence.

## Durable direct SMTP failure observations

The bounded typed slice uses neutral `SmtpFailureStage`/`SmtpFailure` core
values. SMTP `RecipientStatus::RemotePermanentFailure` carries the exact 5xx
reply code and EHLO/MAIL FROM/RCPT/DATA-start/DATA-final command stage alongside
internal diagnostics. The outgoing consumer passes metadata keyed by original
recipient to `MailQueueRepo::finish_delivery_with_smtp`; the old
`finish_delivery` entrypoint delegates with no metadata for existing fixtures
and callers. No new JSON input is accepted or interpreted. Non-5xx retry,
non-220 greeting, local validation and ambiguous-delivery classifications are
unchanged, and resolving DATA only fills still-pending recipient outcomes.

Additive migration `0017_smtp_failure_metadata.sql` adds nullable `smtp_stage`
(with a closed stage vocabulary) and `smtp_code` (500–599). It performs no
backfill: old events and legacy/local failures remain unknown, never inferred
from free text. New metadata is inserted with the existing event/audit/lease
transaction and uniqueness fence, not a second post-commit update. The existing
read DTO and authenticated API expose nullable stage/code but not diagnostics;
RCPT 5xx is still not a mailbox validity verdict. Both semantic schema composers
include the additive snapshot. Disposable SQLite/TCP focused evidence is under
`target/smtp-stage-evidence/`; final parent evidence is under
`target/smtp-stage-parent-gates-1/`: workspace 447 passed / 0 failed / 30 ignored,
build, fmt, strict workspace Clippy, deny, fresh HTTPS RustSec audit, artifact
and diff checks PASS. Rust/schema fingerprints stayed unchanged during gates.
No live PostgreSQL/MTA certification is claimed.

`MailQueueRepo::finish_delivery` records `RecipientOutcome::Failed` before
resolving a reserved attempt. The event producer joins the stored leased `out`
job, message and recipient, requires the current attempt token and excludes
job-bound `workflow_notices`, `owner_deliveries` and `digest_deliveries`.
Caller-editable lease message/queue metadata and JSON notice flags do not grant
event authority. A valid stored list context is required. Pending-but-unreserved,
already-terminal, bogus, transient, sent and ambiguous outcomes create no event.

Migration `0016_smtp_bounces.sql` gives `bounce_events` a unique job/recipient key
and list-owned cascading deletion. Message/job IDs are retained metadata without
spool FKs, so spool retention does not erase history. Event insertion and
`bounce.record` audit share the recipient/queue transaction. The completion path
reads the current deadline under the queue lock and resamples time after the
transition audit, rolling back all effects if the lease has expired. PostgreSQL
lock contention and runtime portability of the new migration are not yet certified.

`BounceRepo` exposes bounded, ordered metadata reads. REST authorizes `lists:read`
and resource binding before lookup and uses the existing flavor-aware pagination
envelopes. Raw diagnostics, message contents and stored transport context are not
projected. Records remain `processed=false`; there is no scoring or event TTL
worker. A permanent SMTP result can reflect relay/sender/content policy rather
than an invalid mailbox, so it must not implicitly disable membership. Digest
classification, authenticated incoming DSNs/VERP, warning/probe/removal processing
and owner notifications remain separate unfinished product paths.

## Atomic confirmation completion receipts

`WorkflowRepo::confirm_owned` publishes a generated completion receipt after token
admission and join/leave mutation, in the same transaction as token consumption
and `subscription.confirm` audit. The email path also includes its final fenced
ACK. Unleased public REST and browser form confirmations now publish the same
receipt; their response formats and token admission rules are unchanged. Each
successfully consumed token permits one receipt,
including idempotent join-already-member/leave-already-absent outcomes. Replays,
expired tokens, ban denial and stale leases cannot publish a success receipt.

Receipt addressing comes from `subscription_workflows.original_email`, not the
confirming envelope or MIME headers. The transport-safe mailbox guard is applied
before rendering. A fresh message ID and fixed action-specific English text
avoid copying secrets or request bytes. Text explicitly describes a historical
confirmation-time result, not a live membership query at SMTP delivery time.
The existing `enqueue_notice` transaction stores immutable bytes, a single
recipient and job-bound `workflow_notices` provenance. Existing outbound retry,
uncertainty and null-sender semantics apply; no SMTP exactly-once claim is added.
Anonymity and DMARC post transformations cannot recook these private receipts.

SQLite trigger controls cover failure at receipt provenance, confirmation audit
and ACK audit with rollback of membership/preferences/addresses, token state,
spool, recipients, provenance and audit. Existing inbound-handler/SMTP fixtures
follow the delivered challenge's actual Reply-To and subject for both actions,
using a different confirming sender. This is not live LMTP/MTA or PostgreSQL
acceptance, nor configurable welcome/goodbye template support.

The HTTP increment additionally exercises the actual public router for private
join/leave receipts and replay, unleased join and actual-member leave rollback
at receipt-provenance and confirmation-audit failures, and the existing
repository-to-SMTP fixture for both completion notices. The reopened file-backed
SQLite concurrent-consumer test requires exactly one receipt as well as one
membership/audit effect. HTTP publication and repository-to-SMTP consumption are
separate evidence boundaries, not a new end-to-end HTTP-to-SMTP service harness.
No schema/configuration fields or direct administrative member-write behavior
changed. Template settings, localization, bounce processing and full PLAN P3
remain open.

## Bounded unconditional DMARC munge_from

PLAN §4.1 / §4.10 are the normative broader contract. This increment exposes only
`DmarcMitigateAction::{NoMitigation,MungeFrom}` and a boolean unconditional flag.
Defaults remain `no_mitigation,false`; `no_mitigation,true` is an inactive valid
pair; only `munge_from,true` activates rewriting. Conditional `munge_from,false`
and unsupported actions are rejected rather than silently skipping DNS policy.
The list row is locked before merging PATCH data and validating the resulting
pair. Settings and `list.config` audit commit together; audit failure rolls both
back. Additive migration `0015_dmarc_munge.sql` supplies portable SQLite/PostgreSQL
columns, defaults and SQL pair/action constraints; the shared semantic schema
snapshot includes the new columns. No live PostgreSQL gate was run for this slice.
REST JSON/form PATCH/PUT and attribute routes share this writer on both prefixes;
PUT defaults include both settings and OpenAPI exposes the bounded enum.

`mail::cook_individual_post` applies unconditional rewriting via `mail::munge`
only for non-anonymous individual deliveries, then shared `mail::cook_post`
privacy. It validates bounded header syntax and exactly one supported author
mailbox before returning any cooked bytes. MIME builder quotes/encodes decoded
name plus original address and list attribution into list-domain From. Sender
and all previous From/Reply-To fields (including continuations) are removed;
valid single/multiple Reply-To targets are re-rendered, otherwise author fallback.
Malformed/ambiguous/control-bearing authors fail closed; obsolete mailbox forms
and SMTPUTF8 mailboxes are intentionally unsupported. Already list-addressed
From is safely re-rendered without stacking attribution on repeated publication.
The original MIME body is spliced byte-for-byte, never parsed/rebuilt for cooking.
This is identity presentation, **not author authentication** or a trusted marker.

Outgoing preparation maps cooking failure to the existing shunt path before
connecting to SMTP. The runner selects the individual renderer only after
database-owned owner/notice/digest provenance checks. A caller-controlled context
flag cannot authorize an exemption. Digest collection and archive publication
retain shared privacy cooking without DMARC rewriting; MIME/summary articles keep
original From and plaintext formatting remains unchanged. This matches Mailman's
[individual-delivery boundary](https://docs.mailman3.org/projects/mailman/en/latest/src/mailman/handlers/docs/dmarc-mitigations.html).
Anonymity still suppresses author identity across publication paths and selects
current privacy settings with authorized archive bytes. Original authors cannot
be restored from already anonymized stored bytes.
Owner deliveries and database-proven workflow notices keep their separate
preparation paths. No DNS lookup, PSL, DMARC verification, DKIM/ARC signing,
transport TLS, real MTA activation or remote deliverability claim is added.

## Individual ban resources

`GET /api/v1/lists/{id}/bans/{email}` and its `/3.1` counterpart resolve the
percent-encoded ban resource emitted by creation and collection responses.
Authorization precedes lookup and requires target-list `lists:read`. `BanRepo::get`
uses bounded validation/canonical mailbox normalization and an exact list/value
SQL predicate; regex text is an identifier, not a query to evaluate. Global rows
are not exposed. Reads perform no policy mutation or admission decision.


## Experimental subscription ban admission

Public/browser request/confirm and inbound email join commands converge on
`WorkflowRepo`. Join checks list-local and existing global bans before issuing a
challenge and again inside the token-consumption transaction. The latter matches
against stored `original_email`, never caller-supplied identity. Exact identities
and case-sensitive regex input share the same matcher as posting policy facts.

Suppressed requests preserve generic success, with no new workflow, outgoing
notice or cooldown; normal bounded expiry cleanup and inbound lease ACK still
apply. A newly banned token yields the ordinary invalid-confirmation error and
rolls back token consumption, membership and confirm audit. It can be retried
after unban before expiry. Email confirmation errors still follow the existing
runner error handling; repository rollback does not promise automatic replay
after an operator removes a ban. Leave and help are not denied by bans; their
existing rate limits remain. Existing memberships are not evicted.

List ban create/delete now acquire the existing site-wide `subscription_rate`
writer reservation before the list writer, matching workflow serialization.
The broad reservation trades concurrency for atomic admission; SQLite tests
cover transaction effects and reservation failures, not PostgreSQL contention.
Out-of-band global-row SQL edits have no new writer-coordination guarantee.
Privileged member CRUD/imports and global-ban administration are not changed.

## Experimental moderator rejection notices

`moderation::review_tx(Reject)` and the legacy `reject`/`dispose` path share one
transaction-local publisher. It reloads the original stored envelope and raw
message, checks the list association and canonical held-sender agreement, and
applies the conservative owner-mail admission guard. Invalid/null senders,
self-list/control addresses, malformed context or headers, and automatic/list
messages produce no notice; rejection and the full moderator reason still persist.
Accept, defer and discard do not publish rejection notices.

An admitted rejection creates a new message/blob, one out job and one recipient.
The historical `workflow_notices` table now provides job-bound provenance for
both subscription and moderation producers. Its private publisher accepts no
external raw MIME; the rejection entry point builds fixed headers and a base64
UTF-8 plain-text body, rendering at most 4096 comment bytes at a character boundary
plus a truncation marker. Routing context contains the list ID only, never notice
authority. The existing outgoing provenance check preserves generated MIME and
uses `MAIL FROM:<>`; subscriber cooking, archive and digest are bypassed. Original
message bytes are not copied into the notice or changed in storage.

Disposition, notice publication, moderation log and audit commit or roll back
together; a repeated decision conflicts without duplicate publication. This is
durable publication, not exactly-once SMTP delivery. Existing outgoing retry and
ambiguous-delivery semantics apply. Admission does not authenticate the remote
envelope sender: moderators should discard suspected spam/forgeries rather than
reject them. Automatic policy rejections, hold/owner notifications, configurable
templates/languages and complete anti-backscatter protection are outside this slice.

## Experimental list-scoped posting bans

`db::bans` uses the existing bans schema. Exact mailboxes are normalized through
the core Address identity; patterns beginning with `^` preserve their original
case and compile using Rust regex, with a 1024-byte source, 1 MiB compiled-size
and 64-level nesting limit. Whitespace/control input and unsupported regex syntax
are rejected before acquiring a writer. Creates/deletes reserve the workflow
writer and then the list writer, and commit the ban mutation plus immutable
edge user/token/IP audit in the same transaction. Duplicate creates conflict;
missing list/ban operations return not-found. Collection SQL filters by list
before applying stable lexical ordering and bounded LIMIT/OFFSET. Count and page
are separate reads, not a concurrent snapshot or stable pagination across edits.

REST shares `authorize_list`, JSON/form extraction and native/compat pagination.
Only list-local rows are exposed; global rows are neither listed nor deleted.
Posting fact gathering compares exact bans with canonical mailbox identity on
both sides, while regexes see the original sender. This prevents trailing-dot
and IDNA spellings from bypassing an exact ban without changing regex case rules.
The in processor's existing reject decision ACKs the input without children;
raw retained bytes are unchanged. There is no SMTP-time failure or generated
rejection notice. Removal affects subsequent policy evaluations, not posts that
have already passed evaluation. This slice does not fence concurrent ban changes
against posting publication or alter owner routing. Public/email subscription
admission is described above; global administration remains absent. SQLite fixture execution is not PostgreSQL,
full Mailman-client, or live transport acceptance.

## Per-list posting size moderation

Migration `0014_message_size.sql` adds a default-zero, nonnegative
`max_message_size` column bounded at 2147483647 KiB for portable INTEGER
storage. Repository updates validate the JSON integer and commit with the
existing list-config audit transaction. Both schema corpus consumers include
the additive snapshot without weakening historical equality.

The in runner samples the list setting at policy evaluation and measures the
original stored bytes (headers plus body), using wide integer arithmetic for
the KiB conversion. Null-sender, ban and loop decisions retain precedence;
otherwise an oversized post is held before membership accept can bypass the
control, including posts from owners. The existing durable moderation path
retains raw bytes and produces no delivery/archive/digest children on hold.
An exact-boundary post passes the size check; zero disables only this check.
The global LMTP transport cap and administrative/command routes are unchanged.
This does not establish streaming intake or a process-memory bound.

REST config GET/PATCH/PUT and OpenAPI expose the field on both prefixes.
Only form input converts known numeric config strings (`max_message_size`,
`next_digest_number`) at the HTTP boundary; JSON strings remain invalid.
PATCH preserves omitted fields and PUT restores the default-zero limit.
SQLite fixture coverage is not PostgreSQL or live MTA acceptance.

## Experimental owner forwarding

The LMTP resolver recognizes `-owner` only after exact-list lookup. It derives
`owner_route` from the envelope recipient, never from message headers/body.
`mail::owner` conservatively rejects null/unsafe senders, automatic/list traffic,
malformed headers and own-list posting/suffix/plus routes. Domain identity uses
the same normalization as intake, while SMTP mailbox validation additionally
rejects whitespace/control bytes and non-ASCII/non-dot-atom transport addresses.

`db::owner_mail::forward` reserves the list and live input lease, snapshots
addresses having an owner or moderator membership using `EXISTS` (one transport
address even with both roles), and rejects empty/unsafe rosters. An outgoing
child, recipients, `owner_deliveries` provenance, audit and input completion
commit together. The live deadline is rechecked after the final ACK/audit write;
stale leases and audit faults roll the whole handoff back. Invalid owner routing
is explicitly shunted rather than acknowledged or sent to subscribers. No
archive/digest child or recooked intake blob is created.

The outgoing worker requires job-bound `owner_deliveries` provenance to select
owner cooking. An arbitrary outgoing context flag is insufficient. Header
allowlisting retains author/reply/thread/MIME presentation, removes Bcc,
approval/authentication/transport claims and unrelated controls, preserves body
octets, adds `Auto-Submitted: auto-forwarded`, and uses a null reverse path.
It does not apply post subject prefixes or anonymous-list rewriting. This is
administrative forwarding, not full configurable owner-chain policy or the
complete notices lifecycle. Prepared-byte tests do not certify remote SMTP
acceptance, DMARC alignment, PostgreSQL or MTA cutover.

## Lease renewal scheduling and authority

`runners/src/heartbeat.rs::run_while_renewing` delegates to one private
`renewing` scheduler; its callback still calls the real queue repository's
`.live().heartbeat`. Initial ownership validation, biased renewal priority,
TTL/3 cadence and per-renewal timeout are unchanged. Timeout or repository
failure drops work and returns `LeaseLost`; no new retry or grace period exists.
Persistent writes retain their own post-lock live-clock fencing.

Paused-time tests drive the same scheduler with controlled renewal futures:
work outlives a 150ms lease through eight renewals, failed initial validation
never polls work, mid-operation failure drops work, and a blocked renewal drops
both futures before an outer watchdog. Actual SQLite tests separately verify
competing-claim exclusion, exact-expiry recovery, stale-owner rejection, and
real wrapper renewal before polling work. Holding the sole pool connection
exercises real timeout cancellation without expiring the still-valid lease.
This is compositional scheduler/repository evidence, not a proof of real-time
scheduling latency or a new PostgreSQL/MTA acceptance run.

## MTA map generations

`listmngr_mail::mta::MapWriter` is the one producer of MTA lookup data:
`render` turns list identities into `(file, content)` pairs for Postfix
(`regexp` rows, or Mailman's `hash` rows) or Exim (`lsearch` lines), and
`publish` writes them into a private staging directory, applies the
configured permissions, compiles `hash` maps with `postmap`, renames the
directory to a `generation-<uuid7>` name, swaps the relative `current`
symlink and prunes beyond `map_generations_kept`. The generator and the LMTP
dispatcher share `COMMAND_SUFFIXES` (now defined here and re-exported by the
runners crate); exact list names still win over suffix routing.

The writer is built from `[mta]` by `MapWriter::from_config` (validated at
config load by `MtaConfig::validate_maps`) and used from three places:
`serve` publishes before the HTTP listener is served, REST list creation and
removal call `refresh_mta_maps` after their transaction commits, and the CLI
does the same after `lists create/remove` and in `aliases regen`. Map
publication is a filesystem side effect outside the database transaction, so
a failure after a committed list change is reported and repaired by rerunning
`aliases regen`, never rolled back into the list change.

Tests cover rendering for all three formats, `LmtpTarget` parsing, symlink
switching, permissions and pruning, a stand-in `postmap` and its failure, the
CLI flags and the regeneration hooks, REST regeneration, startup publication
in the process-level e2e, and real `postmap` lookups for both Postfix formats
(ignored, `POSTMAP_BIN`). `scripts/check-mta-configs.sh` runs the shipped
Postfix image and the Exim routers against fixture generations in containers.
See `POSTFIX_MAPS.md` and acceptance IDs `P2-MTA-MAPS` and `P2-MTA-INTEGRATION`.

## Owner browser list settings

`GET/POST /web/lists/{id}/settings`, linked from `/web/admin`, exposes exactly
`display_name`, `description`, `advertised`, `default_member_action`,
`default_nonmember_action`, and `archive_policy`. Forms reject unknown,
duplicate, missing and invalid enum/boolean fields before business mutation;
text fields retain the ordinary list repository's string contract. The existing
browser request-body limit, HTML escaping, same-origin checks and CSRF checks
apply. `default` maps to NULL (the configured server fallback); explicit `defer`
retains its existing runtime meaning. No bearer authority is created or accepted
as a substitute for a browser session.

Both read and save use `browser_write_tx` and the shared live `browser_owner_tx`:
verified list owner or verified server owner, never moderator-only. SQLite
reserves its writer before reading; PostgreSQL's existing NOWAIT table-lock
reservation conflicts with ordinary session/credential/user/address/member/list
DML. Session credentials and expiry are checked after reservation/retry and
again before commit. The ordinary `ListRepo::update_with_context` now delegates
to `ListRepo::update_tx`; browser saves call that same validator, locked fresh
list snapshot, update and attributed `list.config` audit within their authority
transaction. There is no browser GET snapshot reused at POST, so unrelated
configuration cannot be restored from a stale form. Concurrent edits to the same
six exposed fields remain last successful writer wins; no optimistic conflict UI
is claimed. Broad-lock throughput remains unmeasured.

Evidence: HTTP 404→200 RED→GREEN, SQLite valid/foreign/moderator/revoked authority,
Origin/CSRF/validation and audit rollback; a shared 12-case causal lock corpus
observes actual lock failure before committing revocation or an unrelated field
change. Its SQLite execution passed. Real Chromium saved/reloaded all six fields
with escaped hostile text and no script element. All required local workspace
gates passed in `listmngr-composed-final-gates-9d8jigcg`. New PostgreSQL HTTP and
causal lock tests compile, but their live execution was permission-denied and
is not certified. See the settings evidence rows in `FEATURE_PARITY.md`.

## Owner member search

The member roster accepts `q` plus `page`. `browser_search_members` lowercases the
query against canonical stored email, escapes the explicit LIKE escape character
and wildcard characters, and applies the bound pattern inside the same authorized,
list/role-constrained SQL before LIMIT/OFFSET. The original unfiltered DB method
delegates with an empty query, retaining the same live owner transaction boundary.
Queries over 320 UTF-8 bytes or containing control characters are rejected at the
HTTP and DB boundaries; SQLite's NUL/LIKE behavior must not produce broader matches
or diverge from PostgreSQL's text handling.

`webui_member_query.rs` serializes only a local list-derived URL, not an arbitrary
return URL. Generated previous/next links and hidden policy-form fields retain
the query/page; both are validated before a policy write. A new GET search resets
the page. Empty matches have explicit feedback and a clear-search link. Query
values are escaped in HTML and encoded in URLs. The row/query bounds do not imply
a bounded SQL scan cost; substring search and broad-lock throughput are unmeasured.

## Browser member posting administration

`api/src/webui_admin.rs` serves the authenticated `/web/admin` index, per-list
`/web/lists/{id}/members` roster and protected member-policy POST. Account navigation
links to the index. HTML uses escaped identities and explicit unique label/control
associations. Both enumerations return 20 rows with one lookahead; page offsets
are capped. The index applies owner permission before LIMIT, not after fetching
a global page. Member-role rows alone are shown; owner/moderator-role rows are not
editable through this surface.

`db/src/web_admin.rs` owns live session/credential validation, verified owner
membership (including AsUser consistency), or verified server-owner authority.
The shared DML-conflicting browser transaction protects both reads and writes.
Policy updates constrain member ID, list ID and role in SQL, write an attributed
`member.update` audit in the same transaction, and recheck session expiry after
the writes. Audit failure rolls back the policy. No caller-provided admin flag or
synthetic API token is used.

Only `members.moderation_action` changes. The existing runner reads this field
through `policy_facts::gather_context`; this increment does not alter transport,
already-held dispositions, delivery snapshots or policy evaluation. In particular,
explicit `defer` currently accepts after safety checks, whereas NULL falls back to
the list default; the UI names these separately. Mass operations,
role administration remain future work. Causal lock/revocation
coverage here is SQLite; PostgreSQL has real HTTP/update/audit-rollback coverage,
not a new causal contention matrix. Broad-lock throughput remains unmeasured.

## Authenticated membership departure

`api/src/webui_membership.rs` exposes `/web/members/{id}/leave`: GET is a
non-mutating confirmation, POST uses the existing strict Origin/CSRF boundary.
The account link no longer routes members through an advertised-only list page.
The preview names the exact canonical membership address and list and offers
Cancel; it does not take a caller-supplied email or grant global-admin deletion.

`db/src/web_membership.rs` uses the shared browser writer boundary for both preview
and commit. Session/credential authority, verified address ownership, member role
and AsUser identity are rechecked inside it. The existing membership/preferences
deletion helper and attributed `member.delete` audit commit together, with a final
session check after the writes. Replay, foreign membership and owner/moderator-role
IDs are denied. Accounts, addresses, other memberships and administrative roles
are preserved. This does not cancel immutable queued delivery/digest snapshots or
change the anonymous email-confirmation workflow. Coarse-lock throughput remains
open; dedicated causal contention here is SQLite, while PostgreSQL covers HTTP
behavior and audit-failure rollback.

## Authenticated password change

`api/src/webui_password.rs` exposes `/web/account/password` GET/POST with the
existing Origin/CSRF/session checks, 8 KiB form limit and shared 5/min login
work limiter. The current password is required, confirmation must match, and
the database's configured strength/1024-byte password policy applies. Password
fields are never reflected into HTML, logged or included in audit data.

`db/src/web_password.rs` verifies the current hash and computes the replacement
before acquiring browser writer locks. Inside that boundary it revalidates session
authority and verified account-address availability, conditionally updates only
the proved credential hash/version, deletes all that user's browser sessions and
records `user.password` attributed to that user. A final expiry check after writes
and audit rolls back the whole operation if authority has expired. Credential,
session and audit rollback are tested together; other users' sessions survive.
The response clears the cookie and requires a fresh login. Independent API tokens,
email reset/recovery, signup, 2FA and coarse-lock throughput are separate concerns.

## Archive conversation hints

The archive processor validates the message's own external ID as before, but treats
References/In-Reply-To as optional hints. It selects the first valid Message-ID
from References, otherwise the first valid In-Reply-To, otherwise its own hash.
Invalid hint entries are skipped rather than failing archival with invalid
Message-ID. Existing repository completion still resolves the candidate to an
indexed parent root inside its list-lock transaction and unifies provisional
roots when a parent arrives late. Index/audit/final lease ACK fencing is unchanged.
Single/list parser representations are handled; no subject-based merging, historical
reindex, or automatic retry of previously shunted jobs is introduced.

## Session-authorized archive browser

`api/src/webui_archive.rs` serves `/web/lists/{id}/archive` through the existing
browser security-header middleware. It reads the existing policy-gated archive
repository without synthesizing a bearer identity. Public archives remain anonymous;
private archives use the DB-owned browser authority described below. The repository
rechecks policy at its read boundary and
returns currently cooked MIME-derived subject/plain text, which the page HTML
escapes rather than rendering message HTML. Requests select at most 21 rows to
render 20 plus a next-page indication. Page numbers are 1–5001; search and thread
values are at most 200 UTF-8 bytes. Links URL-encode filters and HTML-escape URLs.
Search is the existing bounded literal SQL substring search, not ranked full text.
Per-message links use `?message={hash}` and `ArchiveRepo::read_browser_message`, selecting
one exact list-scoped hash under the same repository authority and current
publication cooking as archive search. Unrelated raw messages are not
fetched or decoded. Hashes must be nonempty and at most 200 UTF-8 bytes. Combining
a permalink with a nonempty search/thread filter or page other than 1 is rejected,
not silently ignored. Missing hashes return 404; links remain subject to current
private/never policy, and are not a grant of access. These are native browser URLs,
not a claim of HyperKitty URL compatibility.
The browser's `format=mbox` projection uses the same authorized selection and
cooked MIME payloads with the existing mboxrd serializer. It fetches only 20 rows,
not the HTML paginator's 21st look-ahead row, or exactly one permalink message.
The response is an `application/mbox` attachment named `archive.mbox` under browser
no-store/nosniff policy. The generated download URL preserves page, search, thread
and optional message identity; unsupported formats are rejected. This bounded
selection download is not full-archive backup/export completeness or a new
aggregate byte-memory limit.
`db/src/browser_archive.rs` owns private page/thread/search/message/export reads.
It acquires the existing browser DML-conflicting serialization boundary, then
revalidates the opaque session token, CSRF/user binding, credential version,
expiry and verified address/member ownership (including AsUser identity). A
server-owner flag does not bypass membership. Current archive policy and all raw
rows are read on that same transaction; session validity is checked again after
the archive SELECT before commit, then the shared publication renderer handles
the authorized bytes. No business state or audit row is written by an archive read.
The account page links directly to enabled archives, including unadvertised lists.
Public-only fallback keeps its existing authorization-qualified SELECT.

This implementation reuses coarse browser writer locks, including unrelated
tables; its throughput/contention cost is not resolved. SQLite actual-writer-wait
regressions cover revocation and expiry; PostgreSQL executes the HTTP matrix but
does not yet have a private-archive-specific causal contention matrix. Richer
thread presentation remains unimplemented.

Individual attachments use `?message={hash}&attachment={zero_based_index}` through
the same `read_browser_message` authorization and list/hash selection; no spool or
filesystem access is introduced. Search/thread/page conflicts and mbox overrides
are rejected. `mail/src/attachments.rs` projects the pinned MIME parser's attachment
sequence from authorized cooked bytes. Input is capped at 10 MiB, attachment count
at 64, displayed names at 200 characters; parser-reported encoding errors reject
the projection instead of exposing its recovered fallback as a valid download.
Binary parts retain transfer-decoded bytes. Text/HTML attachments are decoded
from the selected part's original `offset_body..offset_end` in the parsed message,
using its transfer encoding, not the parser's charset-normalized display string.
This preserves non-UTF-8 text payload octets and content line endings without
changing archive display text or bypassing current publication/authorization.
These limits apply after the repository read/publication stage, not to total DB
transfer, earlier parsing, nested-content privacy or process memory. Oversized or
invalid messages remain readable but show an attachment-unavailable notice.
Names are HTML-escaped and never used in paths or response headers. Downloads
always use `application/octet-stream`, `attachment; filename=attachment-N.bin`,
and existing no-store/nosniff middleware, including HTML attachments. This does
not scan downloaded files or make them safe to execute locally.

## Composed-source verification boundary

Parent verification of the integrated browser/login, held-recipient, email,
digest/archive and lease source passed the required workspace artifact, format,
locked build/test, strict Clippy, deny and audit gates (with only the documented
`RUSTSEC-2023-0071` audit ignore). The PostgreSQL script executed all 13 exact live
tests in a newly created fixture database. Four additional PostgreSQL UI tests
executed login issuance, forms/session bounds and in-flight revocation/authority
matrices, each in its own new database. Their source fingerprint was unchanged;
all created databases were dropped and their absence verified.

Actual Chromium exercised login, preferences, confirmation, held review and
logout with database effect assertions. Confirmation used a private DB token
bridge, not SMTP. These gates do not establish PostgreSQL recipient-selection
contention coverage, hosted deployment acceptance, or full Mailman parity.
Exact commands and logs are recorded in `FEATURE_PARITY.md`.

## Mailbox identity versus transport

Subscription workflows store normalized `email` for identity/rate limits and
`original_email` for eventual delivery (migration `0010_workflow_mailbox.sql`).
Confirmation validates that the two normalize identically and preserves existing
address ownership and spelling. It does not verify a pre-registered user account:
list membership confirmation is not account authentication. Regular recipient
selection emits `original_email`; own-post exclusion compares normalized identity.
Legacy workflows can only backfill the spelling previously persisted, not recover
case that was already lost. Digest recipient spelling needs composed verification.

This document describes the implemented Phase 0/1 runtime and experimental opt-in Phase 2 mail role. Phase 2 acceptance and later roadmap components remain incomplete.

## Composed acceptance boundaries

### Lease-clock follow-up

Queue and sibling archive/digest repositories preserve explicit-now fixture APIs
and opt into live clocks in production. Completion locks the list and queue,
performs publication writes, then samples the clock again immediately before ACK;
an expired lease rolls back publication and audit together. The archive runtime
uses `process_live`, while deterministic callers retain `process(..., now_ms)`.
Injected-clock regressions exercise pool/row/list/publication contention with
valid-lease controls on SQLite and isolated PostgreSQL. These gates do not certify
browser transaction-bound authorization or standalone live email-only acceptance.

The PostgreSQL gate explicitly runs digest publish rollback/single-winner and
concurrent list-settings PATCH regressions in isolated schemas. SQLite semantic
schema extraction includes implicit uniqueness of an `INTEGER PRIMARY KEY` rowid
alias, not only entries exposed by `pragma_index_list`.

Accepted held posts can start independent outbound, digest and archive jobs.
SMTP completion alone is not queue quiescence. The real-client replay probe
waits for all accepted child jobs to finish before asserting an unchanged queue
snapshot, while retaining the duplicate-audit and exactly-one-delivery checks.

## Durable email-command integration

`listmngr_core::EmailCommand` is the single serialized command model. The mail
adapter parses bounded MIME carriers without depending on the DB; inbound LMTP
stores that typed command in durable context and the `in` worker dispatches it
before posting-policy evaluation. Exact list addresses take precedence over
supported command suffixes. Automatic/null envelope senders are rejected for
commands; unsupported syntax is a permanent DATA failure rather than a silent
posting route. `-owner` uses the separate administrative route above; other
unsupported command families remain rejected.

The workflow repository commits command ACK, challenge/help notice, cooldown,
membership/token effects and audit together. Production calls `.live()`; the
shared injectable lease clock is sampled after the rate/queue locks and again
immediately before the final fenced ACK, including throttled/unknown-list paths.
Post-business expiry rolls the entire transaction back. Confirmation still binds
to the stored mailbox, never a mailbox argument from the reply. Migration 0010
preserves transport spelling; 0012 stores bounded per-list/mailbox help cooldowns.
The original Phase 1 schema snapshot is unchanged; additive schema assertions
use the existing separate composed snapshot.

Help notice rendering in `db/src/workflows.rs` now emits a list-derived
`Reply-To` pointing to the request bot, while retaining the owner `From` address
and explicitly documenting separate human contact. Users must replace the help
subject with one supported command; this does not add natural-language parsing,
subject fallback, scripts, or new commands. The SMTP fixture parses the delivered
help's Reply-To, submits a `join` there via `InboundHandler`, consumes its leased
command through the live-clock workflow repository, then observes a second SMTP
DATA containing the confirmation. Both sends use the exact envelope mailbox and
null reverse path; input From/Reply-To/body are not reflected. Both input/output
jobs finish, with no membership, owner delivery, archive or digest post created.
This is handler → SQLite → actual outbound SMTP fixture evidence, not an LMTP
wire session, live MTA, PostgreSQL or whole-product acceptance.

Passing commands: `cargo test --locked -p listmngr-runners --lib help_reply_reaches_command_bot_and_sends_confirmation_not_owner_mail -- --nocapture`;
`cargo test --locked -p listmngr-mail -p listmngr-db -p listmngr-runners --all-targets`;
`cargo clippy --locked -p listmngr-db -p listmngr-runners --all-targets --all-features -- -D warnings`.
Logs: `target/help-reply-evidence/`; see `P3-HELP-REPLY` for RED and gate details.

Both challenge and help notices register their specific outgoing job in
`workflow_notices` (migration 0011), atomically with spool/audit. Outbound bypass
uses that DB provenance, not a JSON notice marker or Auto-Submitted header.
Forged markers remain ordinary posts subject to cooking and the list bounce
sender. Digest provenance, cooked archive publication, and accepted-post fanout
to Out + Digest + Archive remain unchanged. `policy_facts` continues to emit
original recipient spelling and compare normalized own-post identity.

Standalone live email-only join/leave/restart acceptance is **BLOCKED by denied
authorization**. Neither the denied harness nor an equivalent standalone flow
was executed during integration. Existing fixture unit/DB/runner tests are
permitted regression evidence; they do not establish live email-only acceptance
or the broader Mailman product contract.

## Dependency direction

The intended direction is:

```text
cli -> api / web / runners -> archive -> pipeline -> mail / db -> core
```

Dependencies must not point back up the graph. The nine workspace crates have these responsibilities:

| Crate | Responsibility in the current tree |
|---|---|
| `listmngr-core` | Domain identifiers/types, configuration loading and redaction, shared errors |
| `listmngr-db` | SQLx pool, embedded migrations, repositories, transactional persistence/audit boundary |
| `listmngr-mail` | Bounded Message-ID metadata/hash helpers, standalone immutable filesystem storage, byte-safe header cooking (`cook_headers`), a bounded RFC 2033 LMTP session state machine (`lmtp`), and a plaintext SMTP client (`smtp`) — protocol libraries exercised by stream tests and wired into the opt-in real-socket mail role |
| `listmngr-pipeline` | Pure inbound posting-policy decisions (`policy::decide_posting`) and enabled-recipient selection (`policy::select_recipients`); no DB/network access, no delivery claim |
| `listmngr-runners` | Opt-in LMTP intake, inbound policy, outbound SMTP, heartbeat and role supervision; not full Phase 2 acceptance |
| `listmngr-archive` | Future archive boundary; no Phase 5 archive claim |
| `listmngr-api` | Axum `/api/v1` and Mailman-compatible `/3.1` adapters, auth, OpenAPI |
| `listmngr-web` | Dependency-free escaped semantic HTML and local responsive CSS used by the browser router; not full Phase 4 administration |
| `listmngr` (`cli`) | Binary commands, configuration bootstrap, migration, and HTTP process startup |

## Process and request model

`listmngr serve` runs one Tokio process. In the current phase it starts HTTP endpoints for liveness (`/healthz`), database-backed readiness (`/readyz`), metrics, API compatibility, and OpenAPI. HTTP adapters authenticate and authorize before calling repositories. With `mta.enabled`, it additionally runs experimental LMTP intake and inbound/outbound queue workers. Digest, bounce, and archive workers are not implemented.

`/healthz` proves that the process can answer. `/readyz` proves database-backed readiness. `listmngr status` is an HTTP client of these two endpoints, in that order, not a second database connection. It uses `web.listen`, maps wildcard binds to IPv4/IPv6 loopback, disables environment proxies and redirects, and limits each request to two seconds. Exit codes distinguish transport failure (3), unhealthy HTTP status (4), and not-ready HTTP status (5). The same binary probe runs in the shell-free container. None of these signals is a substitute for end-to-end mail delivery evidence.

## Browser boundary

`api::webui` mounts `/web` outside API Bearer/Basic middleware but inside the
outer request-correlation layer. `web` only renders escaped HTML and local CSS;
there is no JavaScript dependency or fabricated archive endpoint. CSP denies
scripts, external resources and framing. Trace logging records paths, not query
strings. `Referrer-Policy: strict-origin` strips credentials in paths/queries while
allowing Chromium native form POSTs to retain their same-origin Origin header.

Migration `0009_web_sessions.sql` stores SHA-256 session-token digests, CSRF
secrets, optional user IDs, credential versions and fixed expiry. Login rotates
and audits atomically; logout revokes and audits atomically. Password changes
invalidate a session on subsequent lookup. Lookup checks time after acquiring a
pool connection; session issuance cleans at most 100 expired rows. Browser
credentials are not API scope tokens and cannot authorize `/api/v1` calls.
Exact configured Origin and constant-time CSRF comparison protect all form
POSTs, including login and anonymous request/confirm forms. HTTPS sets Secure;
HTTP is allowed only with a configured loopback hostname/address. A small
node-local global login bucket bounds password hashing (five attempts/minute).

Self-service actions require current verified address ownership; AsUser records
also require matching user identity. Moderator access requires current verified
linked owner/moderator membership for the specific list, or server-owner status
and a verified linked address. Held review calls the existing transactional
moderation repository, preserving pending-state fencing, comments, audit and real
queue/recipient effects. Browser audit currently attributes user, not API token
or socket peer IP. Public forms call the existing durable subscription workflow;
GET confirmation is non-consuming. Neither browser acceptance nor repository
notice inspection proves SMTP delivery.

The public directory, account subscriptions, moderation index and held queues use
SQL LIMIT 21 with 20 rendered records, ordered pagination, and an explicit maximum
page of 10,000. Account queries filter verified address ownership, subscription
mode and member role before LIMIT/OFFSET; moderation discovery uses SQL EXISTS
for verified list roles or verified server-owner authority before pagination.
Held raw previews use SQL `substr` before fetching bytes, capped at 65,536 bytes
each. Resource-specific write/held authorization still enumerates memberships;
full large-install scalability is not established. Authorization is checked at handler
entry, outside the downstream mutation transaction: concurrent ownership/role or
credential changes during an in-flight action have not been linearizability-tested.
A subsequent browser-write repair supersedes the preceding handler-only warning
for preferences and moderation mutations: `browser_preferences` and
`browser_review` revalidate session, credential version, expiry and authority in
the business/audit transaction. SQLite uses BEGIN IMMEDIATE; PostgreSQL uses
SHARE ROW EXCLUSIVE NOWAIT locks conflicting with ordinary revocation DML and
rolls back before retry. These coarse locks serialize browser writes; throughput
has not been benchmarked. GET authorization and held recipient resolution before
the transaction remain separate open boundaries; the held-recipient snapshot
follow-up is explicitly NOT closed by the login repair.

`Database::browser_login` now owns password verification through session issuance.
Its private non-Debug proof contains the exact Argon2 hash, credential version,
user ID and canonical verified address. Hash verification finishes before writer
reservation. `issue_browser_login` acquires `browser_write_tx`, samples the clock,
and rechecks exact credential material, live verified address ownership, and the
predecessor token/CSRF/user/expiry/version. Rotation, bounded expiry cleanup and
`web.login` audit share that transaction; denial or audit failure mints nothing.
The older `create_web_session(Some(user), ...)` is a trusted low-level provisioning
primitive, NOT a password-authentication boundary; the HTTP login never calls it.
No schema, pagination, preference or held-recipient selection behavior is changed.

Fresh commands from `/tmp/listmngr-webui`:

```sh
CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo test -p listmngr-db --lib browser_login_issuance -- --nocapture
CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo clippy -p listmngr-db -p listmngr-api --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo test -p listmngr-api -p listmngr-db
```

All passed; README records the explicitly executed disposable PostgreSQL command.
Both backend lock matrices observe actual database busy/lock-not-available errors
before committing ordinary competing DML. They cover valid login, hash/version
changes, address unverify/unlink/reassignment, predecessor expiry/rotation/CSRF,
and audit rollback. The deterministic initial reset test failed against the old
issuance handoff while the valid-login counterpart passed, then both passed after
the repair. Later lock matrices are supplemental coverage, not retroactive RED.


## Data and transaction boundaries

Posting defaults are nullable per-list overrides over site defaults. The
announcement preset supplies `hold`, including an additive migration for existing
announcement lists. The same persisted data feeds inbound policy. Config PATCH
reserves the writer before its read/merge; isolated PostgreSQL lock-wait and SQLite
audit-failure tests protect that boundary. The pure digest renderer uses pinned
MIME parser/builder dependencies and stable issue IDs/boundaries; its passing
tests do not establish the still-in-progress durable digest/archive integration.

### Explicit uncertainty recovery

`listmngr-db::queue_operations` provides operator resolution separate from normal
lease-owned delivery. `queue recipients` exposes email/status/detail without raw
mail or attempt tokens. `queue resolve` locks an inactive outgoing/quarantine job,
conditionally changes exactly one ambiguous recipient, and records `queue.resolve`
with the operator reason in the same transaction. Acknowledged retry restores the
outgoing queue and only the selected recipient's pending state; confirmed sent or
failed outcomes do not schedule mail. Active/stale/repeated resolutions conflict.
Automatic workers still never retry an unknown result. CLI execution is trusted
local administration, not an HTTP self-service authorization capability.


PostgreSQL is the intended production backend; current mail-path production acceptance remains open. SQLite is a single-node development/test backend. SQLx migrations are embedded in `listmngr-db` and applied by `listmngr migrate` or server startup. Repository statements use numbered parameters accepted by both backends, and the live PostgreSQL contract exercises repeated migration plus Domain/User/Address/List/Member/Preferences/Token/Audit behavior rather than connectivity alone. Business mutations, owned-row cleanup, and their audit records share one transaction. IDs and timestamps use portable textual forms at repository boundaries.

The same numbered-parameter rule applies to SQL in API authorization helpers. A dedicated PostgreSQL router test exercises list/domain-scoped user reads, preferences, addresses, and collection filtering under both prefixes, including forbidden-user controls; repository-only tests cannot establish this boundary's portability.

Persistent application state belongs in the configured database. For systemd, `/var/lib/listmngr` is created by `StateDirectory=listmngr` and is the working directory. The strict service filesystem permits no broad host writes. In Compose, the application root filesystem is read-only and only `/tmp` is an ephemeral, bounded tmpfs; PostgreSQL owns its named volume.

## Phase 2 durable intake boundary

Migration `0001_mail_queue.sql` adds blob, submission, and queue tables without
rewriting the Phase 1 migration. A submission has its own UUID: an external
Message-ID is not a trusted global deduplication key. SHA-256 identifies exact
blob bytes; identical bytes may share storage without conflating submissions.
Queue timestamps use Unix milliseconds and must be supplied consistently by the
caller. Queue mutations and their audit event share a database transaction.

`queue inject` validates list existence and the envelope sender, bounds the file
read to 10 MiB, and stores versioned JSON routing context (list, envelope sender,
archive Message-ID hash). It can only inject into `in`, not directly into a
delivery queue. `queue show` emits job metadata unless raw export is explicitly
requested. `queue ls` is bounded to 1,000 records. These are local administrative
commands with the same database-access trust boundary as existing CLI commands;
they are not public REST endpoints.

The filesystem store is a standalone library, not yet a selectable atomic intake
backend. Message lifecycle/GC is not provided by this slice. The experimental
mail role now supplies dispatch supervision. Queue lease fencing protects database state; it does not establish
exactly-once delivery to an external SMTP server. The attempt reservation described below bounds that separate side-effect boundary,
but does not close all runtime acceptance requirements.

Migration `0002_mail_policy.sql` adds `held_messages`, `moderation_log`, and
`delivery_recipients` on top of the queue tables. `Database::moderation()`
(`crates/db/src/moderation.rs`) atomically finishes a leased `in`-job and
durably holds its message (`hold`), or atomically records a held message's
disposition alongside a new `out` job and its recipient snapshot (`accept`) or
alone (`reject`/`discard`) — each fenced (`disposition IS NULL`) so a duplicate
or racing moderation decision has no additional effect. `MailQueueRepo` adds
`heartbeat` (monotonic lease-deadline extension), `unshunt` (validated replay
onto a live queue with a fresh attempt budget), and `complete_with_children`
(atomic source-ack + child job(s) + recipient snapshot + audit).

With `mta.enabled`, `serve` binds LMTP before starting the mail role, then
supervises it alongside HTTP. The inbound and outbound processors renew their
own leases; outgoing preparation fails closed. The held-message REST handlers
share existing scope/list/domain authorization and pass `audit_context` to
`ModerationRepo::review`. That transaction fences pending state (including a
no-op UPDATE lock for defer), records the comment, and writes user/token/IP
audit attribution together with any disposition, delivery job, and recipient
snapshot. Audit failure rolls all business effects back. No schema change is
needed for these REST repairs.

### Outbound attempts and uncertainty

Migrations are additive: `0001_mail_queue.sql` introduces durable intake/queues,
`0002_mail_policy.sql` adds held moderation and recipient snapshots,
`0003_delivery_ambiguous_status.sql` adds ambiguous delivery status, and
`0004_delivery_attempt_token.sql` adds nullable `attempt_token`. The original
Phase 1 schema corpus is retained; separate additive snapshots extend its checks.

`MailQueueRepo::begin_delivery` fences the job lease, marks each selected pending
recipient ambiguous/in-flight with its owning lease token, and commits
`queue.delivery_begin` in the same transaction. `outbound::deliver_one` may connect
TCP first but does not start SMTP commands before that reservation commits.
Reservation failure prevents SMTP negotiation.

`MailQueueRepo::finish_delivery` atomically updates pending recipients or
reservations owned by its lease token and transitions/audits the job. Explicit
`RecipientOutcome::Transient` restores pending; omitted reserved outcomes stay
ambiguous, while omitted unreserved outcomes stay pending. If outcome persistence
or audit fails, rollback preserves the prior durable uncertainty. Reclaim and
repository-level `unshunt` do not make ambiguous recipients eligible for automatic
retry, and a new lease cannot resolve an old reservation. This is conservative
quarantine, not exactly-once delivery: a crash before DATA can also quarantine a
never-sent attempt. A done job means processing finished, not that all recipients
were sent mail. There is no operator uncertainty-resolution command/UI.

SQLite `finish_delivery` acquires `BEGIN IMMEDIATE` before reading recipient
counts, avoiding the previously observed read-to-write snapshot upgrade failure
under contention. PostgreSQL uses ordinary `BEGIN`. This writer-acquisition repair
does **not** fix the separate lease-clock issue O1. The SMTP one-shot connection
is dropped after the final DATA result rather than blocking publication on QUIT.

### Runtime limits and open P1 findings

- **R1 — OPEN:** LMTP wraps the whole `handler.deliver` batch in a timeout, while
  the inbound handler enqueues recipients sequentially. If A commits and B stalls,
  cancellation loses A's known result and may invite upstream retry. Exact reply
  cardinality and single-transaction queue tests do not close this gap.
- **O1 — OPEN:** callers capture `now_ms` before acquiring database locks. A fence
  can therefore use stale time after waiting beyond the lease deadline, including
  `begin_delivery`. Real two-connection deadline-under-lock coverage and an explicit
  production/synthetic clock seam remain required.
- The role requires `mta.enabled` and explicit `plaintext_trusted_relay` or
  verified `required` STARTTLS (see current transport section above). SMTP AUTH
  remains unsupported. Do not expose the unencrypted LMTP listener as a public MTA.
- No DKIM/DMARC/ARC, notices/bounce processing, digests, subscription workflows,
  archive, full administration UI, or Mailman migration is provided by this slice.

### Evidence boundary

Current focused SQLite real-TCP sink/audit rollback/reopen/reclaim, cancellation,
reservation fencing, mixed-outcome, and final-250/no-QUIT tests passed, as did the
parent's locked workspace build/tests/Clippy. The current PostgreSQL attempt gate
**timed out (no PASS)**. Its test is wired into `scripts/test-postgres.sh`; a
compiled ignored test is not backend execution evidence. Earlier seven-contract
PostgreSQL passes predate migration 0004. The parent independently reran the
pinned-client Phase 1 + held gate successfully on the current candidate before
committing; this SQLite-backed probe does not establish PostgreSQL acceptance.

`scripts/test-mailmanclient.py` runs the original Phase 1 probe followed by
`scripts/mailmanclient_held.py` against a real disposable SQLite-backed binary.
The held fixture creates messages via loopback LMTP, then uses installed
`mailmanclient==3.3.5` through Basic-auth API tokens and observes a plaintext SMTP
sink; it does not seed held/message/queue rows. Flavor-aware `parse_list_path`
accepts the client's `name@host` paths under `/3.1` without widening native IDs.
It covers count/list/get/properties/raw preview, defer comments, scope denial,
unsupported options, accept/replay and reject/discard. Serialized replay evidence
is not an exactly-once SMTP guarantee. Current and historical evidence, commands,
and acceptance IDs are centralized in [FEATURE_PARITY.md](FEATURE_PARITY.md).

## Configuration and secret flow

Configuration layers are TOML and `LISTMNGR__SECTION__KEY` environment overrides. The process does not parse `.env`; operators must source an env file or let Compose/systemd load it. Host-side database URLs use `127.0.0.1`; Compose-side URLs use service DNS `postgres`. Production deployments should use `database.url_file` or a protected environment file rather than command-line credentials. Diagnostic configuration output must remain redacted.

CLI passwords enter through a hidden terminal prompt, stdin, or an inherited Unix descriptor, never an argv password option. The reader rejects input beyond the shared 1,024-byte password limit and removes at most one terminal line ending. CLI adapter failures are classified by typed errors, not substring matching, and expose stable categories plus correlation UUIDs. Secret-file read errors are generic even when UTF-8 decoding fails. Exact member lookup/deletion reuses core email/IDNA validation; bulk synchronization retains the newer role-scoped atomic repository implementation.

## Deployment topology

```text
host client -> 127.0.0.1:8000 -> listmngr (UID 1000, read-only scratch image)
                                      |
                                      +-> postgres:5432 (Compose private network)

internet mail -> postfix:25 (172.28.0.25) --LMTP--> listmngr:8024 (172.28.0.10)
                    ^   reads mta-maps volume (current -> generation-*)
                    +-- SMTP relay <-- listmngr outbound (plaintext_trusted_relay)
```

The Compose Postfix image (`deploy/postfix/Dockerfile`, Alpine with a pinned
`postfix` package, the `postfix` user in a `listmngr` group of GID 1000) mounts
the map volume read-only, waits for the first generation, runs `postfix
start-fg` and reloads itself when `current` changes. Static addresses on the
Compose network let listmngr name the relay as `mta.smtp_relay` (a socket
address) and the maps name LMTP without DNS. Port 25 is the only published
port of that service; LMTP stays unpublished.

The Docker builder uses a pinned Alpine Rust image and emits a musl-linked release binary. The final `scratch` image contains the binary and CA roots only, runs as numeric UID/GID 1000 (which owns `/var/lib/listmngr`, so the map volume mounted there is writable), drops capabilities in Compose, and does not need a shell or `curl`. The systemd alternative uses the same non-root trust boundary plus syscall, address-family, kernel, device, home, and filesystem restrictions.

The builder also pins `musl` and `musl-dev` to `1.2.5-r12`: the Rust base includes a compiler but not the C headers required by `ring` and bundled SQLite. Package tools/headers remain outside the runtime image. The real-client acceptance harness (`scripts/test-mailmanclient.py`) uses a separate loopback process and disposable SQLite database, while `scripts/test-postgres.sh` independently exercises PostgreSQL CRUD, schema, authorization, queue, held, and attempt contracts; see the evidence ledger for which checkpoint passed. CI runs these behavioral probes in addition to Rust and anti-stub gates.

The real-client harness feeds its fixture password through stdin. The PostgreSQL-only gate uses `domains ls` after migration to check repository access; it deliberately does not invoke the HTTP `status` command without a running server. CLI network fixtures exercise both address families, unhealthy/not-ready/timeout outcomes, proxy isolation, and redirect rejection independently of a database.

## Security boundaries

Bearer tokens carry scopes and optional list/domain bounds. Mailman Basic compatibility is disabled by default, is accepted only below `/3.1`, and, when configured, trusts the actual socket peer CIDR—not forwarded headers. `/api/v1` accepts Bearer authentication and successful typed GET responses carry content-derived ETags; `/3.1` retains the Mailman-compatible JSON shape instead. Secrets are returned once and only digests persist. Reverse proxies must sanitize forwarding headers and terminate TLS according to the operator threat model. See `SECURITY.md` for threats and residual risks.


## P4-BOUNCE-WEB-RECOVERY — bounded local acceptance verified

Logged-in web confirmation now offers `GET /web/members/{id}/recover` and an
existing-CSRF/configured-Origin protected POST at the same URL. Only a live
session's verified, owned ordinary membership with **direct** `by_bounces`
preferences qualifies (as-user memberships must also match the session user).
The GET never mutates; the POST resets delivery status, bounce score and warning
cycle atomically with session-user-attributed `bounce.recover`. Other preferences
and historical bounce events are untouched. General preference editing still
rejects restricted reasons. The existing browser transaction barrier coordinates
with scorer/maintenance DML, with live-session revalidation after writes.

The user must verify their mailbox is working before restoring delivery. This
slice sends no challenge, probe or recovery email and provides no token-based
recovery. Earlier no-web-recovery evidence describes the previous implementation;
this narrowly authorized action does not establish full P4 or Mailman replacement.
Frozen `target/web-bounce-recovery-parent-gates-20260909-072736/` passed 54/54
gates: workspace558/0/36, mandatory PostgreSQL19/0/0, and native Chromium recovery
with actual restored SMTP delivery after scheduler/restart on both engines.
Foreign-member, bad CSRF/Origin, revoked-session and generic-preference bypass
controls preserve state; the healthy and still-disabled recipients remain distinct.
The SQL assertions retain complete historical bounce rows and exact audit actor.
SQLite's eight-case observed-BUSY matrix covers post-wait revocation/natural expiry
and moderator changes with a valid counterpart. DB rollback tests preserve status,
mode, score and warning state; an enabled maintenance control excludes recovery.
No recovery-specific PostgreSQL contention matrix, audit/COMMIT-wait expiry proof,
or same-identity multi-role/list execution matrix is claimed. Already-published
warning jobs remain durable; recovery cannot recall them or mail already sent.

Build/fmt/strict Clippy/client/browser/DKIM/TLS/AUTH/security passed with the
documented advisory exception; 334/334 source/harness hashes were stable, and
bounded independent static review found no P1/P2. Receipt:
`target/web-bounce-recovery-final-receipt.json`. Acceptance prose is a separately
checked post-run documentation change. Initial sequential RED/GREEN is recorded
in `target/web-bounce-recovery-handoff.md`; post-implementation security controls
are supplemental coverage, not retroactive strict per-guard TDD.

## Ordinary DSN issuance boundary

Migration 0026 binds new message intake to immutable list epochs and published
ordinary recipients to planned memberships. Opt-in `begin_delivery_with_dsn`
commits reservation + immutable HMAC issuance + audit before SMTP, with the final
lease fence after the last audit. The shared singleton transport negotiates ENVID
using current EHLO DSN capability, including after STARTTLS/AUTH. Historical
unbound jobs are not adopted; null/internal notices and digests are excluded.
See [DSN_ISSUANCE.md](DSN_ISSUANCE.md) for exact authority and evidence limits.
