# listmngr

## Webhooks on the web (`P6-WEBHOOKS-WEB`) — bounded local acceptance verified

A list owner manages the list's webhooks at `/web/lists/{id}/settings/webhooks`
(a "Webhooks" entry in the settings navigation) and a server owner every
webhook, the site-wide ones included, at `/web/admin/webhooks` (linked from
the admin index). The page lists each webhook with its URL, list or
"site-wide", enabled or disabled, events, secret fingerprint and
description, with Enable/Disable, Ping, Rotate secret and Remove beside
it, and an add form (URL, events comma-separated, description). Adding or
rotating renders the page once with the new secret in a marked block and
never again — the page is rendered on that response rather than redirected
to, so the secret is in no URL and no history. A webhook's own page lists
what it was owed, newest first: event, state, attempts, last status, last
error, created, next attempt. A list owner cannot see or touch another
list's or a site-wide webhook (`404`), every write needs the session's CSRF
token, and each is audited with the signed-in user as its actor.

## Plugins (`P6-PLUGINS`) — bounded local acceptance verified

A plugin is a Rust type linked into the binary, never loaded at run
time: it implements `listmngr_pipeline::plugins::Plugin` and adds any
of rules (with the links that put them into a `plugins` detour of the
default posting chain, taken after the ban check and before
moderation), handlers, pipelines a list may choose as its
`posting_pipeline`, and archivers a list switches on by name like the
built-in ones. `crates/plugins` lists the plugins a build carries — none
by default — and the binary installs them at start, before any registry
is built. `GET /plugins` (`system:read`) and `listmngr plugins` describe
exactly what this build's plugins add:

```sh
listmngr plugins
{"name":"example","version":"0.1.0","rules":["keyword-hold"],"links":["keyword-hold"],"handlers":["plugin-example-header"],"pipelines":["example-posting-pipeline"],"archivers":["example"]}
```

The line above is the example plugin, built with `--features
plugin-example` to see the wiring work: a post whose subject carries
`[hold-me]` is held, the `example-posting-pipeline` stamps
`X-Listmngr-Plugin: example` before delivery, and the `example` archiver
appends a line per archived post to the file
`LISTMNGR_PLUGIN_EXAMPLE_ARCHIVE` names. Without the feature the binary
carries no plugin and behaves, trace for trace, as before. Mailman's
Python plugin API has no counterpart here by design (`docs/PLAN.md`
§1): a plugin is reviewed, compiled and shipped like the rest of the
program.

## Webhooks: the API and the command line (`P6-WEBHOOKS-API`) — bounded local acceptance verified

Webhooks are managed over REST at `/api/v1/webhooks` (and `/3.1/webhooks`)
with the new `webhooks` scope, which `admin` implies:

```sh
curl -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"url":"https://hooks.example.invalid/listmngr","events":["member.*","moderation.hold"],"list_id":"dev.example.invalid"}' \
  https://lists.example.invalid/api/v1/webhooks
```

`POST /webhooks` answers `201` with the webhook and its `secret` — shown
this once; `GET /webhooks` (paged) and `GET /webhooks/{id}` never show it
again, only `secret_fingerprint`; `PATCH /webhooks/{id}` changes `url`,
`events`, `description` or `enabled`; `POST /webhooks/{id}/rotate` answers
with a new secret; `POST /webhooks/{id}/ping` queues a `ping` delivery;
`GET /webhooks/{id}/deliveries` lists what the webhook was owed, newest
first, with each delivery's `state`, `attempts`, `last_status`,
`last_error` and `payload`; `DELETE /webhooks/{id}` takes the deliveries
with it. A token bound to a list makes and sees only that list's webhooks
(`list_id` defaults to its list, another list is `403`, another list's
webhook is `404`); a token bound to a domain has none to see. The same
from the command line, one JSON line per webhook or delivery:

```sh
listmngr webhooks add https://hooks.example.invalid/ops --events 'list.*,member.create' --list dev.example.invalid
listmngr webhooks ls [--list dev.example.invalid]
listmngr webhooks set <id> --enabled false
listmngr webhooks ping <id>
listmngr webhooks deliveries <id> [--limit 50]
listmngr webhooks rotate <id>
listmngr webhooks rm <id>
```

Every change is audited (`webhook.create`, `webhook.update`,
`webhook.rotate`, `webhook.ping`, `webhook.delete`) with the actor and
never the secret. The operations are in `/openapi.json` and on `/api/docs`.

## Webhooks: delivery (`P6-WEBHOOKS-DELIVER`) — bounded local acceptance verified

With `[webhooks] enabled = true`, `listmngr serve` runs the webhook
runner — with or without the mail role, since events come from the API
and the web as much as from mail. It claims each due delivery under a
lease, posts it once as JSON and records what the target answered: a
2xx is `delivered`; anything else — a 5xx, a 4xx, a redirect (never
followed), a timeout, a connection refused — is retried ten seconds
later, then twenty, doubling to an hour, jittered, until `max_attempts`
(12) gives it up as `failed` with the last status and error kept. A
target the site may never reach is given up at once: a scheme other
than `https://` (unless `allow_http`), or a name that resolves to a
loopback, link-local, private, shared or unspecified address (unless
`allow_private_targets`). The name is resolved before the check and
the connection pinned to the addresses found, so a name that changes
between the two cannot steer a delivery elsewhere. A disabled webhook
keeps its deliveries until it is enabled again.

Every request carries `Content-Type: application/json`,
`X-Listmngr-Event`, `X-Listmngr-Delivery`, `X-Listmngr-Webhook`,
`X-Listmngr-Timestamp` (Unix seconds) and `X-Listmngr-Signature:
sha256=<hex>`, the HMAC-SHA256 of `<timestamp>.<body>` under the
webhook's secret — recompute it, compare in constant time, and refuse a
timestamp older than you like. `listmngr_webhook_deliveries_total`
counts attempts by `delivered`, `retried` and `failed`.

## Webhooks: the store (`P6-WEBHOOKS-STORE`) — bounded local acceptance verified

A webhook is a URL the site posts its audit events to — `list.config`,
`member.create`, `moderation.hold`, everything the audit log records
through the shared path — for the whole site or for one list. Events
are named by their audit action; a webhook subscribes to `*`, a prefix
such as `member.*`, or an exact action. When a write commits its audit
event, every enabled webhook the event matches is owed one delivery *in
that same transaction*: an event can neither be lost nor exist without
its write. A list-bound webhook receives only its list's events (the
target list, the `list_id` the diff names, or the list a member or held
message belongs to). This slice is the store — the tables, the
repository, the fan-out, the sweep; posting the deliveries is
`P6-WEBHOOKS-DELIVER`, and the REST and command-line surface
`P6-WEBHOOKS-API`.

```toml
[webhooks]
enabled = false                      # the runner; the store works regardless
signing_key_file = "/run/secrets/webhook-key"   # ≥ 32 characters, or signing_key
allow_http = false                   # only https:// targets otherwise
allow_private_targets = false        # no loopback, link-local or private addresses
max_attempts = 12
timeout_secs = 10
```

A webhook's secret is derived from the signing key and the webhook's
own salt (HKDF-SHA256) and shown once, on creation or rotation; the
database keeps the secret's hash — the first eight hex digits are the
fingerprint the API shows — and the salt, never the secret, so a
database on its own cannot sign a delivery. A delivery carries `{id,
event, at, list_id, target: {type, id}, actor: {user_id, token_id},
data}` where `data` is the audit diff, redacted as the audit log is
(no password, secret or hash ever leaves). Deliveries posted or given
up are collected by the task sweep after `finished_job_retention_secs`.

## Operator diagnostics (`P6-DOCTOR`) — bounded local acceptance verified

Run `listmngr doctor` with the same configuration as the service. It prints
one JSON document (`version` 1, `ok`, and `checks` of `id`/`status`/`detail`
with `ok`, `fail` or `skip`) and exits `12` (`CLI-DOCTOR`) when a required
check fails; a configuration or usage error is still the redacted exit `2`.
It never migrates or creates a database, sends mail or reloads an MTA. It
checks that the database's migration ledger is exactly the embedded one
(SQLite opened read-only, PostgreSQL in a read-only transaction), reads the
configured relay's SMTP greeting when the mail role is enabled (no `EHLO`,
nothing sent), and resolves each mail domain's MX exchangers or its RFC 5321
implicit MX (`--dns-server 127.0.0.1:5353` points it at another resolver).
A prerequisite that is disabled or unavailable is an explicit `skip`, never
a pass, and the checks this command does not make — TLS, AUTH, delivery,
SPF/DKIM/DMARC/ARC/PTR — are listed as `skip` so nobody reads their absence
as health. Backup/restore is still open (`docs/PLAN.md` §7).

## Mailman's translations (`P6-PO-TEMPLATES`) — bounded local acceptance verified

Notices speak every language GNU Mailman 3.3.10's translators brought, not
only `en` and `vi`: 30 more (`bg`, `bn`, `ca`, `cs`, `de`, `el`, `eo`, `es`,
`fr`, `he`, `hu`, `id`, `it`, `ja`, `ko`, `nb-NO`, `nl`, `oc`, `pl`, `pt`,
`pt-BR`, `ru`, `si`, `sk`, `sq`, `sv`, `tr`, `uk`, `zh-Hans`, `zh-Hant`).
A member, a list or the site set to one of them gets its notices with
Mailman's translated subject and, for the 17 templates whose English here is
still Mailman's (held-post and moderation notices, digest header and
masthead, the list footer, the admin subscription notices, …), Mailman's
translated body. The templates this project reworded — the welcome (no
password story), the confirmations (confirmed over HTTP), the bounce
notices, the rejection (nothing attached) — keep this site's English or
Vietnamese, because Mailman's translation would say something untrue here.
A translation that uses a placeholder Mailman's English does not is left
out. The language pickers for a list and a member offer them all; the
interface itself still speaks `en` and `vi`. `listmngr import3` keeps a
Mailman list's or member's language (`pt_BR` becomes `pt-BR`).

The catalogs are generated, never hand-edited:

```sh
uv venv mm --python 3.12 && uv pip install --python mm/bin/python mailman==3.3.10
mm/bin/python tests/compat/import_mailman_templates.py
```

It writes `crates/mail/catalog/mailman/<lang>/<name>.txt` (with Mailman's
English next to them, which `crates/mail/tests/mailman_catalog.rs` holds this
site's English to), `crates/i18n/locales/mailman/<lang>.ftl` and the two
Rust tables that embed them. Mailman's translations are GPL-3.0-or-later by
the GNU Mailman translators; `crates/mail/catalog/mailman/SOURCE` records
their origin and the terms under which they are combined with this
AGPL-3.0-or-later project.

## Mailman 3 import (`P6-IMPORT3-REST`, `P6-IMPORT3-USERS`, `P6-IMPORT3-HELD`, `P6-IMPORT3-DB`, `P6-IMPORT3-HYPERKITTY`) — bounded local acceptance verified

A whole Mailman 3 site moves over from a running core, read-only:

```sh
printf '%s' "$MAILMAN_REST_PASSWORD" > /protected/path/rest.pass
listmngr import3 --rest http://127.0.0.1:8001/3.1 \
    --user restadmin --password-file /protected/path/rest.pass --dry-run
listmngr import3 --rest http://127.0.0.1:8001/3.1 \
    --user restadmin --password-file /protected/path/rest.pass
```

Or, with the core stopped, straight from its database and `var_dir`
(the `[database] url` of its `mailman.cfg`, `sqlite:///…` or
`postgres://…`; the held messages live in `var/messages`):

```sh
listmngr import3 --db sqlite:////var/lib/mailman/data/mailman.db \
    --var-dir /var/lib/mailman --dry-run
listmngr import3 --db postgres://mailman:…@127.0.0.1/mailman \
    --var-dir /var/lib/mailman
```

Both read the same site into the same plan: the database path decodes
what Mailman stores — its enums as integers, its intervals, the pickled
lists, the pending key/values, the pickled `email.message.Message` of
each held message — into what the REST API would have answered, and is
checked against the REST answers of the very same core. Without
`--var-dir` the held messages are reported and left behind.

The archive's posts come over as HyperKitty's mbox export (`listmngr
archive import`, under the same Message-ID-Hashes and threads); what
readers left on them — votes, tags, each thread's category, favourites —
comes from HyperKitty's own database, alone or after the site:

```sh
listmngr archive import rust-users.example.invalid rust-users.mbox
listmngr import3 --hyperkitty postgres://mailmanweb:…@127.0.0.1/mailmanweb
```

Each reader is matched to an imported account by the address of their
Django account; what has no post, thread or account here is counted as
`skipped`, one `archive.import_interactions` audit event records each
list, and a second run writes nothing new. See `docs/MIGRATION.md`.

The importer reads the core's domains, its site bans, its accounts (each
with its addresses, the one it prefers and its own preferences), every
list (with `--list <list_id>` just one) with its `config`, the four
rosters with each member's own preferences, the list's bans, its header
matches, its template URIs, and what it was still holding — the messages
waiting for a moderator and the subscription changes nobody had decided
— and writes them here: the domains, the accounts (with their
addresses — an address nobody owns is adopted, one Mailman had verified
stays verified — and their preferred address), the lists (style
`legacy-default`, then the imported configuration), the members with
their preferences and their own moderation action, the bans and the
header matches, the held messages (kept whole, with the date the core
held them, so a moderator here accepts or rejects exactly what was held)
and the subscription requests a moderator still has to decide. A member
Mailman subscribed as a *user* is subscribed the same way here, against
the imported account. **An import mails nobody:** the moderators and
subscribers were told once already by the site being left behind. Mailman's spellings become this site's — `7d`
durations become whole days, a multi-line `acceptable_aliases` becomes
one alias per line, and the resources Mailman derives (the list's
addresses, `created_at`, `post_id`, `volume`, `usenet_watermark`) are
read but never written. Nothing is written back to the core: the whole
import is `GET` requests under Basic authentication, and the password is
read from a file so it never enters the shell history or a process
listing.

`--dry-run` prints the plan as JSON and changes nothing. Every run prints
its report (`domains`, `lists`, `settings`, `members`, `owners`,
`moderators`, `nonmembers`, `skipped`, `bans`, `site_bans`,
`header_matches`, `warnings`) on standard output and its warnings on
standard error, each write carries its audit event, and one
`site.import3` event records the report. Running it again leaves
everything that is already here alone and counts it as `skipped`.

**Passwords do not come over.** Mailman hashes them with passlib, in a
scheme this site cannot verify, so every imported account is created
with a random password nobody knows, marked unusable: its owner takes
the account over through the password-recovery flow, and the import
warns once per account that had a password (never printing the hash).

Limits: a confirmation the subscriber still owed is not carried over —
the token belongs to the old site — and is reported instead; Mailman
keeps only a *URI* for each template, so each one is reported as a
warning to set by hand; a setting this site does not have, or a value it
cannot take, is a warning and the rest of the list still imports.
`docs/MIGRATION.md` has the mapping.

## Mailman 2.1 import (`P6-IMPORT21`) — bounded local acceptance verified

A Mailman 2.1 list moves over with its own `config.pck`:

```sh
listmngr lists create rust-users.example.invalid --display-name Rust
listmngr import21 rust-users.example.invalid /var/lib/mailman/lists/rust-users/config.pck --dry-run
listmngr import21 rust-users.example.invalid /var/lib/mailman/lists/rust-users/config.pck
listmngr archive import rust-users.example.invalid rust-users.mbox
```

The pickle is read here (protocols 0–2, Python 2 strings, and the
`Mailman.Bouncer._BounceInfo` instances 2.1 left in `bounce_info`, which
are ignored as Mailman's own importer ignores them), and the mapping is
Mailman 3's `import21`: settings with their renames and conversions,
topics, acceptable aliases, bans, header filter rules as header matches,
the four decoration templates with their placeholders converted, and the
rosters — members (regular and digest), owners, moderators, and the
addresses of the `*_these_nonmembers` lists as nonmembers — each with the
2.1 option bits, delivery status and language. `--dry-run` prints the
plan as JSON and changes nothing. Every run prints its report
(`settings`, `members`, `owners`, `moderators`, `nonmembers`, `skipped`,
`bans`, `header_matches`, `templates`, `warnings`) on standard output and
its warnings on standard error, and can be run again: what is already
there is left alone. Each write carries its audit event, and the import
adds one `list.import21` event with the report. `docs/MIGRATION.md` has
the full mapping table.

Limits: the list must exist first (and its domain with it); 2.1
passwords, `bounce_info` and the moderator password are not imported — a
warning asks for a new moderator password; a language 2.1 had that this
site does not is dropped with a warning; members are subscribed by
address, without user accounts.

## News to mail (`P6-NNTP-GATENEWS`) — bounded local acceptance verified

Mailman's `gatenews`, the other direction of the gateway. A list with
`gateway_to_mail` and a `linked_newsgroup` has its newsgroup polled by
the `nntp` runner — at start, then every `[nntp] gatenews_every_secs`
(300; `0` leaves polling to the command) — and by hand with `listmngr
nntp gate`, which prints one JSON line per polled list (`list_id`,
`newsgroup`, `watermark`, `gated`, `error`). The first poll of a list
only catches up: its `usenet_watermark` becomes the group's last article
and nothing is gated, so linking a busy group does not flood the list.
Each later poll reads the articles after the watermark (`GROUP`, then
`ARTICLE` by number), skips the list's own posts come back (a `List-Id`
naming the list) and articles without a `From`, and hands the rest to
the `in` queue as posts from Usenet — `To` moved to `X-Originally-To`
and set to the list, the `From` address as the envelope sender, the
context marked `fromusenet` so the post is admitted like any other but
never gated back out. The watermark moves article by article, and each
move is one transaction with the article it queues and its audit event
(`usenet.watermark`, saying whether the article was gated or passed):
a poll cut short never repeats itself and never loses what it queued.
An article the server no longer has, or one whose header block cannot
be read, is logged and passed; a session that breaks while an article
is being read — the server hangs up, times out — stops the poll with
the watermark where it was, so the next poll reads that article again
(`P6-NNTP-GATENEWS-FIX`). Two pollers on one list, the runner beside
`listmngr nntp gate`, gate an article once: the watermark is moved by
compare-and-set, and the poller that finds it already moved stops and
says so. A group the server does not know, or a server that cannot be
reached, is reported per list and moves nothing.

Limits: one reader session per poll (all lists in turn), plain TCP as
Mailman's; the `in` queue's admission decides the article's fate (a
moderated newsgroup gateway holds it like any post); Mailman's `HEAD`
pre-check is folded into reading the article whole.

## Mail to news (`P6-NNTP-GATEWAY`) — bounded local acceptance verified

Mailman's `to-usenet` handler and `nntp` runner. Configure the news
server once:

```toml
[nntp]
host = "news.example.org"        # empty (the default) leaves the gateway idle
port = 119
gatenews_every_secs = 300        # news → mail poll (P6-NNTP-GATENEWS); 0 = by hand
user = "gateway"                 # optional; AUTHINFO USER/PASS
password_file = "/etc/listmngr/nntp.pass"   # or password = "…"
# remove_headers and rewrite_duplicate_headers default to Mailman's lists
```

