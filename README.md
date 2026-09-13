# listmngr

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

## Digest settings, RFC 1153 and volume rollover — bounded acceptance verified

Mailman's Digest settings are list settings now: `digests_enabled` (off, the
`to-digest` handler collects nothing), `digest_size_threshold` (KiB of
pending posts that trigger an issue; `0` never), `digest_send_periodic`
(send what is pending once it is a day old) and `digest_volume_frequency`
(`yearly`, `monthly`, `quarterly`, `weekly`, `daily`: a new calendar period
since the last issue advances the volume and restarts the issue numbers at 1,
audited as `digest.bump`).

The plain-text issue now follows RFC 1153 as Mailman writes it: the list's
`list:member:digest:masthead`, `Today's Topics:` with each subject and
author, the `list:member:digest:header`, each message under a numbered
`Message: N` block behind a line of thirty hyphens, the
`list:member:digest:footer` as `Subject: Digest Footer`, and `End of … Digest,
Vol X, Issue Y` with its underline. The MIME issue keeps the posts whole and
carries the same three templates as their own text parts (empty templates are
omitted). `summary_digests` remains Mailman's MIME alias. Templates are
resolved for the list's language with `$volume` and `$issue` added to the
usual placeholders. See `P3-DIGEST-SETTINGS` in `docs/FEATURE_PARITY.md`.

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

| Command | Effect |
|---|---|
| `echo TEXT` | replies with that text, unchanged |
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

| Policy | What happens |
|---|---|
| `open` | the roster changes at once; welcome/goodbye notices follow, no token |
| `confirm` | a confirmation mail; replying (or posting the token) applies the change |
| `moderate` | the request waits for a moderator; the requester gets no token |
| `confirm_then_moderate` | the address is confirmed first, then a moderator decides |

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

| Request | Result |
|---|---|
| `pre_verified`+`pre_confirmed`+`pre_approved` | `201` with the member |
| address not yet proven, or the list confirms | `202` `{token, token_owner: "subscriber"}` and a confirmation mail |
| confirmed but the list moderates | `202` `{token, token_owner: "moderator"}`, no mail |
| `invitation=true` | `202`, an invitation mail; accepting it subscribes, no moderator |

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

| Metric | Kind | Meaning |
|---|---|---|
| `listmngr_lmtp_recipients_total{result}` | counter | LMTP `RCPT` outcomes: `accepted`, `rejected`, `deferred` |
| `listmngr_posts_total{disposition}` | counter | in-runner decisions: `accepted`, `held`, `rejected`, `discarded`, `filtered`, `owner`, `command`, `failed` |
| `listmngr_delivery_recipients_total{result}` | counter | outgoing recipients: `sent`, `transient`, `permanent`, `ambiguous` |
| `listmngr_smtp_transactions_total{result}` | counter | relay transactions `completed` (DATA answered) or `failed` before DATA |
| `listmngr_smtp_transaction_seconds` | histogram | duration of one relay transaction |
| `listmngr_delivery_latency_seconds` | histogram | LMTP acceptance → relay accepting a recipient |
| `listmngr_queue_jobs{queue,state}` | gauge | jobs per queue and state (what `queue stats` prints) |
| `listmngr_queue_shunted_jobs`, `listmngr_queue_oldest_ready_age_seconds` | gauge | shunted jobs; how long the oldest ready job has waited |

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
MTA writes. See `P2-VALIDATE-AUTHENTICITY` in `docs/FEATURE_PARITY.md`.

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
and `site.default_language` that ships as a catalog (`en` and `vi` today;
regional tags such as `vi-VN` select `vi`, and an unsupported language falls
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

## Mailman handler pipeline — bounded acceptance verified

Accepted posts run the list's `posting_pipeline` (default
`default-posting-pipeline`): `member-recipients`, `cleanse`, `cleanse-dkim`,
`cook-headers`, `subject-prefix`, `rfc-2369`, `to-archive`, `to-digest`,
`dmarc`, `to-outgoing`. The `in` runner takes the fan-out decisions from the
pipeline; the archive, digest and delivery consumers each receive the message
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
`no-subject` and `suspicious-header` checks, a detour through the list's own
header rules, and accept. When several deferred checks hit, the single held
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
naming the row instead of ignoring it. Rows are managed through the repository
only for now; REST and browser surfaces are open work. See `P2-CHAIN-RULES` in
`docs/FEATURE_PARITY.md` for evidence and the full list of deliberate
deviations.

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

## Opt-in unconditional DMARC From rewriting (bounded)

Set list config through JSON or form `PATCH`/`PUT` on either REST prefix:

```json
{"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true}
```

Supported pairs are `no_mitigation` + `false` (default), `no_mitigation` +
`true` (inactive/pre-staged), and `munge_from` + `true`. `munge_from` + `false`
is rejected, including one-field PATCH transitions; PUT resets omitted settings.
Other actions (`wrap_message`, `reject`, `discard`) are rejected. This is **not
DNS-based conditional DMARC evaluation**, DKIM/ARC signing, or proof of delivery
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
Leave remains available subject to the existing cooldown. Posting and join share
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
- `/web/login`: password login for an existing account with a verified, linked
  address. Account creation/password administration remain CLI/API operations;
  trusted administrators may use `POST /api/v1/addresses/{email}/verify` **only
  after establishing mailbox ownership**. Browser signup/reset are not provided.
- `/web/account`: your verified-address member subscriptions, delivery-mode and
  enabled/self-paused preference forms, and POST logout. Moderator/bounce-disabled
  delivery cannot be re-enabled here.
- `/web/moderation`: server owners or verified linked list owners/moderators can
  review held mail. Queues show 20 messages/page and at most 64 KiB of escaped
  source per message, bounded in SQL. Accept creates a real outgoing job and
  recipient snapshot; defer keeps held; reject publishes a guarded author notice
  and discard stays silent. Acceptance retains canonical archive scheduling and
  cooking policy; an archive browsing/search UX is not implemented.

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
WEBUI_BROWSER_PYTHON=/tmp/listmngr-webui-browser-venv/bin/python \
WEBUI_BROWSER_SCRIPT="$PWD/scripts/test-webui-browser.py" \
WEBUI_BROWSER_OUTPUT=/tmp/listmngr-webui-browser-evidence \
  cargo test --locked -p listmngr-api --test webui chromium_browser_acceptance -- --ignored --nocapture
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

| Exit | Meaning |
|---|---|
| 0 | Success |
| 1 | Unexpected internal failure |
| 2 | Invalid command line, input, or configuration |
| 3 | HTTP status endpoint unreachable or timed out |
| 4 | `/healthz` returned a non-success status |
| 5 | Healthy process, but `/readyz` returned a non-success status |
| 6 | Resource conflict |
| 7 | Resource not found |
| 8 | Authentication, authorization, or rate-limit rejection |
| 9 | Input/output failure |
| 10 | Database connection, query, or migration failure |

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
comments, and transactional user/token/peer-IP audit attribution. Unsupported
forwarding fields/actions fail closed. Reject records a disposition and publishes
a guarded author notice; automatic posting-policy rejections still send no notice.

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