A list with `gateway_to_news` and a `linked_newsgroup` then has every
accepted post queued on `nntp` by the `to-usenet` handler (between
`to-digest` and `after-delivery`, as in Mailman; an article that came
from the newsgroup, `fromusenet`, is never gated back). The `nntp`
runner takes the post as it stood at `to-usenet` — the list's cooked
copy, prefix and `List-*` headers included — and prepares it as
Mailman's `prepare_message` does: `Approved: <posting address>` for an
`open_moderated` or `moderated` group, the subject without the list's
prefix unless `nntp_prefix_subject_too`, `Newsgroups:` set to the linked
group or the group appended to the poster's own, the `Message-ID`
unfolded (or one of the list's minted), a `Lines:` count, the transport
headers in `remove_headers` dropped (`Received`, `NNTP-Posting-Host`,
`X-Trace`, …) and the second and later `To`, `CC`, `MIME-Version` and
`Content-Transfer-Encoding` moved to their `X-Original-*` targets. It is
then offered over NNTP (`MODE READER`, `AUTHINFO` when configured,
`POST`, dot-stuffed). `240` completes the job; `441` — a `Message-ID`
the server already holds, from a cross-post — gets the article a
`Message-ID` of the list's own and one more try, a second refusal shunts
the job with the server's reply; an unreachable server backs off
(one, two, four … minutes, an hour at most) within the job's five tries.
A list that stopped gatewaying by the time its job runs completes it
without posting. Without a `host`, the runner idles and gated posts wait
in the queue.

Limits: plain TCP as Mailman's `nntplib.NNTP` (no NNTPS/`STARTTLS`); the
stripped subject is the list's prefix taken off wherever it stands, not
Mailman's `stripped_subject` from the prefix handler; a refused
credential or a `480` is a refusal that shunts, not a retry; the
news → mail direction (`gatenews`, the watermark) is `P6-NNTP-GATENEWS`.

## Usenet gateway settings (`P6-NNTP-SETTINGS`) — bounded local acceptance verified

Mailman's Usenet settings are list settings now, on both REST prefixes
(JSON and forms): `gateway_to_news` and `gateway_to_mail` (both off),
`linked_newsgroup` (a newsgroup name as Usenet spells it, or empty),
`nntp_prefix_subject_too` (on: gated posts keep the subject prefix) and
`newsgroup_moderation` (`none`, `open_moderated`, `moderated`); `PUT`
resets what it omits. `usenet_watermark` — the last article number gated
from the newsgroup — is read on the configuration and never written
through it: only the gateway records it, with an audit event
(`usenet.watermark`). Mailman's `news-moderation` rule joins the posting
chain in Mailman's place (after `max-size`): a list gatewaying to a
`moderated` newsgroup holds every post for a moderator, with Mailman's
reason. The gateway itself — `to-usenet`, the `nntp` runner and
`gatenews` — follows in its own packages; these settings decide nothing
else yet.

Limits: no settings page group in the browser (Postorius has none; the
REST configuration and `listmngr lists` are the way in); the watermark
has no writer until the gateway package.

## ARC sealing (`P6-ARC-SEAL`) — bounded local acceptance verified

Mailman's `arc-sign`, as `[mta.arc]`: with it on, every post delivered to
subscribers is sealed (RFC 8617) with the site's key after the list's own
DKIM signature, so a receiver that trusts this site can take the
`Authentication-Results` recorded at intake — DKIM, SPF and DMARC of the
post as it arrived — instead of the checks the list's changes break.

```toml
[mta]
authenticity_checks = true       # the seal carries these results

[mta.arc]
enabled = true
domain = "lists.example.org"      # d= of the seal; the key is published at
selector = "arc"                 # arc._domainkey.lists.example.org
private_key_file = "/etc/listmngr/arc.pem"
```

The `in` runner validates the chain a post arrives with (`arc=` joins the
results) and records its verdict; the pipeline keeps the post's ARC sets
where it would otherwise drop them as stale signatures; the out runner
adds the next set to each delivery — `i=1; cv=none` for a post without a
chain, `i=n+1; cv=pass` after a valid chain, `cv=fail` once after a
broken one and never again — with `ARC-Authentication-Results` carrying
the intake's results and `ARC-Message-Signature` covering Mailman's
signed-header set plus `DKIM-Signature`. The domain page lists the ARC
key's DNS record beside the DKIM ones. Owner mail, notices and digests
are the site's own and are not sealed; a post that carries no intake
record (one injected by hand) is delivered unsealed.

Limits: sealing needs `authenticity_checks`, so posts are checked with
the system resolver; the seal's timestamp makes two seals of the same
delivery differ; ARC is being reclassified as Historic by the IETF
(mail-auth keeps it behind a feature this build enables), so it is an
option for sites whose receivers still evaluate it, not a default.

## DMARC `wrap_message` (`P6-DMARC-WRAP`) — bounded local acceptance verified

`dmarc_mitigate_action` now takes every value Mailman's does:
`no_mitigation`, `munge_from`, `wrap_message`, `reject`, `discard`
(migration 0052 widens the column's CHECK, values preserved; the REST
config, OpenAPI enum and the DMARC settings page offer it). With
`wrap_message`, a post the `dmarc-mitigation` rule tagged (or every post
on an unconditional list) is delivered to subscribers as Mailman's
wrapper: a new message `From` the list with the author named — `Author
(address) via list` — the author reachable through `Reply-To` as
`munge_from` does, a fresh `Message-ID`, and only the headers a reader's
client threads and shows by carried over from the post (`Date`,
`Subject`, `To`, `Cc`, `In-Reply-To`, `References`, `Archived-At`,
`Precedence`, `List-*`, `X-Mailman-*`, plus `X-BeenThere` for the loop
rule); `Sender`, the original `Message-ID` and everything else stay
inside. The body is the post itself, whole, as an inline
`message/rfc822` part — after `dmarc_wrapped_message_text` (wrapped at
seventy columns) as an inline text part when the list has one, else as
the message's only content. The archive and digest copies are never
wrapped, an anonymous list needs no wrapper, and a post already from the
list is left alone.

Limits: the post inside is the cooked copy, as in Mailman — its
`Subject` carries the prefix and `cleanse-dkim` has already removed the
author's signature, so the wrapper preserves the author's headers and
body, not a verifiable original signature; the outer `Message-ID` is
derived from the post (the same post wraps to the same bytes) rather
than random; `Cc` is kept on the wrapper whatever `reply_goes_to_list`
says, where Mailman keeps it only when the author goes to `Reply-To`.

## Subscription by mail, end to end (`P3-SUBSCRIPTION-E2E`) — bounded local acceptance verified

`crates/cli/tests/mailpath_e2e.rs` now walks the subscription flow over
mail on the real binary: a stranger writes `subscribe` to `dev-join@`
through the LMTP socket; the server's challenge reaches the SMTP sink —
`Subject: confirm TOKEN`, `Reply-To: dev-confirm@`, the token in the
body, a null reverse path and `Auto-Submitted: auto-generated` like every
generated notice — and the roster is still empty; the reply, subject
intact, to `dev-confirm@` makes them a member, and the receipt and the
welcome (`send_welcome_message`) arrive; their first post is delivered
back to them with the list's prefix; `unsubscribe` to `dev-leave@` gets
a fresh challenge with a new token; its reply ends the membership and the
goodbye (`send_goodbye_message`) arrives; a replay of the spent join
token is accepted at the socket, as any durable command is, and changes
nothing and sends nothing.

The flow found one thing in the way: the per-address hourly cooldown on
requests counted every request of the last hour, so a member who had
just confirmed a join by mail could not ask to leave by mail for an hour
— the leave was dropped silently. The cooldown now counts only requests
still waiting (their token unspent): an outstanding challenge or a
moderator's queue row holds the address as before, an answered one does
not. Mailman has no such cooldown at all; this one stays, bounded to what
a stranger can produce for an address.

## Digest snapshot (`P3-DIGEST-SNAPSHOT`) — bounded local acceptance verified

The two digest issues are now compared with what Mailman 3.3.10 itself
writes. `tests/compat/generate_mailman_digest.py` runs Mailman core's own
`RFC1153Digester` and `MIMEDigester` under Mailman's test configuration
over fixed inputs — a list `news@example.invalid` with a header and a
footer template, volume 2, issue 3, a UTF-8 Vietnamese post and a post
with a binary attachment — and keeps the results as
`crates/mail/tests/fixtures/digests/mailman-3.3.10-{plain,mime}.eml` with
the inputs in `inputs.json`. `crates/mail/tests/digest_snapshot.rs`
builds the same issue from the same inputs and asserts the same text: the
plain issue line for line, the MIME issue part for part (each
`Content-Description` and text, then every post inside the
`multipart/digest` with its `Message: n` header), and `wrap` against
Mailman's `wrap` on four texts.

To agree with Mailman the renderer changed shape. The plain issue is the
masthead wrapped at seventy columns, the header template, `Today's
Topics:` with each post's subject without the list's prefix (also behind
a `Re:`) and its author's display name (the address when there is none),
seventy hyphens, then each post as `Message: n` followed by Mailman's
kept headers in Mailman's order (`Date`, `From`, `Subject`, `To`, `Cc`,
`Message-ID`, `Keywords`, `Content-Type`; decoded, unfolded, wrapped at
seventy columns with tab-indented continuations) and its scrubbed body —
every `text/plain` part's text, every other part replaced by Mailman's
note (`Name`, `Type`, `Size`, `Desc`), joined by the `next part` line —
behind thirty hyphens, the footer as `Subject: Digest Footer`, and `End
of … Digest, Vol X, Issue Y` underlined to its own length. The MIME issue
is the masthead (described by the issue's title), `Digest Header`,
`Today's Topics (n messages)`, the `multipart/digest` of the whole posts
each with a `Message: n` header, and `Digest Footer`; there is no closing
part any more, as Mailman's postamble never reaches the wire. The digest
is `From` the `-request` address with `Reply-To` the list.

Three Mailman quirks are named in the test and not reproduced: its
scrubber mangles an 8-bit UTF-8 body (`N\u1ed9i dung …`) where this
renderer keeps the text; it glues the `next part` line onto the last body
line where this renderer starts a new line; and it re-encodes an 8-bit
`From` as one RFC 2047 word holding the name and the address together.
Limits: the fixtures are one list, one language (`en`, whose masthead
Mailman encodes as `us-ascii`, so the fixture list's name is ASCII) and
two posts; the kept-header lists are Mailman's defaults, not configurable.

## Real-client suite (`P3-CLIENT-SUITE`) — bounded local acceptance verified

`scripts/test-mailmanclient.py` now runs, after the Phase 1 flow and the
held flow, `tests/compat/mailmanclient_suite.py`: the mailmanclient 3.3.5
doctest (`docs/using.rst`) section by section against a running listmngr
— domains, lists and styles, membership with tokens and pagination, users
and addresses, list settings (with one key per Postorius group), global and
member preferences, pipelines and chains, owners and moderators with
`find_lists`, subscription requests and their moderation, held messages
through `Queue.inject`, site and list bans with pages, archivers, header
matches, the configuration sections, templates on a list and a domain,
and the digest counters. Every call the doctest makes is made, with the
value the doctest prints; 232 checks in 18 sections. Where listmngr differs
on purpose the suite says so and asserts listmngr's behaviour instead, and
prints the list at the end — seven deviations, below.

Closing the suite made the REST surface do what mailmanclient expects:
a domain without a description answers `null`; a new list's display name
is the list name capitalised (`Test-1`); `/lists/styles` carries the
styles with their descriptions and the default; `?advertised=true` narrows
a domain's lists; every `/lists/{id}/…` route takes the `fqdn_listname`
as well as the list id; `GET /members` lists every membership; a member
links its `user`; `DELETE …/roster/member` is the mass unsubscribe;
users are created from a form and answered with a `Location`, addressed
by id or email, patched with `cleartext_password`, and carry
`self_link`, `user_id` and `created_on`; addresses are added to a user
(created or adopted, `absorb_existing` to take one from another account),
listed with `self_link` and `verified`, made preferred
(`users/{id}/preferred_address`) and deleted; preferences and member
patches accept forms with `True`/`False`; `lists/find` finds a
subscriber's lists by role and host; `lists/{id}/{owner,moderator,
nonmember}/{email}` read and remove a role; a member's
`moderation_action` is patchable; a request carries `when`; the
autoresponse action is spelt `respond_and_continue` as Mailman spells it;
the system `hide_address` default is `true`; the two moderation rules hold
with Mailman's own reasons ("The message is not from a list member", "The
message comes from a moderated member"); pipelines, chains and the
configuration index take Mailman's shapes; a list's archivers are a
dictionary of switches, patchable; and deleting a list takes its held
messages and moderation log with it.

The gate also surfaced two robustness gaps, both closed: an internal error
(500) minted a correlation id but logged nothing — it now logs the cause
with the id; and on SQLite the runners' writes collided with the API's
("database is locked") — every pooled connection now enforces foreign
keys, waits five seconds for a lock and, for a file, uses write-ahead
logging, and every repository write transaction is `BEGIN IMMEDIATE`
(`Database::write_tx`), so a writer takes its reservation up front instead
of being refused mid-transaction.

Deviations the suite records: listmngr registers a user only when an
account is made (CLI, REST `create_user`, or web signup), so a bare
subscriber's member carries no `user` link and `client.users` lists
accounts, not subscribers — an account made later adopts the subscriber's
address; a bare `DELETE` on a member keeps the confirmation workflow, so
an administrative removal states `pre_confirmed` and `pre_approved`; the
password policy refuses the doctest's `somepass`; an account's first
address becomes its preferred address at creation (the web account mails
it); `user.password` is always `null` — the hash is never handed out; and
a new list's archivers start off rather than on.

Limits: the suite runs on SQLite (the gate's disposable server); no
doctest section for the digest resource exists in mailmanclient, so the
counters are checked through the raw connection; `absorb_existing` moves
the address and leaves the other account in place rather than merging
the accounts.

## Phase 5 acceptance (`P5-ACCEPTANCE`) — bounded local acceptance verified

The four acceptance criteria of Phase 5 in `docs/PLAN.md` §7, each with
its evidence:

1. **A 100 000-message mbox imports in under ten minutes** on a
   development machine — `import_100k_posts_in_under_ten_minutes`
   (`crates/cli/tests/archive_import.rs`, run by hand): 26.1 s for a
   77 MiB mbox, re-measured on the threading rule below (the numbers of
   the local run are in the ledger).
2. **Search p95 under 100 ms** — `search_p95_is_under_100ms_over_100k_posts`
   (`crates/archive/tests/search.rs`, run by hand): p95 8.4 ms over 200
   queries against 100 000 indexed posts (ledger `P5-SEARCH`).
3. **The Message-ID-Hash agrees with HyperKitty** for the same Message-ID —
   `crates/archive/tests/hyperkitty_parity.rs` against a real HyperKitty:
   every message of `mailman-users@mailman3.org` in March 2025 (138), the
   hash this archive computes was used to fetch HyperKitty's own record
   for the message, and the record's `message_id` and `message_id_hash`
   match in every case.
4. **Threading agrees with HyperKitty's import of the same messages** —
   the same fixture imported through `listmngr archive import`'s path:
   133 of 138 parents and 132 of 138 threads are the ones HyperKitty
   reports; the remaining 5 and 6 are replies to February, whose parent
   and root HyperKitty holds and a month imported alone cannot.

The fixture keeps only the reference headers (`Message-ID`, `In-Reply-To`,
`References`, `Date`) and HyperKitty's answers (hash, thread, parent) —
no body, name or address.

Criterion 4 found one real divergence and it is fixed here: a reply whose
referenced parent is archived nowhere used to be filed under the missing
parent's hash — a thread named after a post nobody holds — where
HyperKitty starts a new thread at the reply. The archive now resolves a
post's thread the way HyperKitty does, in the runner and in the importer
alike: the thread of its first archived ancestor (the provisional root,
else the parent), otherwise the post itself; and a parent arriving after
such replies takes them in with their subtrees, so out-of-order delivery
still ends in one thread.

Limits: the HyperKitty comparison covers one public list and one month
(138 messages) fetched on 2026-09-19, and its REST records, not a HyperKitty
database; a reply whose provisional root and parent are both absent but
whose `References` names an archived ancestor in between still starts its
own thread (HyperKitty walks the whole list); the two benchmarks measure
SQLite in the dev profile on one machine.

## Remote archivers (`P5-REMOTE-ARCHIVERS`) — bounded local acceptance verified

The three archivers a list can switch on in its settings now do something.
Each needs both the list's toggle and the server's configuration under
`[archive] archivers`; either one missing leaves the archiver off, and a
list whose `archive_policy` is `never` forwards nothing at all.

- **`mail-archive`** sends a copy of the archived post to
  `archivers.mail_archive_address` (for example `archive@mail-archive.com`)
  through the ordinary outbound queue. It is queued inside the archive's
  own transaction, so the copy and the archived post commit together or
  not at all, with an `archive.archiver` audit event. The service is
  public, so a list whose archive is `private` never reaches it however
  the toggle is set.
- **`mhonarc`** pipes the archived post to
  `archivers.mhonarc_command`. The command is an argument vector, not a
  shell line, so nothing in a message can become a shell word; the server
  substitutes `$listname`, `$hostname` and `$hash` in each argument and
  writes the message to the command's standard input.
- **`prototype`** drops the archived post into a maildir under
  `archivers.prototype_path`, at `<path>/<list id>/new/<hash>`, written
  into `tmp/` first and renamed, so a reader never sees half a message and
  a replay overwrites rather than duplicates.

`mhonarc` and `prototype` run after the post is stored, so a crash between
the two loses a forward and never the archived post; neither failure fails
the queue job, and both are logged. Every archiver forwards the copy the
archive itself published — cooked, with the list's headers — so an
anonymous list stays anonymous and a post hidden by `P5-ADMIN` is not
forwarded at all.

Limits: there is no per-list configuration of the command or the maildir
(they are the server's, one setting for every list); no retry or dead-letter
for `mhonarc` and `prototype` beyond the log, and no back-pressure if the
command is slow; `mail-archive` sends the post without checking whether the
service accepted it, and its permalink is not shown on the archive pages;
there is no command-line way to replay a post to the archivers, only
`listmngr archive export` and a manual feed.

## Archive administration (`P5-ADMIN`) — bounded local acceptance verified

A list owner now administers the archive from `…/archive/admin`, linked
from every archive page for an owner and for nobody else.

- **Categories**: the page lists the list's categories with the threads
  filed under each, and adds, renames and removes them. A name takes the
  same shape as a tag — lowercase letters, digits and hyphens, at most
  sixty characters — so it needs no escaping in the category page's
  address; a name that normalises to nothing, one the list already has,
  and a rename or removal of one it lacks are refused inline. Renaming
  carries the filed threads over; removing unfiles them and leaves the
  posts alone. This is the page `P4-LIST-SETTINGS` and `P5-INTERACTIONS`
  said was still missing.
- **Hiding**: "Hide this post" on every post and "Hide this thread" on
  every thread take it off every reading surface at once — the archive
  pages, the thread lists and the overview, the sender pages, the search
  (index and database), the Atom and RSS feeds, the REST archive reads and
  the mbox export and download — while the row, the stored bytes and the
  post's place in the thread survive. The administration page is the only
  surface that still names a hidden post, and only to show it again or
  delete it. A reply whose parent is hidden no longer offers an "In reply
  to" link to it.
- **Deleting**: "Delete this post" removes the post with its attachments
  and votes, splices its replies onto its parent, and, when the thread's
  root goes, makes the oldest surviving post the new root — the thread's
  tags, category, favourites and unread marks follow it. "Delete this
  thread" removes every post of the thread and every mark on it. Deletion
  cannot be undone, so the owner lands on the thread list afterwards.
- **Reattaching** a post under another was already the owner's (from
  `P5-RENDER`) and is unchanged.

Every change is refused to anyone but an owner of the list, is checked
against the live session inside the transaction that makes it, and commits
with its audit event in that one transaction: `archive.hide`,
`archive.unhide`, `archive.delete` and `archive.category`.

Limits: hiding is per post or per thread, with no reason recorded and no
notice to the author; a hidden post is hidden from the owner's reading
pages too (the administration page is where they see it); deleting a post
does not remove it from the search index until `listmngr archive reindex`,
though a deleted or hidden post can never be shown because every hit is
read back under the archive's policy; there is no command-line equivalent
of these actions yet; a thread deletion takes at most five thousand posts.

## Archive import and export (`P5-MBOX`) — bounded local acceptance verified

`listmngr archive import <list> <file>` reads an mbox in `mboxrd` form (a
`.gz` file is decompressed on the way in) and stores every message in the
list's archive — five hundred a transaction by default (`--batch`) — with
the same Message-ID-Hash, threading, sender, date and attachments a
message arriving by mail is stored with. A post already archived under
the same hash is skipped, so importing the same file twice stores nothing
the second time; bytes that will not parse as a message are skipped and
counted; a message without a `Message-ID` is given one derived from its
own bytes, written into the stored copy so that its hash and its headers
agree. The list must keep an archive: `archive_policy = never` and an
unknown list are both refused before a byte of the file is read. Every
batch commits its rows and one `archive.import` audit event together. The
importer writes the database only — run `listmngr archive reindex`
afterwards to put the imported posts into the search index.

`listmngr archive export <list>` writes an archive back out as `mboxrd`:
everything, one thread (`--thread <hash>`) or one month (`--month
YYYY-MM`), to a file (`--output`) or to standard output, gzipped with
`--gzip`. The browser has the same export at `…/archive/export.mbox` and
`…/archive/export.mbox.gz` (`?thread=`, `?month=`), linked from the
archive navigation, from a thread's "About this thread" and from a
month's thread list. It reads a page of messages at a time and streams
them, so a large archive never sits in memory, and the browser export is
authorized once, before the first page, under the list's archive policy
for the session: a public archive opens to anyone, a private one to a
verified member (a visitor gets 403), and `never` is 404. Both exports
write the archive's published copy — the same projection the archive
pages apply — so an anonymous list's authors stay hidden in the file.

Performance: the manual benchmark `import_100k_posts_in_under_ten_minutes`
imports a hundred thousand synthetic posts through the real binary and
asserts it finishes in under ten minutes; the numbers of the local run
are in the ledger.

Limits: the command line applies no archive policy on export (it is the
operator's tool; the browser export authorizes); an imported post is not
searchable until a reindex; the page's older `?format=mbox` download (the
twenty messages of the page) is unchanged and separate; a selection is a
whole archive, one thread or one calendar month — nothing narrower; the
`From ` separator carries a fixed epoch date, as Mailman's exports do;
an interrupted import resumes only in the sense that the messages already
stored are skipped on the next run.

## Posting from the web (`P5-WEB-POST`) — bounded local acceptance verified

A signed-in reader who holds a verified address subscribed to the list
(as a member, owner or moderator) can post from the archive: "New thread"
in every archive page's navigation, and "Reply" on every post (the reply
form quotes the parent and proposes `Re: …` without the list's prefix).
The server composes the message itself — the member's address in `From`,
the list's posting address in `To`, a fresh `Message-ID`, `In-Reply-To`
and `References` for a reply, `User-Agent: listmngr-web`, a UTF-8 text
body — and injects it into the `in` queue exactly as a message arriving by
LMTP or the REST `queues/in` injection, with a context that records the
web origin (the account and the address).

The `in` runner admits a web post the way it admits an `Approved:` post —
past emergency moderation and the deferred checks (explicit destination,
size, recipients, subject, header rules) — but only when the envelope
sender is the recorded verified address, the address is not banned, and
the member would post unmoderated anyway (their own moderation action, or
the list's default, is `defer` or `accept`). A moderated member's web post
is held like their mail. From there the post takes the ordinary path: the
pipeline, delivery to the members, the archive.

Refusals: a visitor gets 401; a signed-in reader without a verified
subscribed address, or whose address is banned, 403; a list whose archive
is `never` has no form (404); an empty or over-long subject or body, or a
subject with line breaks, is refused inline. The form alone takes a
96 KiB request (every other browser form is held to 8 KiB); the handler
refuses bodies over 64 KiB and subjects over 200 characters.

Limits: no attachments from the web; no draft or preview; posting needs
the archive to be readable by the poster (the form lives under the archive
pages); no per-user posting rate limit beyond the list's own moderation.

## Archive interactions (`P5-INTERACTIONS`) — bounded local acceptance verified

Signed-in readers now act on the archive, each action a plain form on the
page it concerns, applied under the list's archive policy (any signed-in
reader of a public archive; a verified member of a private one):

- **Votes**: every post shows its score (up votes minus down votes). A
  signed-in reader votes `+1` or `-1` once per post and takes the vote back
  by pressing the same button again; the pressed state is announced
  (`aria-pressed`). Audited as `archive.vote`.
- **Tags**: a thread page lists its tags, each a link to the threads tagged
  the same way (`…/archive/tags/<tag>`). Any signed-in reader adds a tag
  (normalised to lowercase letters, digits and hyphens, one to forty
  characters); the tagger or a list owner removes it. Thread lists show
  tags. Audited as `archive.tag`.
- **Categories**: a list owner files a thread under one of the list's
  categories (rows of `archive_categories`, managed on the archive's
  administration page) or under none. Thread pages and lists show the
  category as a link to `…/archive/categories/<name>`. Audited as
  `archive.category`.
- **Favourites**: a signed-in reader keeps threads as favourites and sees
  them at `…/archive/favorites` (linked from every archive page when signed
  in). Favourites are the reader's own bookmarks, like the last-view marks,
  and carry no audit event.

Limits: the categories themselves are managed on the archive's
administration page (`P5-ADMIN`); votes are not summed into thread lists
or the overview; tags and categories are per list and do not cross lists.

## Archive browsing (`P5-UI`) — bounded local acceptance verified

Every archive page now carries links to the others: an overview, the thread
lists, the recent-posts page, and the Atom and RSS feeds, all under
`/web/lists/<id>/archive/…` and all behind the list's archive policy
(public, private to verified members, or not there at all).

- `…/overview`: posts, threads and participants counted; the months with
  posts (each a link to that month's threads); the ten threads with the
  latest posts; the ten most active threads and the ten senders who posted
  most in the last thirty days.
- `…/threads` and `…/threads/<year>/<month>`: threads twenty a page — the
  root's subject linking to the thread page, posts, participants, the last
  post's date and sender. A signed-in reader sees a `new` badge on every
  thread with posts newer than their last visit to it; opening the thread
  page records the visit. A visitor sees no badges.
- `…/thread/<hash>`: one thread as a tree at a canonical address (absent
  threads are 404; `?thread=` still works).
- `…/senders/<digest>`: one sender's posts, twenty a page, with a count.
  Each post's sender name links here. The digest is SHA-256 of the
  lowercased address, so a link never carries the address; the page shows
  it as the reader may see it (obfuscated for a visitor).
- The search page, when the index answers, says how many posts match ("N
  results for …") and marks every matched word in the subject and the body
  with `<mark>`. Marking runs over the already-safe HTML and never touches
  a tag or a character reference.
- `…/feed.atom` and `…/feed.rss`: the twenty latest posts with subject,
  sender name (never an address), a 500-character text summary with
  addresses obfuscated, and absolute links built from `site.base_url`. A
  private list's feed needs the browser session, so a feed reader gets 403.

Limits: the months are bucketed in Rust from the post dates (capped at
200 000 posts per overview); "unread" is per list and per thread, not per
post; the feeds carry no full body.

## Archive search (`P5-SEARCH`) — bounded local acceptance verified

The archive now has a search index (tantivy 0.26) under `[archive]
index_path` (default `data/index`), one document per archived post: list,
hash, thread, subject, body, sender (name and address) and date. A search on
`/web/lists/<id>/archive?q=` uses the index when it exists — every word must
match in the subject, body or sender; hits rank by relevance and page twenty
at a time; each hit is then read back through the archive's own policy, so a
post the reader may not see is never shown — and falls back to the substring
search over the database until then.

The archive runner keeps the index current while `serve` runs: every post it
archives is added, and changes commit in batches (a hundred changes or two
seconds, whichever first; always on shutdown). `listmngr archive reindex`
rebuilds the index from every archived post (`--index <dir>` overrides the
configured path) and prints the count; it needs the index's write lock, so
run it while `serve` is stopped. The index directory is created on first
use; when it cannot be opened, `serve` logs a warning and search falls back
to the database.

Performance: the manual benchmark `search_p95_is_under_100ms_over_100k_posts`
indexes 100 000 synthetic posts and asserts p95 < 100 ms over 200 queries
(the numbers of the local run are in the ledger).

Limits: no facets (the index carries thread and date filters, offered to
callers of `search::Query`, not yet on the page); the REST archive resource
keeps its substring search; a post removed from the archive is not removed from the index until a
reindex (no removal path exists yet).

## Archive rendering (`P5-RENDER`) — bounded local acceptance verified

The archive now indexes, for each post, the sender's name and address and
the post's date as the cooked copy shows them, the post it replies to
(`In-Reply-To`, else the last `References` entry), and its attachments,
stored once at indexing time.

- **Threads** (`/web/lists/<id>/archive?thread=<hash>`) lay out as a tree:
  replies under their parents, siblings by date, a reply whose parent never
  arrived at the top of its thread, up to 500 posts. Each post links its
  parent ("In reply to").
- **Bodies** render through the archive's own writer: text posts as
  escaped paragraphs with URLs linked and runs of quoted lines folded away
  ("N quoted lines"); lists with `archive_rendering_mode = markdown` get a
  fixed safe subset of Markdown (headings, emphasis, code, lists, quotes,
  tables, `http(s)`/`mailto` links) — raw HTML is shown as text, other link
  schemes drop, images never load.
- **Addresses** in the sender line and the body are obfuscated for visitors
  (`local at domain`) and shown to signed-in readers.
- **Attachments** download from `/web/lists/<id>/archive/attachments/<hash>/<n>`
  with their stored name and type, `nosniff` and an attachment disposition;
  HTML, SVG, XML and script types are served as bytes.
- **Reattach**: a list owner moves a post, with its replies, under another
  post or makes it a thread root from the post's page; a post cannot become
  its own ancestor.
- **Avatars** are off by default; with `[archive] gravatar = true` the page
  shows each sender's Gravatar fetched through `/web/gravatar/<sha256>` by
  this server (cached an hour, `img-src 'self'` only on archive pages), never
  by the browser from gravatar.com.

Limits: posts indexed before this change show no sender or date and keep
the MIME-projected attachment links; the thread tree is bounded to 500
posts and has no paging; quoted-run folding is by `>` prefix only, with no
signature folding; the threading snapshot compares against the HyperKitty
algorithm as documented, not against a HyperKitty run.

## Phase 4 acceptance (`P4-ACCEPTANCE`) — bounded local acceptance verified

Two Chromium runs gate the browser interface in CI (`browser` job):

- `scripts/test-webui-browser.py`, the slice-by-slice harness that every
  Phase 4 package extended, with axe-core on every scanned page and the CSP
  and origin checks.
- `scripts/test-webui-journey.py`, the acceptance journey at a phone
  viewport (390×844): one person signs up, verifies the mailbox with the
  token the harness reads from the mail, signs in, creates a list (seated as
  a domain owner by the harness once verified), subscribes an address through
  the public list page and confirms it, sees the first post held (the harness
  holds it as the mail path would), accepts it from the moderation page,
  changes a setting and signs out. Every stop is checked for horizontal
  overflow and scanned with axe-core; Lighthouse audits the directory, the
  login page, the list page and a signed-in settings page for an
  accessibility score of at least 95; the harness then checks the database
  for what the browser claimed.

Run locally as `README` describes under "Other browser verification", with
`WEBUI_JOURNEY_SCRIPT`, and `WEBUI_LIGHTHOUSE` pointing at a
`lighthouse@13.4.1` executable (`npm install --prefix <dir> lighthouse@13.4.1`).

Limits: "post" is a held message the harness inserts, not a message through
LMTP (the mail path has its own end-to-end harness in
`crates/cli/tests/mailpath_e2e.rs`); Lighthouse scores the four pages named,
not every page; the journey runs on Chromium only.

## Data export and erasure (`P4-GDPR`) — bounded local acceptance verified

A signed-in reader downloads everything stored about them from
`/web/account/export.json` (linked from the account page): the account and
its preferences, every address with its preferences, every membership with
its member-level preferences, API token metadata (never the secret), the
domains they own, how many browser sessions are open, and the audit events
they appear in (the newest thousand). No password hash, token secret or
session token is ever included.

A server owner downloads the same for any account from the account's page
(`/web/admin/users/<id>/export.json`) and erases an account there by typing
one of its addresses back: memberships, addresses, API tokens, credential,
sessions and domain ownerships go in one audited transaction, as with
self-service deletion; moderation history keeps its rows without the link;
the last server owner with a verified address cannot be erased.

The command line has the same two operations: `listmngr user export <id>`
prints the JSON, `listmngr user erase <id>` erases and reports what went.

Limits: the export is a snapshot of listmngr's own tables (archived posts the
person wrote stay in the archive, as in Mailman); erasure does not anonymise
archived posts or bounce events keyed by address; no export of a list.

## Cross-list moderation (`P4-MODERATION-CROSS`) — bounded local acceptance verified

`/web/moderation` is now one queue. Under the per-list counts it carries every
held post across the lists the reader moderates (oldest first per list, up to
fifty) with the same preview and decision form as the list queue — decision,
comment, forward, the sender's posting policy, ban, header-rule shortcut —
and every undecided subscription request with its decision form, each naming
its list with a link to that list's queue. A decision made here returns here
(`?done=1`, `?saved=1`); the same forms on a list's queue still return to that
queue. The keyboard shortcuts work on this page too.

Limits: no bulk decision across lists (the list queue keeps its bulk form);
the page shows the first page of each list's queue, capped at fifty posts
and fifty requests; sender and ban actions from a post return to that list's
queue.

## System page and audit log (`P4-SYSTEM`) — bounded local acceptance verified

`/web/admin/system`, for server owners, shows the software and REST API
versions and the database backend; every runner queue with its jobs by state
(ready, leased, done, shunted), how long the oldest ready job has waited past
its due time, and the runners currently holding leases with their job counts;
the MTA map status (the configured MTA, map directory and LMTP transport, and
the generation the `current` link names, or a note that none is published, or
that no incoming MTA is configured); and the configuration as loaded with
secrets redacted, the same view as `listmngr conf`, as dotted keys.

`/web/admin/system/audit` lists audit events newest first — when, the actor
(a user id, a token, or the system), the peer address, the action, the target
and the recorded details — filtered by an action prefix and a target
substring, twenty per page with the filters kept on the page links.

Limits: the page is a snapshot without refresh; runner status is inferred
from leased jobs (an idle runner is invisible); no job actions here (the CLI's
`queue unshunt` and the REST `/queues` remain the tools); the audit viewer
shows ids, not names, and does not export.

## Domains and accounts (`P4-DOMAINS-USERS`) — bounded local acceptance verified

Two site-wide pages for server owners, linked from the administration index.

**Domains** (`/web/admin/domains`) lists every domain with its description,
alias domain, owners and list count, and adds one from a form (mail host,
description, optional alias; a bad host or a taken one is refused inline with
the other values kept). A domain's page (`/web/admin/domains/<host>`) shows
its facts, seats an owner by an address that belongs to an account and removes
one, lists every template name with the languages the domain overrides and
opens the same editor as a list's (preview, save, remove; a domain body
applies to every list on the domain that stores none of its own), shows the
DKIM DNS TXT record of each signing key configured for the host (derived from
the private key file, never the private half), and deletes the domain once
the host is typed back and no list remains.

**Accounts** (`/web/admin/users`) searches accounts by display name or any
address and pages with the search. An account's page
(`/web/admin/users/<id>`) saves the display name and the server-owner flag in
one audited write (the last server owner with a verified address cannot be
demoted), marks any of its addresses verified or unverified without a mailbox
proof, and lists its memberships with a link to each member's options.

Limits: no domain edit after creation (description and alias are fixed, as on
REST); DKIM keys are configuration, so the page shows their records and
generates none; the account page does not delete or merge accounts
(`P4-GDPR`), reset passwords or manage second factors.

## List creation and the directory (`P4-LIST-CREATE-INDEX`) — bounded local acceptance verified

The directory at `/web` now searches (`?q=`, a case-insensitive substring of
the id, display name or description) and filters by domain (`?domain=`). A
visitor sees advertised lists only. A signed-in reader sees their role on each
list (owner, moderator, member) and can tick **Also show my lists that are not
in the public directory** (`?show=all`), which adds the unadvertised lists they
have a role on — every list, for a server owner — each marked *not in the
public directory*. Paging keeps the filters.

**Creating a list** (`/web/lists/new`, linked from the directory and the
administration index for readers who may use it) is open to a server owner
and to the owners of a domain, on exactly the domains they may create on. The
form asks for the list name (the part before `@`), the domain, a display name,
the first owner's address (the reader's own is offered), the style
(`legacy-default`, `legacy-announce`, `private-default`), whether the list is
shown in the directory, and a description. A refused value comes back inline
on its field with the other values kept (a bad name, a taken id, an unknown
style or owner mailbox); a domain the reader does not own is a 403. The list
row, its `advertised` and `description` through the ordinary settings
validator, its first owner (the address created when unknown) and the
`list.create`, `list.update` and `member.create` audit events commit in one
transaction; the MTA maps are regenerated afterwards; the creator lands on the
new list's settings.

**The list summary** (`/web/lists/<id>`) names the posting address, the
owners' address, the domain, the archive policy and the subscription policy
above the subscribe form, shows the reader's role and a link to the settings
when they administer the list, and is now a page for the people with a role on
an unadvertised list (and for server owners) rather than a 404 for everyone.

Limits: no per-list "who may create" policy beyond server and domain owners;
the owner seated at creation is one address, added unverified as on REST; the
directory filters are the search, the domain and the scope (no sorting, no
role filter); the summary shows policies as their configuration words.

## Moderator queues (`P4-HELD-QUEUE`) — bounded local acceptance verified

`/web/moderation` now says what waits on each list a moderator can act on
(held posts and subscription requests) and links both queues.

**Held posts** (`/web/lists/<id>/held`) show each post with its decoded
`From`, `To` and `Date`, the first text body rendered as text, the number of
attachments, and the raw source (first 64 KiB) folded away. Each post has its
own decision form — keep held, accept, reject, discard — with a comment that
a rejection sends to the author as the reason, and an optional **Forward a
copy to** address: the original, wrapped as `message/rfc822`, goes there
with any decision, exactly as `forward_to` on `POST /api/v1/lists/<id>/held/<id>`;
a forward to one of the list's own addresses is refused inline. The bulk
form at the top applies one decision to the ticked posts in one transaction,
skipping any post decided meanwhile, and reports "N decided, M skipped".
Under each post, the **Sender** section shows the sender's standing on the
list (role and posting policy, or not on the list), sets a posting policy for
the sender — the member's own override, or a nonmember row created for the
purpose, as Mailman's "moderate sender" does — bans the sender on the list,
and links to the header rules with a `From` rule for that address prefilled.
The page loads a first-party `/web/moderation.js` for keyboard shortcuts
(`j`/`k` move, `a`/`r`/`d`/`h` choose, `s` submits, `?` shows the list); every
form works without it.

**Subscription requests** (`/web/lists/<id>/requests`) list the requests
waiting for a moderator and those still waiting for the address to confirm,
with join or leave, the name given and the time. A moderator accepts,
rejects (with a reason recorded in the audit event), discards or keeps a
request; accepting one that still waits for the address makes that
confirmation unnecessary, as on `POST /api/v1/lists/<id>/requests/<id>`.

Every page needs a live session of a list owner, moderator or server owner
and, under `security.require_2fa_for`, an enrolled second factor.

## Member management (`P4-MEMBERS`) — bounded local acceptance verified

`/web/lists/<id>/members` is now the owner's roster of every role: tabs for
members, owners, moderators and nonmembers (`?role=`), the email-substring
search and paging as before, and per row the display name, the member-level
delivery mode and status when one is set, the bounce score when it is above
zero, the inline posting-policy form (members and nonmembers) and a link to
the member's options. The roster is the first page to load the vendored
htmx: the search form carries `hx-get`, so a search swaps only the roster and
pushes the URL, and the same request without the `HX-Request` header — or
with scripts off — returns the whole page; the page's CSP allows
`script-src 'self'` only and htmx's own indicator stylesheet is switched off
so nothing inline is ever needed.

- **Options** (`…/members/<member>`): posting policy, display name, role
  (moving the subscription to another roster), and the member-level
  preferences — delivery mode and status, acknowledgements, hidden address,
  list copy, own posts, language — each with **Inherit** for "not set here",
  saved in one audited `member.update` transaction; the effective values,
  every layer resolved, are shown beside the form. The bounce section shows
  the score and last bounce, and **Reset bounce score and enable delivery**
  clears them and turns delivery back on when bounces had disabled it
  (`bounce.recover`). **Remove from the list** ends the subscription.
- **Add members** (`…/members/subscribe`): addresses one per line, as
  `address` or `Name <address>`, from the textarea or an uploaded plain-text
  file (256 KiB, added to the lines), a role, and Mailman's flags —
  `pre_verified`, `pre_confirmed`, `pre_approved`, `invitation`. Members go
  through the registrar's own workflow, so the list's subscription policy
  decides what is still missing exactly as on `POST /api/v1/members`; owners,
  moderators and nonmembers are added directly. The page reports each
  address: subscribed, held (a request waits for the person or a moderator),
  already on the roster, not an address, listed twice, or refused with the
  reason (a ban, for instance). At most 1000 addresses per submission.
- **Removal**: tick rows on the roster or paste addresses, then
  **Remove selected**; every removal is its own `member.delete` event in one
  transaction, a goodbye goes out when the list sends one, and ids that are
  not on this list are ignored.
- **Export** (`…/members/export.csv?role=`): a CSV attachment with email,
  display name, role, subscription mode, delivery mode and status, posting
  policy, bounce score, last bounce and subscription time, at most 10,000
  rows, RFC 4180 quoting.

Every page needs a live session of a list owner or server owner and, under
`security.require_2fa_for`, an enrolled second factor.

## List settings (`P4-LIST-SETTINGS`) — bounded local acceptance verified

Every list setting Mailman's REST configuration accepts is now editable from
the browser, in the nine groups Postorius uses, at
`/web/lists/<id>/settings/<group>`: **identity** (with the derived addresses
and counters shown read-only), **responses**, **messages** (alter messages),
**dmarc**, **digest**, **acceptance**, **archiving**, **members** (member
policy) and **bounces**. The older essentials form at `/web/lists/<id>/settings`
stays, with the group navigation above it. Each group is one form on the same
patch engine and validator as `PUT/PATCH /api/v1/lists/<id>/config`, so the
browser cannot store what the API would refuse; a save is one transaction that
rechecks the owner's session and authority, locks the list row and records the
`list.config` audit event with the patch. **Preview changes** runs the validator
on a copy and shows a before/after table of exactly what a save would change —
nothing is written — and **Save** applies the form. A refused value comes back
inline, on its field, with the submitted values kept and a 400 status; a form
rendered this way is the one client error whose body is not replaced by the
generic failure page. Free-text lists (MIME types, extensions, nonmember
addresses, DMARC addresses, aliases) are one entry per line.

Beyond the groups, the same navigation reaches:

- **Header rules** (`…/settings/header-matches`): add, edit in place, move up
  or down, remove; each edit rewrites the ordered set in one audited
  transaction, an invalid pattern is refused inline, and a **Test a header
  value** form says which stored rule a value would trip and what it would
  do, without writing anything.
- **Bans** (`…/settings/bans`): ban an address (normalized) or a `^` regular
  expression, lift one, paged twenty at a time. Server owners manage
  site-wide bans at `/web/admin/bans`.
- **Templates** (`…/settings/templates`): the catalogue of every `list:*`
  template with the languages this list stores its own text in; an editor per
  template and language that shows the text in effect and its source
  (list, domain, site or built-in), a **Preview** that expands the
  placeholders with sample values, the placeholder reference, **Save text**
  (`template.set`) and removal of the list's own text (`template.delete`).
- **Digest** actions: **Send the digest now** flushes what has accumulated and
  **Start a new volume** bumps the volume (`digest.bump`), from the digest
  group.
- **Archivers**: the archiving group records the remote archivers
  (`mail-archive`, `mhonarc`, `prototype`) for the list, which take effect
  when `P5-REMOTE-ARCHIVERS` ships; the local archive follows the archive
  policy.
- **Delete list** (`…/settings/delete`): the consequences are listed with the
  current archive policy; the owner types the list id back, and the deletion
  runs the same list-owned graph removal as `DELETE /api/v1/lists/<id>`,
  attributed to the owner (`list.delete`).

Every page needs a live session of a list owner or server owner (a moderator
is refused) and, under `security.require_2fa_for`, an enrolled second factor.
Vietnamese strings ship for every label, help text and refusal.

## Aggregate lease authority — bounded acceptance verified

Digest collection now retains the persisted queue deadline before ACK clears it
and checks expiry after the ACK audit, immediately before commit. Focused SQLite
and owned PostgreSQL regressions cover expired, valid and renewed authority,
including empty recipients. Leased workflow completion and moderation hold now
retain the same authority through their final audits; expiry rolls back staged
membership/token/notices or held records along with queue/audit changes.
Frozen aggregate gates passed 25/25: workspace615/0/48, mandatory PostgreSQL31/0/0,
40 new observed PostgreSQL audit waits and 390 stable source/harness paths.
Fresh-binary digest collection/publication/restart/SMTP passed on both backends.
This does not enable authenticated incoming DSNs or establish Mailman replacement
readiness. See `docs/DELIVERY_AUTHORITY.md` for the precise scope and evidence.

## Recipient-isolated outbound SMTP — producer prerequisite

`mta.smtp_single_recipient = true` opts non-null list mail into one recipient per
SMTP session/transaction. Default `false` retains batching; private null-sender
notices retain their existing batch envelope. This is an implemented transport
prerequisite for a future per-recipient bounce carrier, **not an authenticated
DSN carrier**: no token is issued, no incoming route or automatic DSN scorer is
enabled. Connection/TLS/AUTH cost increases per recipient. Cancellation before
final durable completion conservatively quarantines the whole reserved set,
including recipients not yet attempted. Bounded local acceptance passed 23/23
gates: workspace604/0/43, mandatory PostgreSQL26/0/0, and three actual
API→LMTP→SMTP/restart modes per database. Exact evidence and remaining
issuance-authority scope: [DSN_PRODUCER_PREREQUISITE.md](docs/DSN_PRODUCER_PREREQUISITE.md).

## Queue delivery authority after audit waits

Shared queue operations now recheck the current locked lease deadline after
their final audit/write and before commit. Expired operations roll back instead
of handing SMTP an expired reservation. Bounded acceptance passes all 23 gates:
workspace611/0/44, mandatory PostgreSQL27/0/0, with 378 stable source/harness paths.
Actual PostgreSQL audit-wait tests and both-backend SMTP/restart regressions pass.
This is not DSN issuance
or authentication. Scope and commands: [DELIVERY_AUTHORITY.md](docs/DELIVERY_AUTHORITY.md).

Archive completion now also fences after its own final ACK audit, retaining the
deadline read under the queue lock (including heartbeat renewal). This separate
producer increment passes deterministic SQLite rollback/valid/renewal controls
and eight observed ACK-audit waits on owned native PostgreSQL 14, including
`archive_policy=never`. Frozen parent acceptance passes 25/25 gates:
workspace612/0/45, mandatory PostgreSQL28/0/0, with 382 stable paths.
Actual API→LMTP→SMTP and restart cover public→never→public archive publication
and retained message bodies on both databases. Evidence:
`target/archive-authority-gates-20260910-034840/` and
`target/archive-final-audit/`. Digests/workflows/moderation remain follow-ups.

## Read-only DSN inspection — bounded local acceptance verified

`listmngr queue show <job-id> --dsn` explicitly displays untrusted recipient
Action/Status claims from bounded RFC3464 reports without changing queue/member
state or exposing diagnostics. It is not automatic bounce processing or sender
authentication. Corrected frozen gates passed 20/20 (workspace598/0/43;
PostgreSQL26/0/0; 372 stable paths), including a reproduced/repaired false-MIME-
boundary bug. See [DSN_INSPECTION.md](docs/DSN_INSPECTION.md).

## Unicode subject prefix — bounded local acceptance verified

Non-ASCII prefixes now produce ASCII RFC2047 Subject headers, preserving decoded
Vietnamese/emoji text and preventing duplicate prefixes on repeated composition.
MIME/body bytes and the existing ASCII-prefix path remain unchanged. Fresh frozen
gates passed 18/18: workspace 588/0/43, PostgreSQL 26/0/0, 367 stable hashes.
Native browser→LMTP→SMTP passed seven cases per backend with independent Python
decoding of received Unicode subjects. This is not general SMTPUTF8 support;
see [UNICODE_SUBJECT_PREFIX.md](docs/UNICODE_SUBJECT_PREFIX.md).

## Owner subject prefix — bounded local acceptance verified

**List administration → List settings → Subject prefix** now exposes the existing
prefix with an escaped, labelled text input. Leave it empty to clear; spaces and
Unicode are preserved verbatim. Legacy forms omitting the field preserve the
current value and omit it from the audit patch. The shared DB validator rejects
CR/LF atomically; this adapter does not repair historical invalid rows.

`P4-WEB-SUBJECT-PREFIX`: fresh `target/web-prefix-gates-20260909-143205/`
passed **17/17 gates**, workspace **582/0/43** (passed/failed/ignored), mandatory
PostgreSQL **26/0/0**, with **362 frozen source/harness hashes unchanged**.
Sequential POST then rendered-input RED→GREEN logs: `target/web-prefix-*-post.log`
and `target/web-prefix-*-input.log`. New independent SQLite/PostgreSQL router
controls cover exact values, omission/empty, CR/LF/duplicates, owner authority and
audit-failure rollback/retry. Chromium native save/reload/clear and escaped DOM
passed; screenshot: gate directory `browser/15-subject-prefix.png`.

Independent parent tracers additionally passed seven native browser → LMTP → SMTP
cases on each of SQLite and an owned disposable PostgreSQL cluster: changed and
empty prefixes, restart persistence, literal spaces and an unaffected sibling.
Exact ASCII Subject bytes, body bytes and delivery multiplicity were checked.
See [WEB_SUBJECT_PREFIX.md](docs/WEB_SUBJECT_PREFIX.md) for evidence and limits.
That snapshot proved Unicode storage, not SMTPUTF8 interoperability; the later
bounded RFC2047 increment above addresses prefix encoding. No new in-flight
revocation race, deployment/cutover or complete
Mailman replacement is claimed. The worker's original snapshot/delta is recorded
in `target/web-prefix-handoff.md`; parent closure adds evidence-only documentation.

## Subject-prefix configuration validation

List config writes now reject CR/LF in `subject_prefix` before persistence,
matching the existing mail composer's guard. This prevents saving a prefix that
would later fail mail preparation; it does not newly fix an emitted-header
injection. Unicode, spaces/tabs and an empty prefix remain unchanged. Invalid
patches preserve all list fields and audit; multiline description/info remain
supported. Historical invalid rows are not automatically rewritten.

`P1-SUBJECT-PREFIX-VALIDATION` passed the frozen 17-gate local run:
workspace579/0/42 and mandatory PostgreSQL25/0/0. See
[SUBJECT_PREFIX_VALIDATION.md](docs/SUBJECT_PREFIX_VALIDATION.md) for exact scope.

## Owner emergency moderation — bounded local acceptance verified

**List administration → List settings → Emergency moderation** lets an owner
hold otherwise eligible new posts for review, even when posting defaults accept
them. This is not a delivery shutdown: queued mail and explicit moderator
approvals can still be delivered. Turning it off does not release held posts.
Legacy omission, strict booleans, live authority and atomic audit are preserved.

`P4-WEB-EMERGENCY` passed 17/17 gates: workspace575/0/41, PostgreSQL24/0/0 and
both-backend browser→LMTP→held/SMTP, including sibling survival and restart.
See [WEB_EMERGENCY.md](docs/WEB_EMERGENCY.md). Full replacement remains incomplete.

## Owner welcome/goodbye controls — bounded local acceptance verified

List administration → List settings now exposes **Send welcome messages** and
**Send goodbye messages**. They affect future completed subscriptions/removals;
saving settings does not notify existing subscribers or recall queued mail.
Omitted fields in legacy forms retain their values. Existing live owner checks,
CSRF/Origin protection and atomic configuration/audit remain authoritative.

`P4-WEB-NOTICES` passed 17/17 gates: workspace574/0/40, mandatory PostgreSQL23/0/0,
and native browser→membership APIs→exact private SMTP on both databases, with
sibling-list and enabled/disabled restart controls. This closes another browser
administration gap, not the full replacement. See
[WEB_NOTICE_CONTROLS.md](docs/WEB_NOTICE_CONTROLS.md) for evidence and boundaries.

## Owner web posting limits — bounded local acceptance verified

List administration → List settings now lets owners configure the maximum
original message size in KiB and the visible To/Cc recipient hold threshold.
Zero disables the respective per-list check, not the server intake limit.
Oversized posts are held strictly above the size limit; recipient counts are
held at or above the threshold. Existing owner/session/CSRF/Origin checks and
atomic configuration audit apply. Legacy forms can omit either new field
without resetting its current value.

`P4-WEB-POSTING-LIMITS` passed 17/17 gates, workspace 571/0/40 and mandatory
PostgreSQL 23/0/0. Native Chromium → LMTP → held/SMTP verified 12 differential
cases per database, including equality boundaries, malformed recipients,
sibling-list survival and restart with checks enabled/disabled. Details,
commands and limitations: [WEB_POSTING_LIMITS.md](docs/WEB_POSTING_LIMITS.md).
This closes a browser administration gap, not the full Mailman replacement.

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


## Receive your own posts — bounded local acceptance verified

`P4-WEB-OWN-POSTINGS`: **My subscriptions** now includes a labelled Yes/No
select for receiving your own posts, showing the effective layered preference.
Saving writes an explicit membership override alongside delivery mode/status.
Legacy form clients may omit `receive_own_postings`; omission preserves the
stored override (including NULL/inheritance). Only literal `true`/`false` are
accepted when supplied. Existing verified ownership, session, CSRF/Origin and
restricted-delivery rules remain unchanged; this is not a recovery bypass.

The preference write and attributed audit remain atomic. Existing pipeline,
moderation and delivery semantics are reused: own-post comparison uses the
existing From/sender trust model, not newly authenticated authorship. No schema,
dependency or live deployment change. Two sequential router/UI RED→GREENs and
supplementary SQLite controls are recorded in `target/web-own-postings-handoff.md`.
Frozen `target/web-own-postings-parent-gates-20260909-090105/` passed **58/58 gates**:
workspace **561 passed / 0 failed / 37 ignored**, mandatory PostgreSQL **20/0/0**.
Actual Chromium→SMTP on both engines verified own-post suppression and restoration,
peer delivery, unaffected sibling membership and restart in both states, plus
denial controls, omission preservation and attributed audit multiplicity.
All 339 frozen source/harness paths remained stable; independent bounded review
found no causal P1/P2. Receipt: `target/web-own-postings-final-receipt.json`.
The documented `RUSTSEC-2023-0071` exception remains. This is not whole-P4 parity,
new contention/late-expiry coverage, fresh sender authentication or live cutover.

## Subscriber bounce recovery — bounded local acceptance verified

Subscribers with an existing verified account can now use **My subscriptions →
Restore delivery → confirm** for their own directly bounce-disabled membership.
The explicit action restores delivery and resets the bounce score/receipt and
warning cycle atomically with a user-attributed audit, retaining delivery mode
and historical events. It does not unlock other disable reasons or extend the
general preferences form's authority. The confirmation asks the user to check
their mailbox; no fresh mailbox challenge, probe or recovery email is sent.

Frozen `target/web-bounce-recovery-parent-gates-20260909-072736/` passed **54/54
gates**: workspace **558 passed / 0 failed / 36 ignored**, mandatory PostgreSQL
**19 passed / 0 failed**, and actual Chromium → recovery → scheduler → restart →
fresh SMTP delivery on each backend, with unauthorized and still-disabled controls.
Build/fmt/strict Clippy/client/browser/DKIM/TLS/AUTH/security passed with the
documented `RUSTSEC-2023-0071` exception. Frozen source/harness: 334/334 stable;
bounded independent review found no P1/P2. Receipt:
`target/web-bounce-recovery-final-receipt.json`. Acceptance prose was checked
separately after the frozen run.

Already-published warning jobs are not recalled. Recovery-specific lock-wait
coverage is SQLite, not a PostgreSQL contention matrix or an audit/COMMIT-expiry
guarantee. Token/probe recovery, incoming authentication/DSN/VERP, remaining
lifecycle/localization and migration/real-MTA cutover remain open. This supersedes
historical no-web-recovery wording only for this verified-session action, not full
P4 or whole Mailman replacement. No live deployment is enabled by this change.

## Opt-in automatic bounce maintenance — bounded local acceptance verified

`P3-BOUNCE-SCHEDULER` adds an explicitly enabled, supervised child to the real
`serve` mail role. Defaults remain off; enabling the mail role or a list's
`process_bounces` alone does **not** activate maintenance. This supersedes older
manual-only/no-scheduler statements solely for this opt-in path. No cron is installed.

Safe default TOML (no credentials; configure the existing mail transport separately):

```toml
[mta]
bounce_maintenance_enabled = false
bounce_maintenance_interval_secs = 60 # integer 1..86400
bounce_maintenance_batch_size = 100   # integer 1..1000
```

Equivalent environment defaults:

```sh
LISTMNGR__MTA__BOUNCE_MAINTENANCE_ENABLED=false
LISTMNGR__MTA__BOUNCE_MAINTENANCE_INTERVAL_SECS=60
LISTMNGR__MTA__BOUNCE_MAINTENANCE_BATCH_SIZE=100
```

Explicit activation requires `mta.enabled=true`, a separately reviewed supported
SMTP transport configuration, and `mta.bounce_maintenance_enabled=true`. Bounds
are validated even while disabled. Individual lists must still enable
`process_bounces`. **This publishes private warnings and can remove due Member
subscriptions; it is not a dry run.** Run migrations before serving.

The first SQL-bounded page waits a full interval; every completed/failed page
waits another full interval, without catch-up bursts or overlapping pages within
one instance. Scanned non-due/failed members advance the UUID cursor; an empty
page resets it for the next tick. Page-level failures retain the cursor and retry
later, logging only generic errors; success logs bounded summary counts, never
member IDs, raw database errors, DSNs or message content. Restart begins a fresh
pass. List day-based warning/removal intervals retain their existing semantics.

Shutdown cancels owned in-flight work. Earlier committed members remain committed;
transaction drop initiates rollback but is not whole-page rollback or certainty
about an in-flight commit acknowledgement. This is not DSN/VERP detection, probes,
automatic reenable, multi-instance scheduling leadership or production cutover.
Focused config/runner tests and gate details: `target/bounce-scheduler-handoff.md`.
Enable scheduling on one selected mail-role instance; this slice does not elect
a leader across replicas.

Frozen `target/bounce-scheduler-parent-gates-20260909-063657/` passed **50/50 gates**:
workspace **554 passed / 0 failed / 35 ignored**, mandatory PostgreSQL
**18 passed / 0 failed / 0 ignored**, and six real client→LMTP→SMTP→serve→restart
cases on each backend without CLI sweeps or SQL fixture mutations. Default-off,
batch-one progress, warning/removal/goodbye, positive-day grace, paused lists,
restored delivery, permanent warning rejection, exact durable-to-SMTP bytes,
audits, healthy survivors and graceful restarts passed. Build/fmt/strict Clippy,
browser/client/DKIM/TLS/AUTH/security passed with the documented
`RUSTSEC-2023-0071` exception. Source/harness remained stable; bounded independent
review found no P1/P2. Receipt: `target/bounce-scheduler-final-receipt.json`.
Acceptance prose is checked separately after the frozen run. Full replacement
remains open; this does not prove cancellation during a held SQL transaction
or rollback certainty after an unacknowledged commit.

## Explicit bounce maintenance — bounded local acceptance verified

`P3-BOUNCE-MAINTENANCE` adds the working operator command below. It **publishes
private warnings and removes due Member subscriptions**; it is not a dry run.
It does not install cron, start delivery workers, or add a scheduler to `serve`.
Run migrations before using the command. Use your normal protected configuration;
never put database credentials or secret material into shared command logs.

```sh
listmngr migrate
listmngr bounce sweep --limit 100
# Continue this pass with the returned UUID (not a literal placeholder):
listmngr bounce sweep --limit 100 --after "$NEXT_CURSOR"
```

The JSON result contains `scanned`, `warned`, `removed`, `failed`, `next_cursor`.
`--limit` defaults to 100 and accepts 1..1000. `--after` is an optional UUID.
The cursor is the **last scanned** eligible candidate, even if not due or failed;
continue until an empty page returns null. Start a later maintenance pass without
`--after`, so previously skipped/failed members can be revisited. One member gets
at most one action per invocation, not a catch-up burst. A nonzero failed count
makes the CLI exit nonzero after printing the bounded summary, without raw
per-member diagnostics. Prior successful members remain committed.

Migration 0025 adds `bounce_you_are_disabled_warnings` (default 3, integer 0..100),
`bounce_you_are_disabled_warnings_interval` (default 7 whole days, 0..36500), and
`bounce_notify_owner_on_removal` (default true). `process_bounces` remains false.
Native JSON uses numeric days; Mailman compatibility also accepts/returns `Nd`.
JSON/form/Python booleans, PATCH preservation, PUT default reset, attributes,
legacy defaults and generated OpenAPI are covered. Changing config never removes
members by itself; only an explicit sweep does so.

A current ordinary Member with its own effective `by_bounces` disablement and
`process_bounces=true` receives the first warning immediately. Later warnings
require the full interval. After the last warning, a full interval must pass
before removal; count zero removes immediately. Interval zero deliberately allows
one further action on each successive serialized invocation. Positive intervals
prevent repeated/concurrent same-time publication. A new scoring disable resets
both warning-cycle fields transactionally. Other disable reasons and explicit
reenables are preserved.

Warnings have subject `Membership disabled warning`, preserve subscriber
transport spelling, and give the real list-owner address as Reply-To and human
restoration contact—no unusable recovery token or URL. Removal can publish the
existing optional goodbye and, independently, the default-enabled
`Member removed by bounces` notice to deduplicated owners/moderators. The private
4096-byte MIME producer, `workflow_notices` provenance and null-sender/no-loop
consumer are reused. Counters mean **durable publication**, not guaranteed SMTP
delivery. Each member's mutation, notices and audits commit together; an unsafe
administrative roster rolls back that member's whole operation. Inherited admin
roster fanout is not an aggregate memory/job quota, and later roster edits do not
revoke already published recipient snapshots.

Frozen `target/bounce-maintenance-parent-gates-20260909-054944/` passed **46/46
gates**: workspace **546 passed / 0 failed / 35 ignored**, mandatory PostgreSQL
**18 passed / 0 failed**. Seven actual client→LMTP→SMTP→CLI→restart cases passed
on each engine, including positive default intervals, manual REST restoration,
warning rejection while membership exists, and goodbye/admin rejection without
bounce loops. Build/fmt/strict Clippy, browser/client/DKIM/TLS/AUTH and security
passed with the documented `RUSTSEC-2023-0071` exception. Source/harness stayed
stable; independent bounded static review found no P1/P2. Receipt:
`target/bounce-maintenance-final-receipt.json`. Acceptance wording is post-run
documentation checked separately; focused RED/GREEN provenance remains in
`target/bounce-maintenance-handoff.md`.
No incoming DSN/VERP/probe support, unattended scheduling,
localized templates, or web/email token recovery is claimed. Historical bounded
acceptance sections below describe their own earlier increments; their exclusions
of warnings/removal are superseded only by this explicit maintenance slice.

## Bounce increment notices — bounded local acceptance verified

`P3-BOUNCE-INCREMENT-NOTICE` adds the default-**false** scalar
`bounce_notify_owner_on_bounce_increment` (migration 0024, legacy JSON false).
`process_bounces` remains false. Native/compat config supports JSON/form,
Python `True`/`False`, PATCH preservation, PUT reset to false and attribute reads.
Each fresh eligible UTC-day direct RCPT observation can now publish private
owner/moderator notices, including a threshold transition; enabling both flags
sends both increment and disable notices. The increment body reports the effective
score **before** threshold reset to zero. Stale reset 1→1 or a decreasing score
still qualifies; same-day refresh, replay, out-of-order, disabled effective status,
configuration-only changes and nonmember/non-RCPT/internal notices do not.

The fixed subject is `Member bounce score increased`. MIME names only member,
list and score, never raw posts, SMTP diagnostics or secrets, capped at 4096 bytes
per deduplicated admin with original transport spelling. It reuses the private
`workflow_notices` producer and null-sender/no-loop consumer. All notices and
`bounce.increment_notice` audits commit with the fenced score/event/outcome and
disable transaction. Empty rosters audit count zero; false emits no audit/job.
Inherited limitations: unsafe rosters abort the transaction; recipient snapshots
are not revoked by later roster changes; no total-roster memory/job quota is added.
This direct-RCPT/no-VERP behavior follows Mailman 3.3.10's increment condition
(below threshold OR probes disabled), not new warning/removal/probe/incoming-DSN
support. Frozen run `target/bounce-increment-parent-gates-20260908-203556/`
passed all 42 gates: workspace 531 passed/0 failed/34 ignored; mandatory PostgreSQL
17/0/0; all nine actual mailmanclient→LMTP→SMTP→restart scenarios on both engines;
build/fmt/strict Clippy, browser/client/DKIM/TLS/AUTH and security gates (with the
documented `RUSTSEC-2023-0071` exception). Source and harness fingerprints remained
stable; independent bounded static review found no P1/P2. Receipt:
`target/bounce-increment-final-receipt.json`. Acceptance wording is a post-run
documentation-only change checked separately. Prior counts below are historical;
this does not complete warning/probe/recovery, authentication or MTA cutover.


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

Compatibility `/3.1/members` now accepts mailmanclient `add_owner()` and
`add_moderator()` without subscriber confirmation flags, under the unchanged
list-scoped `members:write` authorization. Role assignment does not verify the
address unless `pre_verified` is explicitly true. Native creation and ordinary
member/nonmember confirmation requirements remain unchanged; invitations remain
unsupported. Parent reproduced HTTP400→201 with a router regression, then passed
the real SQLite client→LMTP→SMTP→restart flow (six differential cases).

The final frozen candidate passed all 38 gates: workspace 524/0/33 and separate
PostgreSQL 16/0/0 (pass/fail/ignored), both-backend real mailmanclient→LMTP→SMTP→
restart, build/fmt/strict Clippy, browser/client/DKIM/TLS/AUTH and security checks
with the documented `RUSTSEC-2023-0071` exception. Source and harness hashes stayed
stable; independent bounded static review found no concrete P1/P2. Evidence:
`target/bounce-notice-parent-gates-20260908-195604/` and
`target/bounce-notice-final-receipt.json`. Acceptance wording was updated after
the run and checked separately. This is not whole Mailman parity or production
cutover. Older counts/no-notice statements below describe previous increments.


## Threshold-triggered bounce suspension — bounded local acceptance verified

The current development candidate extends `process_bounces` with
`bounce_score_threshold` (default 5, finite numeric value >0 and ≤1,000,000;
fractions supported). A new eligible UTC-day RCPT observation at or above the
threshold sets this Member's delivery status to `by_bounces` and resets score to
zero, atomically with event, audit and delivery completion. Same-day receipt
refresh and configuration changes alone do not trigger suspension. New posts
exclude the disabled subscriber; previously materialized recipients are not revoked.

The parent reproduced and fixed stale reset 1→1 threshold handling. The final
frozen candidate passed all 34 recorded gates: workspace 518/0/32 and mandatory
PostgreSQL 15/0/0, including two row-lock barrier regressions for preference races.
Actual mailmanclient→LMTP→SMTP→restart probes passed on both databases, preserving
healthy-survivor delivery. Build/fmt/strict Clippy, client/browser/TLS/AUTH/DKIM and
security passed with the documented RUSTSEC-2023-0071 exception. This is bounded
local acceptance, not full Mailman parity or production deployment approval.
No automatic re-enable/removal, warning scheduler, notification, VERP or inbound
DSN authentication is claimed. See `P3-BOUNCE-DISABLE` in the parity ledger.

## Opt-in direct SMTP bounce scoring

`process_bounces` defaults to false. Enabling it scores new permanent **RCPT**
failures for an existing subscriber, at most one point per UTC day. The default
stale interval is seven days (bounded 1–3650); a new eligible event resets an
expired score to one. Same-day events refresh the receipt timestamp without
adding a point; older timestamps cannot move it backwards. These are direct
configured-relay observations, not authenticated incoming DSNs or proof of an
invalid mailbox. EHLO, MAIL, DATA, transient failures, notices, owners/moderators
and unsubscribed recipients do not score. SMTP metadata remains available.

Both API prefixes expose persisted read-only member `bounce_score` and
`last_bounce_received`. Native config uses integer days; `/3.1` exposes whole-day
strings such as `7d` and accepts them in config/attribute writes. Actual
mailmanclient 3.3.5 `settings.save()` is exercised, including Python form booleans.
PATCH preserves omitted settings; PUT resets them. The previously accepted
scoring-only slice excluded historical replay, automatic disablement, warnings,
VERP and DSN trust; the suspension increment above has separate acceptance.

Scoring-only candidate acceptance passed: workspace **512 passed / 0 failed / 30 ignored**,
mandatory PostgreSQL **13 passed / 0 failed / 0 ignored**, and the real
HTTP/client→LMTP→SMTP→restart tracer on **both databases**. All 30 recorded gates
passed with unchanged source/harness fingerprints, including build/fmt/Clippy,
security (documented `RUSTSEC-2023-0071` exception), client/browser/TLS/AUTH/DKIM.
See `P3-DIRECT-BOUNCE-SCORE` for receipts; this is not full replacement readiness.

## Opt-in durable goodbye notices

List config now supports `send_goodbye_message` (default **false**) on both API
prefixes. PATCH `{"send_goodbye_message":true}` to enable; form booleans are also
supported. PATCH preserves omitted fields, PUT resets them, and older serialized
lists default this new field to false.

Actual Member removal by individual administration, mass unsubscribe/sync,
confirmed leave, authenticated browser leave or list teardown publishes one
private built-in goodbye per removed Member. Owners, moderators, nonmembers,
absent members and repeated operations do not generate a goodbye. Membership and
preference cleanup, notice bytes/recipient/provenance and audit commit together;
publication failure rolls the removal back. Confirmed workflows retain their
separate completion receipt. Notices retain the stored original mailbox spelling,
use the existing null-envelope outgoing path and survive process restart (including
delivery of already-published notices after list deletion).

The real `mailmanclient` administrative call
`mailing_list.unsubscribe(email, pre_confirmed=True, pre_approved=True)` now works
through `DELETE /3.1/lists/{id}/member/{email}` (also on `/api/v1`). This alias only
removes the Member role and requires both flags explicitly true; unsupported
confirmation/moderation combinations fail closed. Use the public leave workflow
for confirmation, not this administrative shortcut.

Final local acceptance passed: workspace **505 passed / 0 failed / 30 ignored**,
mandatory PostgreSQL **13 passed / 0 failed / 0 ignored**, and independent listening
HTTP/client → disabled mail spool → process restart → SMTP on **both databases**.
Build, fmt, strict Clippy, deny, fresh-source audit (documented
`RUSTSEC-2023-0071` exception), real TLS/AUTH, pinned client, Chromium and independent
DKIM gates passed on an unchanged candidate. Full receipts and prior failed-run
history are under `P3-GOODBYE` in `docs/FEATURE_PARITY.md`. Custom goodbye
templates, localization, per-removal overrides, deployed MTA certification and
complete Mailman replacement readiness are not claimed.

## SMTP AUTH PLAIN over REQUIRED TLS (bounded local acceptance verified)

Optional `mta.smtp_auth_username` and `mta.smtp_auth_password` enable only PLAIN;
both absent preserves no-auth delivery. Both credentials must be nonempty, at most
255 UTF-8 bytes each and contain no control characters. Authentication requires
`smtp_tls = "required"`, including disabled configurations; plaintext credentials
are rejected rather than silently ignored. Prefer `smtp_auth_password_file`
(or `LISTMNGR__MTA__SMTP_AUTH_PASSWORD_FILE`) instead of inline password. It is
mutually exclusive with the inline password, accepts one terminal LF/CRLF, and on
Unix requires a private regular file (e.g. mode 0600), not a symlink. Keep its
parent directory administrator-controlled and restart after credential rotation.

Only verified TLS authorizes fresh EHLO and exact post-TLS `AUTH PLAIN` capability
matching; pre-TLS advertisements and substring/greeting lookalikes do not count.
A single command-timeout budget covers this EHLO, AUTH initial response and at
most one empty 334 continuation. Only 235 authorizes the shared MAIL/RCPT/DATA
path. AUTH failures retain pending recipients with bounded retry and no mailbox
bounce. Debug/config dumps, AUTH errors and later authenticated SMTP reply text
are redacted, including hostile raw/base64 credential echoes. Typed envelope/DATA
status and uncertainty semantics remain intact.

If an initial response would exceed SMTP's 512-octet command limit, the client
instead sends bare `AUTH PLAIN`, waits for an empty 334, then sends the credential
response. A premature 235 before sending credentials is rejected; the supported
255-byte per-credential limit is unchanged.

`cargo test --locked -p listmngr-runners --lib smtp_auth` verifies the real
config→runner→owned SQLite→TCP/verified TLS path, exact synthetic credentials,
envelope/body and failure controls. See `P2-SMTP-AUTH` and
`deploy/starttls.example.toml`. Parent acceptance now includes an independent
OpenSSL real-process AUTH matrix, 495 workspace passes (30 ignored), 13 explicit
PostgreSQL passes, client/browser, independent DKIM and security gates. The ledger
records the timed-out orchestration and sequential continuation on unchanged
source. LOGIN/XOAUTH2, opportunistic/implicit TLS, live production-relay cutover
and full Mailman replacement remain outside this increment.

## Outgoing dependency recovery

Database failures while reading the durable message or resolving DKIM signing
authority now schedule a fenced retry with backoff before opening SMTP. Missing
or invalid authority and invalid signing input still quarantine the job; errors
never authorize unsigned fallback. Focused lookup-classification and durable
retry/shunt regressions passed after separate observed failures. This does not
guarantee recovery while the database remains unavailable or exactly-once SMTP.

## REQUIRED outbound STARTTLS (bounded local acceptance verified)

The outgoing runner now supports verified `mta.smtp_tls = "required"`:

```toml
[mta]
enabled = true
smtp_relay = "192.0.2.25:25" # documentation-only address; replace with relay IP:port
smtp_tls = "required"
smtp_tls_server_name = "relay.example.invalid" # certificate DNS identity, not EHLO
# smtp_tls_ca_file = "/etc/listmngr/relay-ca.pem" # optional additional private CA(s)
command_timeout_secs = 30
```

`smtp_relay` remains a numeric IPv4 `IP:port` or IPv6 `[IP]:port`; DNS dialing is
not added. Without `smtp_tls_server_name`, verification uses the relay IP and
requires a matching IP subjectAltName. Set the explicit DNS name for a relay
certificate issued to that name. Public trust uses bundled Mozilla/webpki roots;
the optional PEM CA file **adds** trust for a private relay, never disables
chain, validity or hostname verification. Restart the mail role after changing
CA files; keep the system clock correct. Root-store updates require a rebuilt
binary. Missing/empty/invalid CA files and invalid names fail role startup.

The client uses Rustls with the ring provider and TLS 1.2/1.3. It sends only EHLO
and STARTTLS before verified TLS, then repeats EHLO before MAIL/RCPT/DATA. One
`command_timeout_secs` budget covers greeting through TLS completion; TCP connect
has its own same-length bound. Missing/rejected STARTTLS, certificate/name errors
and negotiation timeout never fall back or create mailbox bounce events; they
retain pending recipients for bounded queue retry. Final DATA success is published
without waiting for QUIT; existing ambiguity, per-recipient and final-byte DKIM
semantics remain shared with explicit `plaintext_trusted_relay`.

No opportunistic TLS, implicit TLS, inbound LMTP TLS, certificate
revocation service, live MTA cutover or complete Mailman replacement is claimed.
The disabled default remains disabled; enabling opportunistic/unknown modes is
rejected. See `P2-STARTTLS` in [the evidence ledger](docs/FEATURE_PARITY.md) and
[deployment guidance](deploy/README.md). The combined candidate passed 486
workspace tests (30 environment-specific tests ignored), 13 explicit PostgreSQL
tests, client/browser/security gates and independent OpenSSL transport checks.
The evidence ledger records the orchestration timeout, repaired PostgreSQL
fixture preflight and sequential revalidation; older totals below are historical.


## Per-list visible recipient moderation

Authenticated list-config JSON or form PATCH/PUT accepts `max_num_recipients`
(integer 0..2147483647). Zero disables the check; omitted PATCH preserves the
value and omitted PUT restores zero. Both `/api/v1` and `/3.1` expose it.
Following Mailman's exact compatibility boundary, a post with a To/Cc mailbox
count **at or above** a nonzero limit is held for moderation, without publishing
delivery, archive or digest jobs. Repeated headers and group members count;
display-name/comment commas, Bcc, Reply-To and subscriber roster size do not.
When enabled, malformed or partially parsed visible headers are held rather
than bypassing the limit. This conservative admission is not full RFC mailbox
grammar or all Mailman message-acceptance parity. See `P2-RECIPIENT-LIMIT` in
`docs/FEATURE_PARITY.md` for verification status.

## Task runner and `notify` — bounded acceptance verified

The mail role now runs Mailman's task runner: every `mailman.run_tasks_every_secs`
(an hour) it sweeps what nothing else cleans up on its own schedule, each
step as bounded batches in their own audited transactions (`task.sweep`):

- confirmation tokens past their day that nobody answered (a request waiting
  for a moderator is never touched), bounce probes past their life, help
  cooldown rows older than an hour, and autoresponse records outside the
  list's `autoresponse_grace_period`;
- finished queue jobs acknowledged more than `mailman.finished_job_retention_secs`
  (7 days) ago — never one a still-valid DSN issuance could answer — with
  their recipient snapshots and notice provenance; disposed held messages
  and their moderation log after the same retention; then the messages and
  blobs no job, held row or issuance references any more. Shunted jobs and
  pending held posts keep their message however old it is;
- bounce scores whose last bounce is older than the list's
  `bounce_info_stale_after` are forgotten (`bounce.stale_reset`), as the
  next bounce would have started them over anyway.

```toml
[mailman]
run_tasks_every_secs = 3600          # the sweep's period on the mail role
finished_job_retention_secs = 604800 # how long finished jobs and their messages stay
```

`listmngr tasks run` sweeps once by hand and prints the JSON summary.
`listmngr notify [--list ID]... [--dry-run]` is Mailman's `notify`: every
list with held messages or subscription requests waiting for a moderator
sends its owners and moderators `list:admin:notice:pending` (`$count`, and a
`$data` block listing each section's first entries), audited as
`list.notify`; it is meant for a daily cron, not the sweep. See
`P3-TASK-RUNNER` in `docs/FEATURE_PARITY.md`.

## Digest settings, RFC 1153 and volume rollover — bounded acceptance verified

Mailman's Digest settings are list settings now: `digests_enabled` (off, the
`to-digest` handler collects nothing), `digest_size_threshold` (KiB of
pending posts that trigger an issue; `0` never), `digest_send_periodic`
(send what is pending once it is a day old) and `digest_volume_frequency`
(`yearly`, `monthly`, `quarterly`, `weekly`, `daily`: a new calendar period
since the last issue advances the volume and restarts the issue numbers at 1,
audited as `digest.bump`).

The plain-text issue follows RFC 1153 as Mailman writes it: the list's
`list:member:digest:masthead`, the `list:member:digest:header`, `Today's
Topics:` with each subject and author, each message under a numbered
`Message: N` block behind a line of thirty hyphens, the
`list:member:digest:footer` as `Subject: Digest Footer`, and `End of … Digest,
Vol X, Issue Y` with its underline (the exact layout is compared with
Mailman's own output under `P3-DIGEST-SNAPSHOT`, above). The MIME issue
keeps the posts whole and carries the same three templates as their own
text parts (empty header and footer templates are omitted).
`summary_digests` remains Mailman's MIME alias. Templates are resolved for
the list's language with `$volume` and `$issue` added to the usual
placeholders. See `P3-DIGEST-SETTINGS` in `docs/FEATURE_PARITY.md`.

Mailman's `/lists/{id}/digest` resource (`P3-DIGEST-REST`) is on both
prefixes: `GET` returns the list's `volume` and `next_digest_number`; `POST`
takes any of `bump` (advance the volume and restart numbering, audited as
`digest.bump` with the caller's token), `send` (publish whatever is collected
as one issue now) and `periodic` (publish only if the list's size or daily
trigger is due), applied in that order and answered with `202` and how many
issues were published. Booleans may be spelled `True`/`False` on forms.
Publication renders the issue and queues it for the outgoing runner; the
response does not mean SMTP delivery. Reads need `lists:read`, verbs
`lists:write`, within the token's list bounds. The renderer moved to
`listmngr_db::digests::render` so the CLI, the digest runner and REST share
it.

## Bounce processing — bounded acceptance verified

The mail role now runs Mailman's bounce runner over the `bounces` queue. A
report that reached `list-bounces@` names the member it concerns in one of
three ways, most trustworthy first:

1. a VERP bounce address (`list-bounces+local=domain@host`), decoded at
   intake;
2. a delivery-status report whose `Original-Envelope-Id` this server issued
   (`mta.dsn_issuance_enabled`), verified against the stored issuance — the
   report's own claims are then ignored;
3. the report's `Final-Recipient` lines with `Action: failed`;
4. for MTAs that write prose instead of a report, the heuristic detectors
   (`listmngr_mail::bounce`, after `flufl.bounce`): Postfix, qmail, Exim,
   Sendmail, Yahoo, Exchange, and a generic permanent-failure phrase
   matcher, with a delay/warning matcher that recognizes a message as
   temporary rather than a failure.

Each named member of a list with `process_bounces` is scored with exactly the
rules an SMTP-time failure uses (one point per day, threshold, disable, the
owner notices), and the report's job finishes in the same transaction.

At the threshold the list does what Mailman does: it sends the member a
**probe** from a one-time bounce address
(`list-bounces+probe=TOKEN@host`, routed by the same MTA maps as any VERP
bounce), resets the score, and disables delivery only when that probe
bounces — a bounce that names the token, verified against its stored hash,
inside `mailman.bounce_probe_lifetime_secs` (7 days). Everything else about
the probe token is inert: unknown, spent, expired, or for another list. Set
`mailman.bounce_probes = false` to disable at the threshold at once instead.

```toml
[mailman]
bounce_probes = true               # Mailman's behaviour; false disables at the threshold
bounce_probe_lifetime_secs = 604800
``` A
report that names nobody goes where `forward_unrecognized_bounces_to` says —
the list's owners and moderators, the site owner, or nowhere — as a sanitized
owner delivery with a null reverse path. Delays (`Action: delayed`) count as
recognized but change nothing. `/metrics` gains
`listmngr_bounces_total{result}`. See `P3-BOUNCE-RUNNER` in
`docs/FEATURE_PARITY.md`.

## Automatic responses — bounded acceptance verified

Mailman's Automatic Responses are list settings now: `autorespond_owner`,
`autorespond_postings` and `autorespond_requests` (each `none`, `respond` or
`respond_and_discard`), the matching `autoresponse_*_text` (the reply body,
with the usual `$listname`-style placeholders; empty means the built-in
text) and `autoresponse_grace_period` (days; `0` answers every message).

```sh
listmngr … # or PATCH /3.1/lists/dev.example.com/config
autorespond_owner=respond_and_discard&autoresponse_owner_text=Owners%20read%20mail%20weekly.&autoresponse_grace_period=30
```

A writer is answered at most once per address, kind and grace period; the
reply is `Auto-Submitted: auto-replied`, and automatic or null-sender mail
is never answered, so two responders cannot loop. `respond_and_discard`
swallows the original — the owner mail is not forwarded, the command is not
run, the post is not delivered — whether or not a reply went out this time,
and is audited as a discard. See `P3-AUTORESPONDER` in
`docs/FEATURE_PARITY.md`.

## Email commands: echo, end and stop — bounded acceptance verified

The command bot at `list-request@` understands two more of Mailman's verbs:

| Command        | Effect                                                                       |
|----------------|------------------------------------------------------------------------------|
| `echo TEXT`    | replies with that text, unchanged                                            |
| `end` / `stop` | stops reading commands here — a signature or quoted reply below is never run |

`echo` shares `help`'s budget: one bot reply per mailbox, list and hour, plus
the site-wide notice budget. Its text is bounded at 200 characters and must be
printable and single-line, so a reply can never be steered by control or
bidirectional characters. `end` is accepted and does nothing at all: no
notice, no workflow, the job simply finishes. See `P3-EMAIL-COMMANDS` in
`docs/FEATURE_PARITY.md`.

## Subscription policies and the moderator queue — bounded acceptance verified

`subscription_policy` and `unsubscription_policy` now decide what a public
join or leave request becomes, the way Mailman does:

| Policy                  | What happens                                                            |
|-------------------------|-------------------------------------------------------------------------|
| `open`                  | the roster changes at once; welcome/goodbye notices follow, no token    |
| `confirm`               | a confirmation mail; replying (or posting the token) applies the change |
| `moderate`              | the request waits for a moderator; the requester gets no token          |
| `confirm_then_moderate` | the address is confirmed first, then a moderator decides                |

Moderators work the queue with the CLI:

```sh
listmngr requests ls dev.example.com     # one JSON object per waiting request
listmngr requests accept <request-id>    # applies the join or leave
listmngr requests reject <request-id>    # closes it, nothing changes
listmngr requests discard <request-id>   # closes it silently, leaving no row
listmngr requests defer <request-id>     # leaves it waiting, recorded in the audit log
```

Every decision commits with the membership change and the audit event it
causes. A request waiting for a moderator is never swept by the confirmation
expiry. See `P3-SUBSCRIPTION-POLICY` in `docs/FEATURE_PARITY.md`.

The same queue is on the REST API in Mailman's shape, on both `/3.1` and
`/api/v1` (scope `moderation`):

```text
GET  /lists/{id}/requests[?token_owner=subscriber|moderator&request_type=subscription|unsubscription]
GET  /lists/{id}/requests/count
GET  /lists/{id}/requests/{token}
POST /lists/{id}/requests/{token}    action=accept|reject|discard|defer [&reason=…]
```

Entries carry `email`, `display_name`, `list_id`, `token` (the request id),
`token_owner`, `type`, `request_date`, `self_link` and `http_etag`. A
moderator may act on a request that still waits for the subscriber's
confirmation: accepting it applies the change and spends the token. See
`P3-SUBSCRIPTION-REQUESTS-REST` in `docs/FEATURE_PARITY.md`.

`POST /members` is Mailman's registrar. For `role=member` the list's
`subscription_policy` decides what the subscription still needs, and the
flags supply those steps in advance:

| Request                                       | Result                                                             |
|-----------------------------------------------|--------------------------------------------------------------------|
| `pre_verified`+`pre_confirmed`+`pre_approved` | `201` with the member                                              |
| address not yet proven, or the list confirms  | `202` `{token, token_owner: "subscriber"}` and a confirmation mail |
| confirmed but the list moderates              | `202` `{token, token_owner: "moderator"}`, no mail                 |
| `invitation=true`                             | `202`, an invitation mail; accepting it subscribes, no moderator   |

The `token` is the request's REST handle (`/requests/{token}`), never the
secret in the mail. An approval given up front survives the confirmation the
subscriber still owes. `role=owner`, `moderator` and `nonmember` are role
records: created outright, with no workflow and no flags. See
`P3-ADMIN-SUBSCRIBE` in `docs/FEATURE_PARITY.md`.

## LMTP size and body parameters — bounded acceptance verified

The LMTP listener now honours the extensions it announces. A front MTA that
declares `MAIL FROM:<…> SIZE=n` larger than `mta.max_message_bytes` is
refused with `552 5.3.4` before `DATA`, so an oversized message is never
transferred; `BODY=7BIT` and `BODY=8BITMIME` (RFC 6152) are accepted, and any
other parameter — `BODY=BINARYMIME`, DSN's `RET`/`NOTIFY`/`ORCPT`, `AUTH` —
is refused with `555 5.5.4` rather than silently ignored, because this server
announces none of them.

Outgoing mail follows the same rule: a message containing 8-bit octets is
sent as `MAIL FROM:<…> BODY=8BITMIME` when the relay announces 8BITMIME, and
a relay that does not announce it never receives the message (the delivery
retries instead). Every current MTA announces 8BITMIME; a relay that does not
needs the message re-encoded upstream. See `P2-LMTP-PARAMETERS` in
`docs/FEATURE_PARITY.md`.

## Mail metrics — bounded acceptance verified

`GET /metrics` (unauthenticated, Prometheus text) now reports the mail path
next to `listmngr_up`:

| Metric                                                                   | Kind      | Meaning                                                                                                    |
|--------------------------------------------------------------------------|-----------|------------------------------------------------------------------------------------------------------------|
| `listmngr_lmtp_recipients_total{result}`                                 | counter   | LMTP `RCPT` outcomes: `accepted`, `rejected`, `deferred`                                                   |
| `listmngr_posts_total{disposition}`                                      | counter   | in-runner decisions: `accepted`, `held`, `rejected`, `discarded`, `filtered`, `owner`, `command`, `failed` |
| `listmngr_delivery_recipients_total{result}`                             | counter   | outgoing recipients: `sent`, `transient`, `permanent`, `ambiguous`                                         |
| `listmngr_smtp_transactions_total{result}`                               | counter   | relay transactions `completed` (DATA answered) or `failed` before DATA                                     |
| `listmngr_smtp_transaction_seconds`                                      | histogram | duration of one relay transaction                                                                          |
| `listmngr_delivery_latency_seconds`                                      | histogram | LMTP acceptance → relay accepting a recipient                                                              |
| `listmngr_queue_jobs{queue,state}`                                       | gauge     | jobs per queue and state (what `queue stats` prints)                                                       |
| `listmngr_queue_shunted_jobs`, `listmngr_queue_oldest_ready_age_seconds` | gauge     | shunted jobs; how long the oldest ready job has waited                                                     |

Counters and histograms live in the process (the mail role runs inside
`serve`), so a restart resets them; the queue gauges come from the database
and are cached for five seconds per process. See `P2-METRICS` in
`docs/FEATURE_PARITY.md`.

## Authentication and DMARC mitigation — bounded acceptance verified

Turn on `mta.authenticity_checks` and every post is checked for SPF, DKIM
and DMARC before the posting chain runs; the verdict travels with the post
as `Authentication-Results` and drives DMARC mitigation the way Mailman
does: `dmarc_mitigate_action = munge_from` rewrites `From` only for posters
whose domain publishes `p=reject` or `p=quarantine` (set
`dmarc_mitigate_unconditionally` to munge everyone, or list addresses in
`dmarc_addresses`), and `reject` / `discard` refuse such posts with
`dmarc_moderation_notice`:

```toml
[mta]
authenticity_checks = true   # uses the system resolver
```

The client IP for SPF comes from the first `Received:` header, which your
MTA writes. See `P2-VALIDATE-AUTHENTICITY` in `docs/FEATURE_PARITY.md`;
`[mta.arc]` (`P6-ARC-SEAL`, above) seals deliveries with these results.

## Delivery sizing and retries — bounded acceptance verified

Shared deliveries go out in transactions of at most
`mta.max_recipients_per_transaction` recipients (Mailman's `max_recipients`),
grouped by domain; transient failures back off exponentially with jitter
between `mta.retry_initial_secs` and `mta.retry_max_secs`:

```toml
[mta]
max_recipients_per_transaction = 100
retry_initial_secs = 10   # then 20, 40, 80 … seconds, ±20%
retry_max_secs = 3600
```

`listmngr queue stats` shows depth per queue and state, how many jobs are
shunted and how long the oldest ready job has waited; `listmngr queue
unshunt <id> --target <queue>` replays a shunted job. See
`P2-DELIVERY-POLICY` in `docs/FEATURE_PARITY.md`.

## Personalized delivery and VERP — bounded acceptance verified

`personalize = individual` sends every member their own copy: the list
header and footer can use `$user_email`, `$user_name`,
`$user_delivered_to`, `$user_language` and `$member`, and the copy carries
the one-click unsubscribe pair. `personalize = full` also addresses the
copy to the member (`To: Name <address>`). Turn on VERP so bounces name the
member they concern even when the bouncing server does not:

```toml
[mta]
verp_personalized_deliveries = true   # personalized copies: list-bounces+member=domain@host
verp_delivery_interval = 10           # every 10th post of any list is VERP'd
```

The MTA maps route `list-bounces+local=domain@host` to the LMTP intake,
which records the encoded member on the queued bounce. See `P2-PERSONALIZE-VERP`
in `docs/FEATURE_PARITY.md`.

## One-click unsubscribe (RFC 8058) — bounded acceptance verified

Mailbox providers that require `List-Unsubscribe-Post` for bulk senders get
it from any personalized list: set `personalize` to `individual` (or `full`)
and `site.base_url`, and every subscriber copy is delivered on its own with
`List-Unsubscribe: <https://…/unsubscribe/{list_id}?token=…>, <mailto:…>` and
`List-Unsubscribe-Post: List-Unsubscribe=One-Click`. A `POST` to that URL
with the body `List-Unsubscribe=One-Click` unsubscribes immediately (goodbye
notice and audit included); a person who follows the link gets a
confirmation page instead. Links are per recipient, never contain the
address, expire after 90 days, and are signed with a site key generated
into the database on first use — delete the `site_secrets` row to rotate
it.

```sh
curl -X PATCH -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
  https://lists.example.com/api/v1/lists/dev.example.com/config \
  -d '{"personalize":"individual"}'
```

See `P2-ONE-CLICK-UNSUBSCRIBE` in `docs/FEATURE_PARITY.md`.

## Mailman's header set — bounded acceptance verified

Delivered posts carry Mailman's headers: `List-Id` (with the list
description), `List-Help`, `List-Subscribe`, `List-Unsubscribe`, `List-Post`
(`NO` for announce-only lists with `allow_list_posts = false`), `List-Owner`,
`Precedence: list`, `Sender: <list>-bounces@…`, `X-Mailman-Version`,
`Message-ID-Hash`/`X-Message-ID-Hash`, and — once `site.base_url` is set —
`List-Archive` and `Archived-At` pointing at HyperKitty-shaped archive URLs.
`include_rfc2369_headers = false` suppresses the `List-*` set,
`include_sender_header = false` keeps the poster's own `Sender`, and
`reply_goes_to_list` with `reply_to_address` and `first_strip_reply_to`
drive `Reply-To` exactly as in Mailman (`no_munging`, `point_to_list`,
`explicit_header`, `explicit_header_only`):

```toml
[site]
base_url = "https://lists.example.com"   # enables List-Archive / Archived-At
```

See `P2-COOK-HEADERS` in `docs/FEATURE_PARITY.md`.

## List headers, footers, topics and receipts — bounded acceptance verified

Every subscriber copy carries the list's `list:member:regular:header` and
`list:member:regular:footer` templates (Mailman's default footer ships
built in), expanded with `$display_name`, `$listname`, `$short_listname`,
`$domain` and the other list placeholders and added the way Mailman does:
concatenated into a plain-text body, spliced into a `multipart/mixed`, or
wrapped around anything else. Archive and digest copies are never decorated.
Override the footer per list, domain or site through the template resources:

```sh
curl -X PUT -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
  https://lists.example.com/api/v1/lists/dev.example.com/templates/list:member:regular:footer \
  -d '{"language":"en","body":"-- \n$display_name -- $listname\nUnsubscribe: $leave_email\n"}'
```

Topics work as in Mailman: enable `topics_enabled`, define `topics` (each a
name and a multi-line pattern whose lines are alternatives), and matching
posts — by `Subject:`, `Keywords:` or the header-like lines opening the body,
up to `topics_bodylines_limit` — carry `X-Topics`. Accepted posts bump the
list's `post_id` and `last_post_at`, and members whose `acknowledge_posts`
preference is on receive `list:user:notice:post` in their language. See
`P2-HANDLERS-DECORATE` in `docs/FEATURE_PARITY.md`.

## Content filtering — bounded acceptance verified

Lists filter attachments and rich text the way Mailman does. Turn on
`filter_content`, then remove or keep MIME types (`type` or `type/subtype`)
and file-name extensions, collapse HTML alternatives to the first part, and
convert HTML to plain text:

```sh
curl -X PATCH -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
  https://lists.example.com/api/v1/lists/dev.example.com/config \
  -d '{"filter_content":true,"filter_types":["application/octet-stream","image"],
       "filter_extensions":["exe","bat"],"collapse_alternatives":true,
       "convert_html_to_plaintext":true,"filter_action":"reject"}'
```

Surviving parts are delivered byte-for-byte; a changed message carries
`X-Content-Filtered-By: listmngr/mime-delete`. When nothing deliverable is
left, `filter_action` decides: `discard` (silent), `reject` (the author gets
`list:user:notice:rejected` with Mailman's reason), `forward` (the moderators,
or the owners when the list has none, receive the only copy attached as
`message/rfc822`) or `preserve` (kept in the shunt store for `listmngr queue`
when `[mailman] filtered_messages_are_preservable = true`; otherwise a
discard, as in Mailman). Every outcome is audited as `post.*`. Posts the
chain rejects now also notify their author. See `P2-MIME-DELETE` in
`docs/FEATURE_PARITY.md`.

## Mailman list settings — bounded acceptance verified

The list configuration resource now carries Mailman's Alter Messages group
(`filter_content`, `filter_types`, `pass_types`, `filter_extensions`,
`pass_extensions`, `collapse_alternatives`, `convert_html_to_plaintext`,
`filter_action`, `include_rfc2369_headers`, `allow_list_posts`,
`reply_goes_to_list`, `reply_to_address`, `first_strip_reply_to`,
`personalize`, `include_sender_header`), the Member Policy group
(`subscription_policy`, `unsubscription_policy`, `member_roster_visibility`),
the DMARC text settings (`dmarc_addresses`, `dmarc_moderation_notice`,
`dmarc_wrapped_message_text`) and `forward_unrecognized_bounces_to`, with
Mailman's defaults and wire values. `mailmanclient` works unchanged:

```python
settings = client.get_list('dev@lists.example.com').settings
settings['filter_content'] = True
settings['filter_types'] = ['image/jpeg', 'application/octet-stream']
settings['filter_action'] = 'preserve'
settings['reply_goes_to_list'] = 'point_to_list'
settings['subscription_policy'] = 'confirm_then_moderate'
settings.save()
```

Values are validated before anything is written and the change is audited
as `list.config`; `PUT` resets omitted settings to their defaults. These are
settings only for now — the handlers that act on them land in later work
packages (content filtering, header munging, personalization, subscription
policies). See `P2-LIST-SETTINGS` in `docs/FEATURE_PARITY.md`.

## Notice languages — bounded acceptance verified

Generated notices are sent in the recipient's language. Each notice picks the
first of the member's `preferred_language`, the list's `preferred_language`
and `site.default_language` that ships as a catalog (`en`, `vi` and the
languages Mailman's translators brought, `P6-PO-TEMPLATES`; regional tags such
as `vi-VN` select `vi`, and an unsupported language falls
through to the next preference and finally English). Subjects come from the
Fluent catalogs in `crates/i18n/locales/`, bodies from the built-in template
catalog in that language (an operator template stored for the language, or for
`en`, still wins at its scope). Owners and moderators each get the hold notice
in their own language. Set a member's language through preferences:

```sh
curl -X PATCH -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
  https://lists.example.com/api/v1/members/$MEMBER_ID/preferences \
  -d '{"preferred_language":"vi"}'
```

The confirmation `Subject` stays `confirm TOKEN` in every language. See
`P3-I18N` in `docs/FEATURE_PARITY.md`.

## Mailman notice templates — bounded acceptance verified

Every generated notice (welcome, goodbye, confirmation challenge, command
help, confirmation receipt, moderator rejection, bounce warnings and owner
bounce notices, and the new hold notices) renders from a Mailman-named
template with Mailman placeholders such as `$listname`, `$display_name`,
`$owner_email`, `$request_email`, `$subject`, `$reasons` and `$user_email`.
Resolution is list → domain → site → built-in English, trying the list's
preferred language and then `en` at each scope. Manage templates the way
`mailmanclient` does:

```sh
# point a template at a file on the host (mailman:/// selects the built-in)
curl -X PATCH -u "$TOKEN:" https://lists.example.com/3.1/lists/dev.example.com/uris \
  -d 'list:user:notice:welcome=file:///etc/listmngr/templates/welcome.txt'
# or store an inline body in a language (listmngr extension)
curl -X PUT -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
  https://lists.example.com/api/v1/lists/dev.example.com/templates/list:user:notice:welcome \
  -d '{"language":"vi","body":"Chào mừng đến với $display_name!\n"}'
```

`GET /uris` lists managed template URIs on the list, domain (`/domains/{host}/uris`)
and site (`/uris`, administrators only) scopes; `PUT` replaces the set and
`DELETE` clears it. `https://` sources are accepted but not fetched by this
runtime; a template that cannot be loaded falls back to the next scope and
finally the built-in, and is logged by name only. Holding a post notifies the
poster (`respond_to_post_requests`, default true; a null or list-owned sender
is never notified) and the owners and moderators (`admin_immed_notify`,
default true). Non-ASCII templates and subjects are encoded safely. See
`P2-TEMPLATES` in `docs/FEATURE_PARITY.md`.

Mailman's `admin_notify_mchanges` (migration `0041`, default false, on the
list configuration resource under both prefixes) tells every owner and
moderator when a member is subscribed (`list:admin:notice:subscribe`,
"*display name* subscription notification") or removed
(`list:admin:notice:unsubscribe`, "… unsubscription notification"), naming
the member's address as `$member`. The notices are enqueued in the
membership's own transaction by every path that inserts or deletes a member
role — REST, CLI, mass operations, confirmed and moderated workflows, bounce
removal and one-click unsubscribe — independently of `send_welcome_message`
and `send_goodbye_message`, each in the administrator's own language. Owner
and moderator role changes are not membership changes. See
`P3-ADMIN-NOTIFY-MCHANGES` in `docs/FEATURE_PARITY.md`.

## Phase 2 end-to-end acceptance — bounded acceptance verified

`crates/cli/tests/mailpath_e2e.rs` runs the real `listmngr serve` binary on a
disposable SQLite database with a bound LMTP socket, a real SMTP sink and,
when asked, a DKIM key generated by the system OpenSSL, and covers the plan's
Phase 2 acceptance matrix (`P2-E2E-ACCEPTANCE`): a member's post reaches an
enabled member once and never a disabled one; the delivered copy carries
`Subject: [Dev] …`, `List-Id`, `List-Post`, `List-Unsubscribe`, `List-Archive`,
`Precedence: list` and the list's footer template, and its `DKIM-Signature`
verifies (`dkim=pass`) against the fixture's public record; a nonmember's post
is held; a banned sender receives the rejection notice and the members nothing;
a post over `max_message_size` is held with the reason; `personalize = full`
with `verp_personalized_deliveries` sends one copy per member with
`dev-bounces+local=domain@host` envelopes, a personal `To` and the RFC 8058
one-click pair; a post accepted while the server is stopped is delivered
exactly once after start; and a server killed with SIGKILL while the relay
holds its DATA answer delivers exactly once after restart against the same
database. Kill points inside the `in` stage are not scheduled by this harness;
lease expiry and re-claim there are covered by the repository contracts.

## Mailman handler pipeline — bounded acceptance verified

Accepted posts run the list's `posting_pipeline` (default
`default-posting-pipeline`): `member-recipients`, `cleanse`, `cleanse-dkim`,
`cook-headers`, `subject-prefix`, `rfc-2369`, `to-archive`, `to-digest`,
`to-usenet` (since `P6-NNTP-GATEWAY`), `dmarc`, `to-outgoing`. The `in`
runner takes the fan-out decisions from the pipeline; the archive, digest,
news and delivery consumers each receive the message
as it stood at their own `to-*` handler, so DMARC `From` rewriting reaches
subscribers but never the archive or digests. `GET /system/pipelines` lists
each pipeline with its real handler order and whether it can run. The
`posting_pipeline` setting accepts only a registered, executable pipeline that
resolves recipients and delivers. Header emission order now follows handler
order, and the archive copy carries the full `X-BeenThere` loop history. See
`P2-PIPELINE-HANDLERS` in `docs/FEATURE_PARITY.md`.

## Mailman posting rules and header matches — bounded acceptance verified

Inbound posts run Mailman 3's built-in chain order: `no-senders`, `approved`,
`emergency`, `loop`, `banned-address`, member/nonmember moderation, then the
deferred `administrivia`, `implicit-dest`, `max-recipients`, `max-size`,
`news-moderation` (since `P6-NNTP-SETTINGS`), `no-subject` and
`suspicious-header` checks, a detour through the list's own header rules,
and accept. When several deferred checks hit, the single held
message lists every reason. An explicit member or nonmember `accept` bypasses
the deferred checks, exactly as in Mailman; emergency moderation and bans
still apply. Owners and moderators post as explicitly accepted senders.

List configuration (JSON or form, both `/api/v1` and `/3.1`) gains
`administrivia` and `require_explicit_destination` (booleans, default true),
`acceptable_aliases`, and `accept_these_nonmembers` / `hold_these_nonmembers` /
`reject_these_nonmembers` / `discard_these_nonmembers` (arrays of exact
addresses or `^`-anchored regexes; the lists win over a `nonmember` role row,
which wins over `default_nonmember_action`). `moderator_password` is
write-only: PATCH/PUT accept a plaintext that is stored as Argon2id, an empty
string clears it, and no read ever returns it. A post carrying that key in an
`Approved:`/`Approve:`/`X-Approved:`/`X-Approve:` header, or as the first line
of an unencoded plain-text body, is accepted and the key is removed before
delivery, archiving and digesting. A key inside a base64 or quoted-printable
part is neither honored nor stripped; use the header form.

Site-wide header checks live in `listmngr.toml`:

```toml
[antispam]
jump_chain = "hold"                      # where per-list rules without a chain go
header_checks = [
  { header = "X-Spam-Flag", pattern = "^yes$" },
]
```

Per-list `header_matches` rows (header, pattern, optional `chain` of
`accept|hold|reject|discard`, optional `tag`) are evaluated in position order;
the first match wins. Rows are validated with the exact regex settings used at
evaluation, and a stored pattern that no longer compiles holds the message
naming the row instead of ignoring it. See `P2-CHAIN-RULES` in
`docs/FEATURE_PARITY.md` for evidence and the full list of deliberate
deviations.

Rows are managed through Mailman's `header-matches` resource on both prefixes
(`P2-HEADER-MATCHES-REST`): `GET`/`POST`/`DELETE /lists/{id}/header-matches`,
`GET`/`PATCH`/`PUT`/`DELETE /lists/{id}/header-matches/{position}` and
`POST /lists/{id}/header-matches/find`. Rows are numbered from zero; a `POST`
appends and answers `201` with the new row's `Location`; `PATCH` changes only
the fields it names and `position` moves the row, shifting the ones in
between; `PUT` needs `header` and `pattern` and clears the optional fields it
leaves out; deleting a row renumbers the rest. The header name is stored
lower-cased. The compatibility prefix speaks Mailman's dialect — the chain
under `action`, absent optional fields omitted, form bodies as mailmanclient
sends them — and the typed prefix uses `chain` with explicit `null`s; both
accept either spelling on input. The same header and pattern twice is a `400`,
as in Mailman; `defer` names no chain and is refused. `find` takes any of
`header`, `tag` and `action` and returns the matching rows with their real
positions (an empty collection when none match). Every edit rewrites the
list's set inside one transaction under the list's writer reservation and
records one `list.header_matches` audit event naming the change. The browser
UI does not expose the rows yet.

## Experimental outbound DKIM (local acceptance verified)

The current candidate passes the workspace, PostgreSQL, compatibility-client and
Chromium gates. Seven fresh production SMTP fixture profiles also verify with
dkimpy 1.1.8 and eight negative controls each. This is local fixture acceptance,
not DNS publication, real MTA cutover or complete Mailman replacement. See the
current evidence section in `docs/FEATURE_PARITY.md`.

Signing uses RFC 6376 `relaxed/simple` canonicalization. The pinned signing
library's relaxed body mode mishandles trailing whitespace-only lines; simple
body mode preserves MIME bytes and avoids that defect. Benign body whitespace
changes in transit can therefore invalidate a signature. A regression includes
empty, whitespace-only, non-UTF-8 and interior/trailing blank-line bodies.

The outgoing runner can sign final cooked messages with an operator-configured
RSA key. Configuration is opt-in and scoped to the authoritative stored list
domain, not the author's From address. For example, in the existing TOML config:

```toml
[[mta.dkim_signing]]
domain = "lists.example.invalid"
selector = "outbound"
private_key_file = "/operator-managed/path/to/private-dkim-key.pem"
```

This is a placeholder, not a deployable key path or a command to enable the MTA.
Use a PEM RSA key of at least 2048 bits, a regular file no larger than 64 KiB,
and owner-only permissions (`0600` recommended on Unix). The service user must
be able to read it. Unix FIFO paths are rejected without waiting for a writer.
Keep private keys outside the repository; configuration carries a path, never
inline key bytes. Keys load when mail-role configuration is constructed; restart
the role after rotating a key. Publish the matching public key as a DNS TXT record
at `outbound._domainkey.lists.example.invalid`, with value
`v=DKIM1; k=rsa; p=<base64 DER SubjectPublicKeyInfo public key>` before real use.
DNS publication and production delivery have not been exercised here.

Empty signing configuration and unconfigured list domains remain unsigned.
Invalid configured keys fail role construction; local signing failures shunt the
job before opening SMTP, without creating a mailbox bounce event. Signing uses
`mail-auth =0.12.1` with only its ring crypto backend, normalizes transport CRLF,
oversigns From, and leaves stored raw content unchanged. Private/owner/digest
paths share the final signing step without changing their recipient/envelope rules.

Affected mail/runners all-target tests pass: 153 passed, 0 failed, 1 ignored;
strict affected-package Clippy, formatting and artifact checks pass. The FIFO
regression was observed failing before the nonblocking-open repair.
See `P3-DKIM` in `docs/FEATURE_PARITY.md` for evidence and outstanding gates.
Independent dkimpy 1.1.8 verification now covers seven actual SMTP fixture
captures: ordinary posts, owner forwarding, private rejection notices, and
regular/plain/MIME/summary digest producers. Each passes a valid-signature check
and seven rejection controls. Read-only source review, workspace build/Clippy,
fresh deny/audit and disposable PostgreSQL checks also pass. The first full run
timed out in workspace tests and failed capture freshness, so it was rejected.
A resource-bounded full retry is running; final acceptance remains pending.
This is not incoming SPF/DKIM/DMARC verification, ARC, verified DNS deployment,
full mail-authentication parity, or production readiness. Earlier checkpoint
descriptions below are historical; the current DKIM candidate is not yet accepted.

## Opt-in durable welcome notices (bounded)

Authenticated list config at `/api/v1/lists/{id}/config` and
`/3.1/lists/{id}/config` now exposes `send_welcome_message` (default **false**).
PATCH `{"send_welcome_message":true}` to enable it; form bodies accept
`send_welcome_message=true` or `false`. JSON remains strictly boolean. PATCH
preserves omitted settings; PUT resets an omitted welcome setting to false.
The setting is included in config GET, attribute GET and OpenAPI.

When enabled, an actual new Member subscription from direct/API, bulk/sync or
confirmed join enqueues one private built-in welcome in the same transaction as
membership and audit. Existing membership/no-op, other roles, pending requests,
replay and disabled lists do not create a welcome. Banned mailboxes receive no
welcome. Existing administrative direct/bulk admission remains unchanged;
public/email join workflows continue to enforce bans independently of this flag.
Confirmed joins still receive the separate, unconditional completion receipt:
an enabled new join therefore produces one welcome **plus** one receipt.

The English plain-text notice is bounded to 4096 bytes and uses stored list
identity and subscribed mailbox spelling, not the administrator/requester,
display names, descriptions, submitted content or confirmation secrets. Existing
job-bound notice provenance, private recipient snapshots, null SMTP reverse path
and outbound retry/lease fencing remain in use. This is exactly-one durable
enqueue per successful insertion, **not exactly-once SMTP delivery**.

Focused SQLite/API→reopened DB→real disposable SMTP and rollback evidence lives
in `target/welcome-evidence/` (`P3-WELCOME`). Parent acceptance: workspace
**465 passed / 0 failed / 30 ignored**, build/fmt/strict Clippy/artifact/diff and
fresh security gates PASS. Disposable PostgreSQL 14.24: canonical 13 tests and
serial CLI welcome default/enable/private-recipient/owner/disable tracer PASS;
owned cluster cleanup verified. See `target/welcome-evidence/final.json`.
PostgreSQL contention and live MTA/cutover remain unverified. This slice
does not implement custom templates/overrides, language fallback, goodbye,
invitation/admin notices or full Mailman welcome parity.

## Durable incoming bounce inbox (bounded)

The experimental LMTP role now accepts each list's bare `-bounces` address,
including null-envelope-sender and automatic reports. Raw bytes, list-scoped
context, a `bounces` queue job and enqueue audit commit before the positive DATA
reply. These untrusted reports never enter ordinary posting or create trusted
SMTP failure events. A missing Message-ID receives an internal storage identity
without modifying raw bytes; malformed/duplicate IDs and header bounds remain
enforced. Ordinary posts still require their Message-ID. Exact posting-list
names take precedence over suffix routing; VERP/plus addresses remain rejected.

Operators can inspect retained reports with `listmngr queue ls --queue bounces`,
`listmngr queue show JOB_ID`, and explicit `listmngr queue show JOB_ID --raw`.
Raw reports can contain private messages and tokens: protect the spool and any
exports. After review, run `listmngr queue acknowledge-bounce JOB_ID --reason
"reviewed report"`. Only a stored `bounces`/`ready` job can become `done`, atomically
with one `queue.acknowledge_bounce` audit. Repeating the command fails without
another audit or timestamp changes. The reason is trimmed, nonempty, single-line,
and at most 2,048 UTF-8 bytes; do not include message contents or secrets.
Use `listmngr queue ls --queue bounces --state ready` for the pending backlog.
The optional state accepts ready/leased/done/shunted and filters before LIMIT 1000;
omitting it preserves the existing retained-job listing. Show/raw exports still
work after acknowledgement. This is trusted local operator bookkeeping, not
proof of delivery failure. Since `P3-BOUNCE-RUNNER` the mail role consumes
this inbox (see "Bounce processing" below); the operator commands remain for
whatever the runner has not yet reached or has shunted. This closes intake loss, not DSN/VERP authentication or full bounce
processing. Existing MTA maps require explicit regeneration/review; no live MTA
configuration is changed. See `P3-BOUNCE-INBOX` in `docs/FEATURE_PARITY.md`.

Prior intake-only candidate gates passed: 452 workspace tests, 0 failures, 30 ignored;
build/fmt/strict Clippy/deny/fresh HTTPS advisory audit and explicit real Postfix
fixture lookup pass. This is not PostgreSQL or MTA-daemon acceptance.
Acknowledgement focused evidence: `cargo test --locked -p listmngr-db --test bounce_ack`
(3 pass), `cargo test --locked -p listmngr --test bounce_inbox` (3 pass), affected
strict Clippy and workspace fmt check PASS in `target/bounce-ack-evidence/`.
Final acknowledgement acceptance: 457 workspace tests passed, 0 failed, 30 ignored
(79 summaries); build/fmt/strict workspace Clippy/artifact/diff passed, with all
20 required regression markers. Fresh official HTTPS audit passed. Default deny
fetch failed over SSH; an online retry with a fresh advisory directory and
child-process Git-config isolation passed without changing policy or Git files.
`scripts/test-postgres.sh` passed 13 tests on a disposable PostgreSQL 14.24 cluster.
A separate real CLI/PostgreSQL tracer verified acknowledgement, ready/default/done
listing, retained raw, wrong-queue/replay rejection and exactly one audit. All
owned clusters were stopped and removed. This does not certify PostgreSQL
acknowledgement contention, other server versions, live MTA or production cutover.
Evidence and retained initial failures: `target/bounce-ack-evidence/final.json`.

## Direct SMTP failure events (bounded)

The experimental outgoing runner now records permanent failures for reserved
recipients of ordinary list jobs as durable bounce metadata. Administrators can
read `GET /api/v1/lists/{id}/bounces` (also `/3.1/lists/{id}/bounces`) with a
`lists:read` token bound to the appropriate list/domain. Pagination supports the
existing `count`, `page` or `cursor` contract, with at most 100 records per page.
Events retain the original recipient spelling, internal message/job IDs and a
Unix-millisecond timestamp, but no SMTP diagnostic, raw body or token-bearing
context. Events survive spool removal and are deleted with their owning list;
there is no independent event-retention scheduler yet.

New remote 5xx events also expose nullable `smtp_stage` (`ehlo`, `mail_from`,
`rcpt`, `data_start`, `data_final`) and numeric `smtp_code`. These values come
from the actual SMTP reply and command state, not diagnostic-text parsing.
Historical events and legacy/local failures keep both fields `null`. A retained
RCPT rejection is not overwritten by a later DATA rejection. Even `rcpt`/550
can represent policy, not an invalid mailbox. Greeting non-220 replies remain
transient; retry and ambiguous-delivery behavior is unchanged. Final parent
workspace gates pass: 447 passed / 0 failed / 30 ignored, with build, fmt,
strict Clippy, deny and fresh HTTPS audit PASS (see **P3-SMTP-FAILURE-METADATA**).
Ignored PostgreSQL tests and live MTA operation are not certified.

Recording, recipient outcome, queue completion/retry and audit are atomic and
lease-fenced. Replay does not duplicate an event. Workflow notices, owner mail
and digest deliveries are excluded from this ordinary-post event stream.
These are unprocessed observations (`source=smtp_permanent_failure`,
`context=normal`, `processed=false`), not proof that a mailbox is invalid.
No inbound DSN/VERP handling, bounce scoring, disabling/removal, probes or owner
notifications are enabled by this change. See **P3-SMTP-BOUNCE-EVENT** in
`docs/FEATURE_PARITY.md`; full Mailman and live PostgreSQL/MTA acceptance remain open.

## Confirmation completion receipts (email and HTTP)

After confirming a join/leave challenge by email, public REST or the browser
confirmation form, the requester receives a private `List join request completed` or
`List leave request completed` receipt. It records the outcome at confirmation
time and provides help and human-administrator addresses. Delivery goes only to
the original mailbox stored with the token, never the confirming email's sender,
From or Reply-To. No token or quoted request body appears in the receipt.

The receipt spool, membership change, token consumption and audit commit together;
email confirmations also include the fenced command ACK. Failed/expired/replayed
confirmations do not publish another receipt. SMTP uses a null reverse path and
`Auto-Submitted: auto-generated`; receipts do not become subscriber posts, archives
or digests. HTTP response formats remain unchanged, but successful confirmation
now queues the same private receipt. A valid token also receives one receipt when
join finds membership already present or leave finds it already absent.
These fixed English receipts are not configurable welcome/goodbye templates or
full subscription moderation/invitation support. Administrative membership CRUD
and authenticated direct member removal are not token confirmations and do not
acquire this behavior. See **P3-HTTP-CONFIRM-RECEIPT** and the preceding email
increment in `docs/FEATURE_PARITY.md` for evidence and remaining boundaries.

## DMARC From rewriting (`munge_from`)

Set list config through JSON or form `PATCH`/`PUT` on either REST prefix:

```json
{"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true}
```

`dmarc_mitigate_action` takes `no_mitigation` (default), `munge_from`,
`wrap_message` (`P6-DMARC-WRAP`, above), `reject` and `discard`;
`dmarc_mitigate_unconditionally` applies the action to every post, and
without it the `dmarc-mitigation` rule decides by the From domain's
published policy when `[mta] authenticity_checks` is on
(`P2-VALIDATE-AUTHENTICITY`), by `dmarc_addresses` always. This section
describes `munge_from`; it is not ARC sealing or proof of delivery
acceptance by remote providers.

Individual subscriber delivery replaces From with the list posting address and MIME-safe
`Author (address) via list-address` attribution. Valid Reply-To mailbox targets
are retained (normalized without display names), otherwise the author is used.
Superseded From/Sender/Reply-To fields are removed; MIME body bytes are unchanged.
One modern ASCII dot-atom author mailbox is required; quoted/encoded display
names are supported, but comments, groups, quoted local parts, domain literals,
SMTPUTF8 mailboxes, duplicate/missing/malformed or control-bearing authors fail
closed. Anonymous-list suppression wins. Owner forwarding and private workflow
notices bypass post cooking. DMARC rewriting does not change archive/mbox authors
or digest articles. MIME/summary digests preserve original article From; plaintext
digest formatting remains unchanged. Shared anonymity suppression still applies
to publications and digest collection, independently of delivery mitigation.
Already-anonymized stored bytes cannot reconstruct the original author.

See acceptance **P2-DMARC-MUNGE** in `docs/FEATURE_PARITY.md` for exact focused
SQLite/SMTP fixture evidence. Conditional DNS/PSL, other actions, DKIM and ARC,
live PostgreSQL/MTA cutover and complete PLAN P2/P6 acceptance remain open.

List ban `Location`/`self_link` URLs now support authenticated GET on both
`/api/v1` and `/3.1`, requiring `lists:read` for the target list. This reads a
stored list-local ban (canonical mailbox or verbatim regex), not effective ban
status for an arbitrary address. Missing/deleted resources return 404.


Experimental public/email join now honors list bans and existing global ban rows.
Banned join requests return the same generic success as ineligible requests,
without a challenge notice, token or new mailbox cooldown. Confirmation rechecks
the stored original mailbox: a ban added after the challenge blocks admission
without consuming the token; removal allows retry within its original expiry.
Leave remains available subject to the cooldown, which since `P3-SUBSCRIPTION-E2E`
counts only requests still waiting. Posting and join share
canonical exact matching and original-case regex matching. Ban writes share the
workflow transaction reservation. This does not change privileged member CRUD or
imports, administer global bans, or evict existing members. See
`P3-SUBSCRIPTION-BAN-ADMISSION` in `docs/FEATURE_PARITY.md` for evidence and limits.

Experimental moderator rejection notices now use the durable outgoing queue.
Rejecting a held post through the existing REST/browser review path (or legacy
repository method) creates one private notice to its stored envelope sender,
atomically with disposition and audit. `discard` stays silent. The notice uses
a null SMTP envelope sender, includes only the moderator comment (at most 4096
UTF-8 bytes plus a truncation marker), and never redistributes the original post.
Automatic/list traffic, malformed messages/context, unsafe or self-list targets,
and envelope/held-sender mismatches suppress the notice but preserve the decision.
These are syntax/loop guards, **not sender authentication**: use discard for spam
or suspected forged senders. Hold notices, automatic policy-rejection notices,
templates/localization and full backscatter protection remain unimplemented.
Delivery requires the configured outgoing runner; REST success means durable
publication, not SMTP acceptance. See `P3-MODERATOR-REJECTION-NOTICE` in
`docs/FEATURE_PARITY.md` for evidence and limits.

Experimental list-scoped posting bans can now be managed through
`GET/POST /api/v1/lists/{id}/bans` and
`DELETE /api/v1/lists/{id}/bans/{email}` (also mounted under `/3.1`).
POST accepts JSON or URL-encoded form with `email`: an exact mailbox, or a
case-sensitive Rust regex beginning with `^`. Exact mailboxes use canonical
case/IDNA/domain-dot identity. Follow the returned `Location` when deleting
regexes or addresses containing reserved URL characters. Collections use the
existing pagination format; access requires `lists:read` or `lists:write` and
the token's list/domain bounds. Writes and audit commit together.
Banned posting senders produce no outgoing/archive/digest fanout, including
otherwise-authorized owners. This is not global-ban administration,
SMTP-time rejection, or a rejection notice. Public/email join admission is
described above; owner routing is unchanged. See `P3-LIST-POSTING-BANS` in
`docs/FEATURE_PARITY.md`; full Mailman-client parity is not claimed.

Mailman's site-wide bans (`P3-SITE-BANS-REST`) are `GET/POST /bans` and
`GET/DELETE /bans/{email}` on both prefixes, with the same `email` input,
canonical identity, `Location` and pagination as the list-scoped resource. A
site ban matches on every list — posting, public/email join and privileged
subscription — but is not one of any list's bans, and a list ban is not a site
ban; the same value site-wide twice is `409`. The rows are server
administration: the token must hold `lists:read` (reads) or `lists:write`
(writes) and be bound to no list or domain. Writes take the site-wide
reservation and commit with their `ban.create`/`ban.delete` audit event on the
`site` target. mailmanclient's `client.bans` (`add`, `in`, `find_by_email`,
`remove`) runs against it in the compatibility gate.

Per-list `max_message_size` is now an experimental posting control. Set it via
list config PATCH on `/api/v1` or `/3.1` (JSON integer or URL-encoded form).
The unit is KiB (1024 bytes), including original headers and body; `0` (the
default) adds no per-list limit. Otherwise-eligible posts above the limit are
held for moderation without outgoing/archive/digest fanout; exact-boundary
posts pass this check. PATCH retains omitted settings; PUT resets them.
The site-wide LMTP hard cap still applies. This is not a memory quota or a
limit on administrative/command mail. See `P2-MESSAGE-SIZE` in
`docs/FEATURE_PARITY.md` for evidence and backend limitations.

Experimental `list-owner@` mail now follows a durable administrative route to
the list's owners **and moderators**, once per address, never to ordinary
subscribers or archive/digest jobs. Exact list names still win over suffixes.
The input message is retained unchanged; outgoing preparation preserves author,
reply/thread and MIME data while removing private/transport-control headers.
Producer-owned database provenance selects this path, not a forged header or
outgoing JSON flag. Forwarding uses a null reverse path and `Auto-Submitted`;
automatic/list traffic and unsafe sender/recipient addresses are refused.
Missing or unsafe administrative rosters are explicitly shunted for operator
inspection. This is not the full configurable Mailman owner chain or notice
lifecycle. Bounce/VERP/DSN, authentication/DMARC, PostgreSQL and actual MTA
cutover acceptance remain open. See `P2-OWNER-FORWARD` in `docs/FEATURE_PARITY.md`.

Lease-renewal tests now separate the production scheduler's virtual-time
cadence/cancellation from database authority checks. The runtime still renews
with the live clock, uses the same TTL/3 timeout, and stops work conservatively
on renewal failure. This removes the short-lease test's dependence on SQLite
thread scheduling; it is not a guarantee against real scheduler stalls. See
`P2-LEASE-HEARTBEAT` in `docs/FEATURE_PARITY.md` for evidence and remaining gates.

`[mta] incoming = "postfix"` (or `"exim"`) makes listmngr publish the MTA's
lookup maps the way Mailman does: at startup, after every list creation or
removal, and on demand with `listmngr aliases regen`. Each run writes an
immutable `generation-<uuid>` directory under `map_directory`, switches the
`current` symlink and prunes old generations. Postfix gets anchored `regexp:`
maps (or Mailman's `hash:` files compiled by `postmap` with
`transport_file_type = "hash"`), Exim gets `lsearch` files for the routers in
`deploy/exim/listmngr.conf`. See [the MTA map runbook](docs/POSTFIX_MAPS.md)
for the formats, permissions and activation boundary. The Compose deployment
now includes a Postfix front MTA built from `deploy/postfix/Dockerfile` that
reads the shared map volume, hands list mail to listmngr over LMTP and relays
its outbound mail; `scripts/check-mta-configs.sh` verifies the shipped Postfix
and Exim configurations in containers. Daemon delivery through Compose end to
end, bounce handling and full Mailman replacement acceptance remain open.

List owners now have “List administration” → “List settings” forms for display
name, description, directory advertising, default member/nonmember posting
actions, and archive policy. All six fields persist together with an attributed
audit event. A currently verified list owner or server owner is required;
moderator-only users cannot read or save these settings. The browser rechecks
session, credential generation and ownership inside the write transaction.
Unrelated settings changed since opening the form are preserved. “Use system
fallback” clears a posting default; explicit defer still accepts after safety
checks. Archive policy changes do not delete retained messages.

This increment passed SQLite HTTP/causal-contention tests, actual Chromium native
form save/reload, and all local required workspace gates (evidence and exact
commands in `docs/FEATURE_PARITY.md`). Shared PostgreSQL tests are implemented
but their execution was permission-denied; PostgreSQL acceptance for this
increment remains **blocked**, not inferred from SQLite. This is not full list
administration or Mailman/Postorius parity.

The owner member roster now supports literal email-substring search. Enter an
address or fragment in “Search member email”; `%`, `_`, `!` and `+` are literal,
not search operators. Search is applied before pagination, and the query/page
survive navigation and posting-policy saves. “Clear search” restores the roster.
Queries are limited to 320 UTF-8 bytes and may not contain control characters.

List owners can use “My subscriptions” → “List administration” to browse member
subscriptions and set their posting-policy overrides: hold, accept, reject,
discard, defer, or clear the override to use the list default. Lists and member
rosters are paginated, including unadvertised lists the user owns. A verified
server owner may administer all lists; a moderator role alone cannot do this.
Overrides affect future posting decisions, not existing held/queued messages.
The current runtime treats explicit defer as accept after its safety checks;
“Use list default” instead clears the override. This is not full list administration.

Members can leave a subscription from “My subscriptions” → “Leave list”, even
when its list is not advertised. A confirmation page identifies the membership
address and list; only the subsequent protected POST removes it. Account/address
records, other subscriptions and owner/moderator roles remain intact. Already
queued mail may still arrive. Anonymous leave requests still require email proof.

Signed-in users can change their password from “My subscriptions” → “Change
password”. The current password and matching confirmation are required; the
configured password-strength policy applies. Success signs out all that user's
browser sessions, without affecting other users. This is not email password reset
or account signup, and does not revoke independently issued API tokens.

Archive processing tolerates malformed optional threading metadata: it uses the
first valid References identifier, then a valid In-Reply-To, or the message's own
identity as a standalone thread. Invalid hints no longer reject a valid post.
Existing parent/root resolution remains list-scoped, including late-arriving
parents; this does not automatically reindex old archives or retry shunted jobs.

Public archives are now browsable from a list's “Browse public archive” link at
`/web/lists/{id}/archive`: escaped plain-text messages, literal substring search,
thread filtering, 20-message pages and per-message “Permanent link” URLs using
`?message={hash}`. Permalinks select the exact list/hash rather than searching a
page of messages; missing messages return 404. Private archives are available from
“My subscriptions” → “Read archive” to logged-in members with a currently verified,
owned membership address and valid session. A server-owner flag alone grants no
browser archive access. Anonymous/nonmembers remain denied; disabled archives
return 404. Messages now offer individual “Download attachment” links under the
same archive authority. Downloads use a safe `attachment-N.bin` filename and
`application/octet-stream`, never inline HTML. Richer threading and full
HyperKitty parity remain open.

Attachment projection accepts cooked messages up to 10 MiB and 64 attachments;
parser-reported transfer-encoding errors are rejected. Both text and binary
downloads retain transfer-decoded payload bytes without charset conversion.
Display text remains a separate UTF-8 projection. This is not a complete original
message backup, malware scanner or process-wide memory cap.

“Download this selection (mbox)” exports the current page, search/thread selection
or permalink via `format=mbox`, preserving the displayed filters and current
archive policy. It contains at most 20 cooked messages, not a complete archive
backup and not an original-spool export.

**Current convergence:** browser self-service/login and lock-bound held-recipient
selection are integrated with canonical email/digest/archive/lease behavior. This
has passed parent workspace gates, isolated PostgreSQL gates and actual Chromium
self-service checks on the composed source, but is not whole-product acceptance. See the
current [convergence ledger](docs/FEATURE_PARITY.md); older checkpoints below are
historical. Standalone live email acceptance remains blocked by denied permission.

Mailbox identity and SMTP destination spelling are separate: confirmed new
subscriptions retain the original mailbox spelling for regular delivery. A list
confirmation does not verify or relink a pre-existing user account. Focused
regressions and a restart/HTTP/LMTP/SMTP probe cover this repair; composed release
acceptance remains separate (see `docs/FEATURE_PARITY.md`).

`listmngr` is a security-focused mailing-list manager written in Rust, version **0.1.0 (unreleased development)**, licensed **AGPL-3.0-or-later**. Phase 1 has recorded local acceptance evidence. The current development checkpoint adds a **bounded, opt-in plaintext trusted-relay LMTP → held moderation → SMTP path**, durable queue attempts and conservative uncertainty quarantine through migration `0004_delivery_attempt_token.sql`.

**This is not production-ready or a complete Mailman replacement.** The composed development tree now includes subscription confirmation, digest and archive behavior. Parent verification on 2026-09-06 passed locked workspace build, workspace tests on rerun, full Clippy, isolated PostgreSQL and the pinned mailmanclient 3.3.5 bounded compatibility probe. An initial full-suite heartbeat test failed under concurrent load and passed in isolation and on rerun; this timing sensitivity remains open. Lease-lock fencing, browser UI and email-only confirmation worktrees are not covered by this composed result. See [`docs/FEATURE_PARITY.md`](docs/FEATURE_PARITY.md) for exact evidence boundaries; [`docs/PLAN.md`](docs/PLAN.md) remains the normative product target.

## Integrated email commands (bounded development slice)

Canonical now includes durable `join`/`subscribe`, `leave`/`unsubscribe`,
`confirm TOKEN` and bounded `help`. Use `list-join@`, `list-leave@`, their aliases,
`list-confirm@`, or send a command to `list-request@`. An existing exact list
posting address wins over suffix routing. Confirmation notices provide a
`Reply-To` and `Subject: confirm TOKEN`; membership changes only after consuming
that list-scoped, one-time token. No mailbox arguments or moderator commands are
supported. An explicit subject takes precedence; otherwise only the first
nonblank line within 20 actual text/plain body lines is considered, never HTML
conversion or attachments. Null/automatic senders cannot solicit command replies.

Help notices now set `Reply-To: list-request@host`, rather than sending a user's
command reply to administrators via the `From: list-owner@host` address. Click
Reply and **replace the subject with a single command**, for example `join`;
keeping `Re: List email command help` is still unsupported. The help body also
names `list-owner@host` for a separate human-support message. The follow-up join
still sends a one-time confirmation, never subscribes immediately. Existing help
cooldowns, automatic-sender guards and envelope-only reply targeting remain.

Verified with `cargo test --locked -p listmngr-runners --lib help_reply_reaches_command_bot_and_sends_confirmation_not_owner_mail -- --nocapture`
and `cargo test --locked -p listmngr-mail -p listmngr-db -p listmngr-runners --all-targets`
(253 passed, 0 failed, 14 ignored). This uses the existing disposable SQLite and
SMTP sink fixtures, not a live MTA or PostgreSQL. See `P3-HELP-REPLY` in
`docs/FEATURE_PARITY.md` for exact lint commands and evidence logs.

The shared core command type, migration 0012 help cooldown, notice provenance and
post-lock/final-ACK clock fencing are integrated with the existing Out + Digest +
Archive fanout. Commands do not enter posting fanout. Original mailbox spelling,
private-header cooking and the immutable Phase 1 schema fixture are preserved.

**Standalone live email-only acceptance is BLOCKED: authorization was denied.**
The denied `live_email_commands.py` harness and equivalent standalone workflows
were not run. Unit/DB/runner fixture tests are regression evidence, not that
acceptance. Fresh permission is required; UI integration and whole-Mailman
replacement acceptance remain separate. See the current entry in
[FEATURE_PARITY.md](docs/FEATURE_PARITY.md) for gate commands and logs.

Replacement implementation is tracked in [docs/MAILMAN_REPLACEMENT.md](docs/MAILMAN_REPLACEMENT.md).
Subsequent lease-fencing integration now passes fresh workspace build/tests/Clippy
and the expanded isolated PostgreSQL gate. Archive/digest completion rechecks
expiry after publication waits; explicit fixture clocks remain supported.
This supersedes the lease-candidate exclusion above, not the UI/email-only or
whole-product acceptance boundaries. See the lease follow-up in the evidence ledger.
The historical checkpoint above is not acceptance of ongoing changes. Operator
recovery now includes `queue recipients JOB` and `queue resolve JOB EMAIL
--outcome sent|failed|retry --reason REASON`. Only ambiguous recipients of inactive
jobs may be resolved. Retry additionally requires `--acknowledge-duplicate-risk`:
check the relay first, because an unknown SMTP result may already have delivered.
The resolution and audit event commit together; known-sent recipients are not reset.

Integrated repairs now return LMTP 451 for transient dependency errors and commit
multi-recipient intake atomically. Outbound mail sanitizes private headers,
applies anonymous identity, and preserves validated cross-list loop history.
Anonymous mode does not anonymize body or attachment content. Bounded tokens
cannot modify shared global identities, and legacy admin scopes respect bounds.

Lists persist `default_member_action` and `default_nonmember_action` overrides
(`null` inherits site defaults). `legacy-announce` now defaults to moderation;
explicit member overrides retain precedence. Owner/moderator-only addresses can
post unless the ban/loop/emergency checks intervene. Configuration PATCH reserves
the writer before reading so lock waits cannot restore stale unrelated fields.
The preceding migration-0004 paragraph is historical, not the status of these fixes.
## Browser self-service (experimental)

Account subscriptions and the moderator list index are SQL-paginated at 20
records per page, with next/previous links. Verified ownership and role filters
apply before pagination; ordinary membership does not reveal moderator queues.
Preferences and moderation writes now revalidate session/ownership/role in the
business transaction after conflicting revocations finish. Parent verification
passed API/DB tests, Clippy, Chromium and an isolated PostgreSQL authority matrix.
Password login now binds the exact verified password hash/version and verified
address ownership to session issuance under the same DML-conflicting transaction.
Argon2 runs before acquiring writer locks. Issuance rechecks the predecessor's
expiry/CSRF/revocation, then rotates and audits atomically. Deterministic reset
RED→GREEN and SQLite/PostgreSQL login-lock matrices passed. PostgreSQL still uses
coarse table locks: throughput remains an open production boundary. The integrated
held-review intent resolves recipients inside that lock and atomically schedules
Out, Digest and policy-enabled Archive children. See the canonical convergence
entry in the parity ledger; prior donor gate results below are historical.

## Mail the site sends outside any list (`P4-SITE-NOTICES`)

Address verification and password reset mail belong to no list, so they leave
as **site notices**: `From:` and the Message-ID domain are `site.site_owner`,
templates resolve at the site scope then the built-in (`site:user:action:verify`,
`site:user:action:reset`, in `en` and `vi`), the subject comes from the shared
catalog, and the out runner signs for the owner's domain when `[[mta.dkim_signing]]`
has a key for it. Like every generated notice they leave with a null reverse
path, carry no `List-*` header and bind no list delivery authority. Producers
enqueue inside their own transaction (`Database::site_notices().enqueue_tx`),
so a business write and its mail commit together.

```toml
[site]
name = "Example Lists"                  # $site_name in site notices
site_owner = "postmaster@lists.example.com"  # From: of site notices; DKIM domain
```

Signup verification (`P4-ACCOUNT-SIGNUP`) and password reset (`P4-ACCOUNT-RESET`) are the producers.

## API reference without a CDN (`P1-API-DOCS-ORIGIN`)

`/api/docs` is a server-rendered reference generated from the same OpenAPI value
`/openapi.json` returns: every documented path, its methods, summary, required
scopes, parameters and response codes. It loads nothing from another origin,
contains no script, and carries the same strict CSP as the browser surface.

It is deliberately not an interactive explorer: point your own client (Swagger
UI, Redoc, Bruno, Postman, `openapi-generator`) at `/openapi.json` when you want
to issue requests from a UI. Embedding one would have meant either executing
third-party JavaScript in the operator's origin or vendoring megabytes of it.

## Browser shell (`P4-SHELL`, bounded local acceptance verified)

Every browser page now renders from one Askama template set in `crates/web`
(`templates/base.html` plus one template per page); no handler builds markup.
Auto-escaping is compile-time and writes the same named character references as
before, so the pages are byte-compatible with the previous output.

- **Language.** The shell and every page string come from the shared Fluent
  catalog (`crates/i18n/locales/{en,vi}.ftl`). Each request negotiates the
  document language from the browser's ordered `Accept-Language` preferences,
  then `site.default_language`, then English, and stamps it on `<html lang>`.
  A signed-in reader's profile language (`/web/account/profile`) takes
  precedence over the browser. Only `en` and `vi` ship.
- **Appearance.** `crates/web/assets/style.css` holds design tokens that both
  colour schemes bind; dark mode follows `prefers-color-scheme` and needs no
  per-page markup.
- **Accessibility.** Landmarks, a skip link, visible focus and
  `aria-current="page"` on the navigation item being read. The Chromium
  acceptance runs axe-core over four representative pages and fails on any
  `critical` or `serious` violation.
- **Scripts.** htmx 2.0.10 is vendored (0BSD, `crates/web/assets/README.md`
  records its provenance and SHA-384) and served from this origin at
  `/web/htmx.min.js`. No page loads it yet, so every page keeps
  `default-src 'none'` and contains no `<script>` element. Nothing on the
  browser surface loads a third-party origin.

Open `/web` on the same `serve` process after migrating the database. The browser
router is separate from Bearer/compatibility API authentication; do not paste API
tokens into browser forms. Configure `site.base_url` to the exact browser origin
(including its port). HTTPS is required except for explicit loopback development,
for example `LISTMNGR__SITE__BASE_URL=http://127.0.0.1:8000`. Keep public proxy access
logs free of query strings: confirmation tokens may be entered in a URL or form.

- `/web`: advertised list directory, 20 lists/page; each public list offers join
  and leave **requests**, not immediate membership changes.
- Copy the token from the durable confirmation email into the list confirmation
  form. Opening the form never consumes a token; submitting it does. Delivery
  requires the existing configured mail worker/relay; a queued notice is not a
  receipt. Unknown/ineligible requests receive the same generic response.
- `/web/signup` (on by default; `web.signup = false` removes the route and the
  link): a visitor creates an account with a password. The account cannot sign
  in until the address is proven: a single-use token, stored only as a SHA-256
  digest and valid for 24 hours, is mailed as a site notice from
  `site.site_owner`, and `/web/verify` (form or `?token=` prefill) consumes it.
  The response is the same whether or not the address already had an account:
  a verified account is neither recreated, changed nor mailed; an account that
  was never proven belongs to whoever proves the address, so a repeat signup
  re-arms it with the new name and password; at most one verification mail
  per address per hour. Password strength, name and mailbox validation are the
  same as everywhere else, and the whole write — account, credential, token,
  mail — is one audited transaction (`user.signup`, then `address.verify`).
- `/web/reset` and `/web/reset/confirm`: password reset by mailbox proof. The
  request page accepts any address and answers the same way; only a verified
  address of an account with a password gets a single-use, 24-hour token
  mailed as a site notice (at most one per address per hour, in the account's
  own language). Confirming with the token and a new password — checked for
  strength before the token is touched, so a refused attempt does not burn it
  — replaces the credential, ends every session of the account and audits
  `user.password` with the cause. Both forms use the shared pre-auth rate
  bucket, Origin and CSRF.
- `/web/login`: password login for an existing account with a verified, linked
  address. Trusted administrators may still use
  `POST /api/v1/addresses/{email}/verify` **only after establishing mailbox
  ownership**.
- `/web/account`: your verified-address member subscriptions, delivery-mode and
  enabled/self-paused preference forms, and POST logout. Moderator/bounce-disabled
  delivery cannot be re-enabled here.
- `/web/account/profile`: display name, interface language and IANA time zone.
  The interface language a reader chooses wins over the browser's
  `Accept-Language` on every page they open while signed in; it does not change
  the language of list notices, which stays a subscription preference. One
  validator serves this form and the REST user patch: names are non-empty,
  at most 256 characters and free of control characters; the language must be
  a shipped catalog (`en`, `vi`); the zone must be an IANA name.
- `/web/account/addresses`: the account's addresses, primary first. Adding one
  links it unverified and mails a token (the same `/web/verify` flow as
  signup); an address another account owns links nothing and mails nothing,
  and the page looks the same either way. Any verified address can become
  primary and can be used to log in. Removing an address unlinks it and
  forgets its verification — whoever claims it next must prove it again — but
  keeps the address row and its list subscriptions, which still deliver to
  that mailbox. The primary address and the last verified address cannot be
  removed. Every write is one audited transaction (`address.add`,
  `address.primary`, `address.remove`).
- `/web/account/tokens`: the account's API tokens — name, scopes, binding,
  expiry, last use, revoke — and a form to mint one. Scope follows authority,
  because the API authorizes on the token alone: a list owner may only choose
  list-level scopes (`lists:*`, `members:*`, `moderation`) and must bind the
  token to a list they own; a server owner may choose any scope and leave it
  unbound. The secret is shown once, on the page that follows creation, and
  never stored. Revocation takes effect at once. Changing or resetting the
  password does not revoke tokens.
- `/web/account/totp`: two-step sign-in with a time-based one-time password
  (RFC 6238: SHA-1, six digits, 30-second steps, one step of drift either
  way). Enrolment shows a base32 secret, an `otpauth://` URI and an inline QR
  code rendered on the server; the first correct code confirms it and shows
  ten single-use recovery codes once (stored as SHA-256 digests). Afterwards
  the password alone yields a pending session that can do nothing but the
  second step at `/web/login/totp`; the right code (each accepted once) or a
  recovery code rotates it into a signed-in session, and five wrong codes end
  it. Regenerating recovery codes and turning two-step sign-in off need the
  password. `security.require_2fa_for = ["server_owner"]` (the default) keeps
  a server owner who has not enrolled out of list administration, moderation
  and token minting until they do; the account page says so. Only
  `"server_owner"` is an accepted value.
- `/web/account/passkeys`: WebAuthn passkeys. Registration asks the
  authenticator for a discoverable credential with user verification; the
  public key and signature counter are stored in `webauthn_rp`'s binary
  encoding, the ceremony a browser is in the middle of lives on its session,
  and a response for another origin or a stale challenge is refused. "Sign in
  with a passkey" on the login page is passwordless and needs no further
  step — user verification on the device is the second factor — so a passkey
  also satisfies `security.require_2fa_for`. Removal needs the password. These
  two pages are the only ones that load a script: `/web/passkeys.js`,
  first-party, with a CSP of `script-src 'self'; connect-src 'self'`; every
  form still works without it, and the button is hidden until it runs. The
  relying-party id is the host of `site.base_url`, which must therefore be a
  domain name (`localhost` works for development), not an IP address.
- `/web/login/oidc/<name>` and `/web/account/oidc`: sign in through an
  OpenID Connect provider. Each `[[web.oidc]]` entry names one provider
  (`name` is a slug used in the paths, `display_name` is what the login page
  shows, `issuer` is the provider's issuer URL — `https://accounts.google.com`
  for Google, and any provider publishing
  `/.well-known/openid-configuration` works the same way — with `client_id`
  and either `client_secret` or `client_secret_file`). The flow is
  Authorization Code with PKCE (S256), `state` and `nonce`, the redirect URI is
  `<site.base_url>/web/login/oidc/<name>/callback`, and the ID token is
  verified against the provider's JWKS (RS256 or ES256) for issuer, audience,
  expiry and nonce. A provider identity signs in the account it is linked to;
  otherwise its email — trusted only when the provider asserts
  `email_verified` — links it to the account that has verified that address,
  or creates an account just in time with a random, unusable password and
  that address verified. An unverified email is refused and creates nothing.
  Signing in through a provider replaces the password step only: an account
  with two-step sign-in enabled still lands on `/web/login/totp`, and
  `security.require_2fa_for` applies as usual. The account page links or
  unlinks each configured provider; unlinking asks for the password when the
  account has a usable one, and the only link on an account with neither a
  usable password nor a passkey is refused as the last way in — set a
  password through `/web/reset` first. Register the callback URL with the
  provider exactly as above; GitHub is not an OpenID Connect provider (OAuth2
  only) and is not supported.

  ```toml
  [[web.oidc]]
  name = "google"
  display_name = "Google"
  issuer = "https://accounts.google.com"
  client_id = "1234567890-abc.apps.googleusercontent.com"
  client_secret_file = "/run/secrets/listmngr-google-client-secret"  # mode 0600
  ```

  The secret never appears in logs or a dumped configuration (it is redacted
  like the SMTP credentials).

- `/web/account/delete`: delete the account after entering the password
  again. One audited transaction ends every membership held by the account or
  any of its addresses (each with its own `member.delete` audit event, so list
  owners can see why a member vanished), removes the addresses and their
  pending tokens, the API tokens, the credential, the sessions and any domain
  ownership, unlinks the account from moderation history, and deletes the
  user row. Posted messages stay in archives; the audit log keeps the actor id
  as text. The last server owner cannot delete themselves.
- `/web/account/sessions`: every browser currently signed in to the account,
  with when it was issued and when it expires, the one making the request
  marked. A reader can end one session or every other session; each revocation
  is one audited transaction. Sessions are addressed by an opaque id — the token
  digest never reaches the page. Changing the password still ends all of them.
- `/web/moderation`: server owners or verified linked list owners/moderators can
  review held mail. Queues show 20 messages/page and at most 64 KiB of escaped
  source per message, bounded in SQL. Accept creates a real outgoing job and
  recipient snapshot; defer keeps held; reject publishes a guarded author notice
  and discard stays silent. Acceptance retains canonical archive scheduling and
  cooking policy; an archive browsing/search UX is not implemented.

Upgrading past migration `0043` signs every browser out once: the migration
deletes existing session rows rather than backfilling them with an id derived
from a credential digest. Sessions are short-lived (eight hours authenticated),
so this costs one login.

`security.rate_limit.login` (default `5/min`) is now the bucket every browser
password check draws from — login, password change, reset confirmation,
deletion, and the second-factor management forms; it was previously fixed at
five a minute regardless of configuration.

Sessions use opaque random credentials, hashed storage, independent CSRF secrets,
rotation at login, server-side logout revocation and password-version checks.
Cookies are HttpOnly, SameSite=Strict, Path=/web, and Secure on HTTPS. Authenticated
sessions expire after eight hours; anonymous forms after thirty minutes. Every
form POST requires both a session-bound CSRF secret and the exact configured
Origin. The `strict-origin` referrer policy strips paths/queries while retaining
Chromium's same-origin form Origin; accepting `Origin: null` is not a workaround.

Focused login-race verification (executed from `/tmp/listmngr-webui`):

```sh
CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo test -p listmngr-db --lib browser_login_issuance -- --nocapture
CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo clippy -p listmngr-db -p listmngr-api --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo test -p listmngr-api -p listmngr-db
# Opt-in: WEBUI_LOGIN_POSTGRES_URL must name a NEW empty disposable database.
WEBUI_LOGIN_POSTGRES_URL=postgres://webui_test@127.0.0.1:56439/webui_login_issuance_20260906 CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo test -p listmngr-db --lib browser_login_issuance_postgres_lock_matrix -- --ignored --nocapture
```

The PostgreSQL database above was created only after verifying the worker-owned
cluster's `data_directory`, and dropped after PASS; recreate a fresh owned database
before rerunning. Login regression source is `crates/db/src/web_login_tests.rs`
and `crates/db/src/web_sessions_tests.rs`. Default focused run: 3 passed, 1 opt-in
ignored; explicit PostgreSQL run: 1 passed (10 cases). Existing API browser suite:
14 passed, 4 opt-in ignored; no new Chromium run is implied by these commands.

Other browser verification:

```sh
cargo test --locked -p listmngr-api --test webui --test phase0_security
# Real Chromium render + real server + disposable in-memory DB assertions:
python3 -m venv /tmp/listmngr-webui-browser-venv
/tmp/listmngr-webui-browser-venv/bin/pip install playwright==1.55.0
/tmp/listmngr-webui-browser-venv/bin/playwright install chromium
# axe-core is development-only and is passed in, not vendored; without
# WEBUI_AXE_SCRIPT the run prints AXE SKIPPED and scans nothing:
curl -sSo /tmp/axe.min.js https://cdn.jsdelivr.net/npm/axe-core@4.13.0/axe.min.js
WEBUI_BROWSER_PYTHON=/tmp/listmngr-webui-browser-venv/bin/python \
WEBUI_BROWSER_SCRIPT="$PWD/scripts/test-webui-browser.py" \
WEBUI_BROWSER_OUTPUT=/tmp/listmngr-webui-browser-evidence \
WEBUI_AXE_SCRIPT=/tmp/axe.min.js \
  cargo test --locked -p listmngr-api --test webui chromium_browser_acceptance -- --ignored --nocapture
# The phone-viewport acceptance journey, with Lighthouse accessibility:
npm install --no-audit --no-fund --prefix /tmp/listmngr-lighthouse lighthouse@13.4.1
WEBUI_BROWSER_PYTHON=/tmp/listmngr-webui-browser-venv/bin/python \
WEBUI_JOURNEY_SCRIPT="$PWD/scripts/test-webui-journey.py" \
WEBUI_BROWSER_OUTPUT=/tmp/listmngr-webui-journey-evidence \
WEBUI_AXE_SCRIPT=/tmp/axe.min.js \
WEBUI_LIGHTHOUSE=/tmp/listmngr-lighthouse/node_modules/.bin/lighthouse \
  cargo test --locked -p listmngr-api --test webui chromium_acceptance_journey -- --ignored --nocapture
```

Alternatively set `WEBUI_CHROMIUM_EXECUTABLE` to an installed Chromium/Chrome
binary. The harness captures credential-free desktop/mobile screenshots, checks
browser console errors, and verifies persistent preference, membership, moderation
queue effects and logout. It reads a confirmation notice from its disposable DB,
**not from SMTP**. Its one-time token bridge is removed after the probe. PostgreSQL
has a separate opt-in probe requiring a **new empty disposable**
`WEBUI_POSTGRES_URL`: `cargo test --locked -p listmngr-api --test webui
postgres_browser_session_forms_and_bounded_preview -- --ignored --nocapture`.
These bounded checks do not establish full Phase 4/Mailman parity or production
security acceptance; see the current browser boundary in the parity ledger.

## Prerequisites

- Rust 1.88.0 (see `rust-toolchain.toml`)
- Docker with Compose v2 for the PostgreSQL/container path

## Locked build and local tests

```sh
cargo fmt --all --check
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
scripts/check-phase0-artifacts.sh
python3 -m unittest discover -s scripts/tests -v
python3 scripts/check-production-crates.py
```

The clippy gate is intentionally blocking in CI. A local failure is not evidence that another gate failed. PostgreSQL acceptance includes router-level scoped-user authorization, not just repository CRUD; token boundaries are exercised with both visible and forbidden users.

Phase 1 additionally requires the live PostgreSQL gate and the real Python compatibility flow. `scripts/test-postgres.sh` must receive a disposable PostgreSQL URL through `TEST_POSTGRES_URL`; `tests/compat/mailmanclient_phase1.py` must run against a live `/3.1` server with `mailmanclient==3.3.5`. Exact commands and evidence boundaries are in `docs/FEATURE_PARITY.md`.

Both backends run in CI (`P3-DUAL-BACKEND-CI`): the ordinary workspace suite
is the SQLite suite; `scripts/test-postgres.sh` is the chosen PostgreSQL
contract set; `scripts/test-postgres-all.sh` runs every `#[ignore]`d
PostgreSQL test in the workspace (53 today) on the same disposable server.
Each such test creates and drops its own schema through
`listmngr_db::test_support::IsolatedSchema`, so the server only has to be
disposable, never empty, and the former per-test `WEBUI_*_POSTGRES_URL` /
`ARCHIVE_THREAD_POSTGRES_URL` databases are gone. The three ignored tests
that need other fixtures (a browser, `postmap`) are skipped by name.

To run the client flow without configuring a development server, install `tests/compat/requirements-mailmanclient.txt` in a Python virtual environment, then run `python3 scripts/test-mailmanclient.py` after the locked build. The harness starts a loopback server with a fresh temporary SQLite database, creates fixture-only credentials, runs the real client, and stops the server on success or failure. It never reads `.env` or uses your configured database. The PostgreSQL gate separately runs both live CRUD and semantic schema contracts. These probes and the anti-stub check are blocking CI steps; local success is not a hosted CI run.

## Run against PostgreSQL from the host

`.env` is **not loaded automatically** by the Rust process. The example contains only disposable local-development values:

```sh
cp .env.example .env
set -a
. ./.env
set +a
# Start only the database; the Rust process itself remains on the host.
docker compose --env-file .env -f deploy/docker-compose.yml up -d postgres
export LISTMNGR__DATABASE__URL="$LISTMNGR_HOST_DATABASE_URL"
cargo run --locked -p listmngr -- migrate
cargo run --locked -p listmngr -- serve
```

Then, from another shell:

```sh
curl --fail --show-error http://127.0.0.1:8000/healthz
curl --fail --show-error http://127.0.0.1:8000/readyz
```

`127.0.0.1` is correct for host-side commands. The hostname `postgres` is a Docker Compose network name and does not resolve on the host.

## Run with Docker Compose

Build context must be the repository root; the Compose file already resolves it correctly.

```sh
cp .env.example .env
# Replace POSTGRES_PASSWORD in .env; never commit .env.
docker compose --env-file .env -f deploy/docker-compose.yml config
docker compose --env-file .env -f deploy/docker-compose.yml up -d --build --wait
curl --fail --show-error http://127.0.0.1:8000/healthz
curl --fail --show-error http://127.0.0.1:8000/readyz
docker compose --env-file .env -f deploy/docker-compose.yml down
```

For a direct image build, use `docker build -f deploy/Dockerfile -t listmngr:dev .`; `docker build deploy` is invalid because it omits workspace manifests. The runtime image is a static musl binary in `scratch`, runs as UID/GID 1000, and uses `listmngr status` for its tool-free healthcheck.

The builder installs exact-version musl C headers required by `ring` and SQLite; see `deploy/README.md`. Ordinary `down` preserves PostgreSQL data. Use `down -v` only for a deliberately disposable test project, never as a routine production shutdown.

## CLI passwords, status, and exit codes

`user create` and `user passwd` prompt for a hidden password by default. For
automation use `--password-stdin` or, on Unix, `--password-fd FD`; the two options
are mutually exclusive. The old `--password VALUE` option is rejected: passwords
must not enter process arguments or shell history. For example:

```sh
# Interactive, hidden prompt:
listmngr user create owner@example.com --display-name Owner --server-owner
# Automation: the input file must be protected and contain only the password.
USER_ID='replace-with-the-user-uuid'
listmngr user passwd "$USER_ID" --password-stdin < /protected/path/password
# Unix inherited descriptor, without putting the secret in argv:
listmngr user passwd "$USER_ID" --password-fd 3 3< /protected/path/password
```

Input is UTF-8, limited to 1,024 password bytes by the shared password policy.
One final LF or CRLF is removed from stdin/FD input; oversized input is rejected,
not silently truncated. Password strength checks still apply. An issued API token
is printed once to stdout; keep that output out of logs.

`listmngr status` probes `/healthz` and then `/readyz` on `web.listen`, without
opening its own database connection. Wildcard IPv4/IPv6 addresses are mapped to
their loopback equivalents. Each HTTP request has a two-second timeout; environment
proxies and redirects are disabled. Start `serve` first: a reachable database alone
does not make a stopped HTTP service healthy. `members find` and `members del`
validate and normalize complete email addresses, including IDNA domains.

| Exit | Meaning                                                      |
|------|--------------------------------------------------------------|
| 0    | Success                                                      |
| 1    | Unexpected internal failure                                  |
| 2    | Invalid command line, input, or configuration                |
| 3    | HTTP status endpoint unreachable or timed out                |
| 4    | `/healthz` returned a non-success status                     |
| 5    | Healthy process, but `/readyz` returned a non-success status |
| 6    | Resource conflict                                            |
| 7    | Resource not found                                           |
| 8    | Authentication, authorization, or rate-limit rejection       |
| 9    | Input/output failure                                         |
| 10   | Database connection, query, or migration failure             |
| 11   | A Mailman site being imported could not be read              |

Runtime errors emit a stable `error[CLI-…]` category and a correlation UUID,
without raw error chains, input values, or database credentials. Usage errors are
also redacted; use `--help` for command syntax.

## Durable queue tools (Phase 2 foundation)

Run `listmngr migrate` before using the queue commands. Intake currently stores
the original bytes in the database and atomically creates a submission, an inbound
job, and an audit event. The injection command itself does not send mail or start a listener; an independently running enabled mail role can consume the job.

```sh
listmngr queue inject dev.example.com ./message.eml --sender alice@example.com
listmngr queue ls --queue in
listmngr queue show JOB_UUID
# Raw mail is only emitted when explicitly requested. Protect the exported file.
listmngr queue show JOB_UUID --raw > /protected/path/message.eml
```

Injection requires an existing list, a valid envelope sender, and one supported
`Message-ID` header. The intake bound is 10 MiB. Metadata parsing currently accepts
modern dot-atom Message-IDs, not the full obsolete RFC syntax. Duplicate
Message-IDs do not discard distinct submissions or overwrite their bodies.
Queue listing returns at most 1,000 records in ID order, including retained jobs.
The hash in submission routing metadata is the Mailman archive identifier;
blob identity separately uses SHA-256 of the exact raw bytes.

Mailman's `/queues` resource (`P2-QUEUES-REST`) is on both prefixes:
`GET /queues` lists the twelve runner queues in Mailman's order, each with
`directory` (`queue_jobs/<name>`: jobs are database rows, not files), `files`
(the ids of the jobs still ready or leased, oldest first, at most 1000) and
`count`; `GET /queues/{name}` is one queue and `GET /queues/{name}/{id}` one
job's metadata (state, attempts, `run_after`, last error — never the raw
message). `POST /queues/{name}` is mailmanclient's `Queue.inject(list_id,
text)`: only `in` accepts it; the text is the complete message, its `From`
mailbox becomes the envelope sender, `Message-ID` is required, line ends are
normalised to CRLF, the 10 MiB intake bound applies, and the answer is `201`
with the job's location. Reading queues needs `system:read` on a token bound
to no list or domain; injecting needs `lists:write` within the token's list
bounds. Injection stores the job like the CLI; only a running mail role
consumes it.

An opt-in mail role is now wired into `serve` when `mta.enabled` is true.
Keep deployment MTA snippets disabled while the remaining acceptance and
operational review obligations are open. The standalone filesystem-store library is not selected by
CLI intake; filesystem/DB lifecycle and garbage collection integration remain
future work. See the Phase 2 evidence boundary in `docs/FEATURE_PARITY.md`.

## Experimental mail role and held-message REST

The mail role is disabled by default. Enabling `mta.enabled` requires either
`mta.smtp_tls = "required"` (verified STARTTLS, described above) or explicit
`plaintext_trusted_relay` for an isolated trusted relay. Opportunistic/unknown
modes fail closed. AUTH PLAIN requires verified required TLS; implicit TLS is not implemented. Keep deployment
MTA snippets disabled pending full acceptance and operational review.

`serve` connects the LMTP session library, durable database intake, pure inbound
posting policy, and inbound/outbound workers with lease renewal and shutdown
supervision. Held REST under `/api/v1` and `/3.1` supports read/count and
accept/reject/discard/defer with authorization, pending-state fencing, persisted
comments, and transactional user/token/peer-IP audit attribution. Reject records
a disposition and publishes a guarded author notice; automatic posting-policy
rejections still send no notice.

Mailman's `forward` (`P2-HELD-FORWARD`) is on the same `POST`: `forward=True`
with `forward_to=<mailbox>` (Postorius's form spelling; JSON booleans work too)
sends a copy of the held post, wrapped as `message/rfc822` under
"Forward of moderated message", from the list's `-bounces` address to that one
mailbox, in the same transaction as the decision — with `defer` the post stays
held, with `accept`/`reject`/`discard` the decision applies as before. The
address is validated first (a mailbox, not one of the list's own addresses),
so a bad `forward_to` refuses the whole request and nothing changes;
`forward` without `forward_to` is `400`; `forward=False` ignores `forward_to`.
The moderation log row and the `moderation.*` audit event carry `forward_to`.

Before SMTP commands, `begin_delivery` commits selected recipients as
ambiguous/in-flight with an owning attempt token and `queue.delivery_begin` audit.
TCP connection establishment may precede that commit. `finish_delivery` resolves
owned reservations and finalizes the job atomically. Known transient results
restore pending/retry; omitted reserved results, cancellation, or outcome/audit
rollback leave uncertainty quarantined and excluded from automatic retry. Missing
*unreserved* outcomes remain pending. A done job is not proof that all recipients
were sent mail. Even never-sent attempts can require manual reconciliation; no
operator resolution command/UI or exactly-once SMTP guarantee is provided. The
SMTP client returns the final DATA result without waiting for QUIT.

Focused SQLite real-TCP sink, failed-audit, cancellation, pool-reopen/reclaim, and
mixed-outcome tests cover this bounded O2/O3 repair. Full current PostgreSQL
verification remains blocked by the attempt-gate timeout, and R1/O1 remain open.
No DKIM/DMARC/ARC, bounce processing, digests, subscription workflows, archive,
administration UI, or Mailman migration is claimed.

The pinned client harness runs both Phase 1 and real LMTP nonmember → held REST
→ SMTP flows against a disposable SQLite-backed binary. The parent reran this
gate successfully on the migration-0004 candidate before committing; this does
not close PostgreSQL acceptance or R1/O1. To rerun after the locked build:

```sh
uv run --with-requirements tests/compat/requirements-mailmanclient.txt python scripts/test-mailmanclient.py
```

The maintained helper is `scripts/mailmanclient_held.py`. The harness checks held
count/list/get/properties/raw preview, defer comments, scope denial, unsupported
options, accept/replay with one subscriber delivery, one private rejection notice,
and silent discard. The updated real-client helper has not been rerun for the
rejection-notice candidate; current evidence is the DB/REST and owned SMTP tests
listed in `P3-MODERATOR-REJECTION-NOTICE`, not full Mailman compatibility.

## Configuration and deployment

Configuration is TOML plus `LISTMNGR__SECTION__KEY` environment overrides. Prefer `database.url_file` or a root-readable environment file in production; `conf` output redacts credentials. See:

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for crate, process, data, and deployment boundaries;
- [`deploy/README.md`](deploy/README.md) for Compose, systemd, and intentionally disabled MTA snippets;
- [`docs/SECURITY.md`](docs/SECURITY.md) and [`security.txt`](security.txt) for the threat model and private reporting path;
- [`docs/PLAN.md`](docs/PLAN.md) for the canonical roadmap and acceptance IDs.

## License and versioning

All workspace packages are version `0.1.0`; no released tag is implied. The project is licensed under GNU Affero General Public License v3 or later. The complete license is in [`LICENSE`](LICENSE); rationale is in ADR-0003.


## P4-BOUNCE-WEB-RECOVERY — verified-session behavior

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
Initial DB/router RED/GREEN provenance is in `target/web-bounce-recovery-handoff.md`;
later security tests are supplemental, not retroactive per-guard TDD. Final
54-gate acceptance and coverage boundaries are recorded above and in
`target/web-bounce-recovery-final-receipt.json`.

### Opt-in durable DSN issuance

Ordinary singleton deliveries can now issue audited durable RFC3461 ENVIDs with
`mta.dsn_issuance_enabled=true`, a protected issuer key and actual relay DSN
capability. Default is off; null/internal notices and digest deliveries are
excluded. See [DSN issuance](docs/DSN_ISSUANCE.md) for producer incarnation
binding, retry/ambiguity and rotation limits. No incoming DSN scorer is enabled.
