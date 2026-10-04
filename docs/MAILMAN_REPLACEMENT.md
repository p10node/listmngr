# Mailman replacement execution and acceptance

> **Historical — superseded.** This file records the 2026-09 checkpoints of the
> mail path. The "open scope" it describes was closed row by row in
> `docs/FEATURE_PARITY.md` (Phases 2–7); `1.0.0` was released on 2026-10-04.
> It is kept for the record and is not a current obligation list.

## Current outbound carrier prerequisite (not authenticated DSN completion)

`P2-DSN-PRODUCER-PREREQUISITE` adds a real default-off single-recipient SMTP
session policy for non-null list envelopes. It deliberately takes the smaller
consumer-integrated path: current queue attempt reservations are not durable
DSN issuance authority. No HMAC token or token intake is introduced, and incoming
DSNs remain untrusted/read-only. The next producer vertical still needs a durable
audited issuance ledger, list/member incarnation binding, expiry/key lifecycle,
lease-fenced idempotent issuance and actual authenticated envelope evidence.
Bounded local acceptance passed 23/23 gates, workspace604/0/43 and mandatory
PostgreSQL26/0/0, with three native process/SMTP modes per database. See
[DSN_PRODUCER_PREREQUISITE.md](DSN_PRODUCER_PREREQUISITE.md) for exact evidence
and conservative cancellation behavior. Whole replacement stays open.

## Current emergency moderation control

`P4-WEB-EMERGENCY` exposes the existing hold policy to list owners on the web.
The completed frozen run passed 17/17 gates, workspace575/0/41 and mandatory
PostgreSQL24/0/0; six actual browser→LMTP→held/SMTP cases per database cover
default-off, enabling, sibling survival, enabled/disabled restart and retaining
old holds after disabling. See [WEB_EMERGENCY.md](WEB_EMERGENCY.md). This is
neither an outgoing delivery shutdown nor completion of the obligations below.

## Current owner lifecycle-notice controls

`P4-WEB-NOTICES` exposes the existing welcome/goodbye policies in native owner
settings without changing publication, templates or transport defaults. The
frozen run passed 17/17 gates, workspace574/0/40 and PostgreSQL23/0/0; both
databases passed actual browser configuration→membership APIs→private SMTP,
including sibling survival and enabled/disabled restart. See
[WEB_NOTICE_CONTROLS.md](WEB_NOTICE_CONTROLS.md). Remaining full-replacement
obligations below are not closed by this increment.

## Current owner posting limits and list-copy closure

`P4-WEB-POSTING-LIMITS` closes the owner browser configuration gap for size and
To/Cc holds, using the existing policy and atomic audited authority path. The
frozen 17-gate run passed workspace571/0/40, PostgreSQL23/0/0 and 12 actual
browser→LMTP→held/SMTP differential cases per database, including restart and
unaffected sibling lists. See [WEB_POSTING_LIMITS.md](WEB_POSTING_LIMITS.md).

The prior `P4-LIST-COPY` frozen run completed successfully before interruption:
63/63 gates, workspace568/0/39, PostgreSQL22/0/0, both-backend browser/SMTP and
restart. Its 349 hashes were matched before this increment. This supersedes the
pending parent-acceptance handoff, not the full-replacement obligations below.

Neither increment supplies authenticated incoming DSN/VERP, incoming sender
authentication/ARC, token/probe recovery, remaining templates/localization,
account-security parity, incumbent migration or real-MTA production cutover.
No deployment or complete Mailman-replacement claim is made.

## Current subscriber own-post preference (bounded local acceptance verified)

`P4-WEB-OWN-POSTINGS` exposes the existing own-post recipient policy through
My subscriptions: explicit Yes/No, effective layered rendering, optional-field
compatibility and atomic attributed audit under existing authority/restrictions.
Frozen `target/web-own-postings-parent-gates-20260909-090105/` passed **58/58 gates**:
workspace561/0/37, mandatory PostgreSQL20/0/0, actual Chromium→SMTP on both engines
with suppression/restoration, peer and same-identity sibling delivery, restart
in both states, omission and negative controls. All 339 source/harness paths
remained stable; independent bounded review found no causal P1/P2.
Receipt: `target/web-own-postings-final-receipt.json`. Final acceptance prose is
checked separately. RUSTSEC-2023-0071 remains a disclosed audit exception.
No new sender authentication, exhaustive contention/expiry coverage, digest
semantics, inheritance-reset control, live deployment or whole replacement claim.

## Previous verified-session bounce recovery checkpoint (bounded local acceptance verified)

`P4-BOUNCE-WEB-RECOVERY` lets an existing verified account restore its own directly
bounce-disabled subscription through a native web confirmation. Current identity,
subscription mode, session and CSRF/Origin are checked; reset and one attributed
audit commit atomically without changing delivery mode or historical events.
General preference editing cannot bypass this explicit action or other disabled
states. The UI asks the user to check mailbox health; it sends no new challenge.

Frozen `target/web-bounce-recovery-parent-gates-20260909-072736/` passed 54/54 gates:
workspace558/0/36, mandatory PostgreSQL19/0/0; actual Chromium confirmation and
restored fresh SMTP delivery after active scheduler/restart on both engines,
with foreign/revoked/CSRF/Origin/generic-form denials and unaffected controls.
Build/fmt/strict Clippy/client/browser/DKIM/TLS/AUTH/security PASS with the documented
advisory exception; 334/334 files stable; bounded independent review no P1/P2.
Receipt `target/web-bounce-recovery-final-receipt.json`; final acceptance prose
checked separately. Original behavior REDs and supplemental security tests have
distinct chronology. SQLite contention coverage does not prove PostgreSQL-specific
recovery interleavings, audit/COMMIT-wait expiry or same-identity multi-role/list
preservation through a dedicated executable fixture. Published warnings remain.

Token/probe/email recovery, incoming authentication/DSN/VERP, remaining lifecycle/
localization and migration/real-MTA cutover remain open. No deployment, full P4 or
whole-replacement claim. Historical no-web-recovery wording is superseded only
for the new verified-session confirmation.

## Previous opt-in scheduled maintenance (bounded local acceptance verified)

`P3-BOUNCE-SCHEDULER` adds an owned child to the real mail role; explicit global
activation is separate from mail transport and per-list processing. Defaults
off/60-second completion-based page delay/100-member page preserve legacy
behavior. Bounds validate even disabled; first page waits, cursor progresses
over non-due/failed members, empty pages wrap and page errors retry later.
Shutdown cancels owned work without pretending to roll back earlier commits.
Enable on one selected instance; no leader election is provided.

Frozen `target/bounce-scheduler-parent-gates-20260909-063657/` passed 50/50 gates:
workspace554/0/35, mandatory PostgreSQL18/0/0 and six real no-CLI scheduler cases
on each backend. Actual client configuration and LMTP-triggered bounce disable
lead to automatic warning/removal; held notices survive graceful restart with
exact SMTP bytes, audit/no-loop and healthy/restored recipient checks. All
build/fmt/strict Clippy/client/browser/DKIM/TLS/AUTH/security gates passed with
the documented advisory exception. Source/harness stable; bounded independent
review no P1/P2. Receipt `target/bounce-scheduler-final-receipt.json`; acceptance
prose checked separately. Tests do not directly establish held-SQL-transaction
cancellation or certainty about a commit whose acknowledgement was lost.

Probes, web/email token recovery, incoming authentication/DSN/VERP, remaining
lifecycle/localization and migration/real-MTA cutover remain open. No production
deployment or whole-replacement claim. Historical manual-only/no-scheduler
exclusions below are superseded only for this explicit opt-in path.

## Previous warning/removal maintenance (bounded local acceptance verified)

`P3-BOUNCE-MAINTENANCE` supplies an actual operator `bounce sweep` command with
bounded keyset pages, private partial-failure summaries, configurable warning
count/interval and independent owner-removal notification. Warning publication,
cycle state, authoritative member deletion, optional goodbye and audits share
per-member transactions. This is an explicit operation, not a scheduler in `serve`.
Frozen `target/bounce-maintenance-parent-gates-20260909-054944/` passed 46/46 gates:
workspace 546/0/35, mandatory PostgreSQL 18/0/0, and seven actual client→LMTP→SMTP→
CLI→restart cases per engine. These include manual REST restoration, a warning
rejected while membership exists, and goodbye/admin rejection without bounce
loops. All build/fmt/strict Clippy/client/browser/DKIM/TLS/AUTH/security gates
passed with the documented advisory exception. Source/harness stable; independent
bounded static review no P1/P2. Receipt `target/bounce-maintenance-final-receipt.json`.
Acceptance wording is post-run documentation checked separately. Unattended
maintenance, probes, web/email recovery, incoming authentication/DSN/VERP,
remaining lifecycle/localization and migration/real-MTA cutover stay open.
No production deployment or whole-replacement claim. Historical warning/removal
exclusions below are superseded only by this explicit operator-maintenance scope.

## Previous early bounce notification increment (bounded local acceptance verified)

`P3-BOUNCE-INCREMENT-NOTICE` adds default-false owner/moderator notices on fresh
eligible scoring days, including pre-reset score at threshold. Both notification
flags are independent; processing remains default-off. Audit, score, optional
disable and private durable notices commit together; null sender prevents loops.
Frozen `target/bounce-increment-parent-gates-20260908-203556/` passed 42/42 gates:
workspace 531/0/34; mandatory PostgreSQL 17/0/0; nine actual client→LMTP→SMTP→restart
cases on each engine; build/fmt/strict Clippy, browser/client/DKIM/TLS/AUTH/security
with the documented advisory exception. Source/harness stable; bounded independent
static review no P1/P2. Receipt: `target/bounce-increment-final-receipt.json`.
Acceptance wording is post-run documentation, checked separately. Warning/probe/
recovery, incoming authentication/DSN/VERP, lifecycle gaps and migration/real-MTA
cutover remain open. No production deployment or whole-replacement claim.

## Previous owner-disable notification increment (bounded local acceptance verified)

`P3-BOUNCE-DISABLE-NOTICE` adds default-true owner/moderator notices under the
default-off scorer, recipient deduplication, atomic private durable publication,
zero-admin auditing and null-envelope/no-loop transport. Actual mailmanclient
administrative role calls work without implicitly verifying their addresses.
Frozen run `target/bounce-notice-parent-gates-20260908-195604/` passed all 38 gates:
workspace 524/0/33, mandatory PostgreSQL 16/0/0, real client→LMTP→SMTP→restart on
both engines, build/fmt/strict Clippy, client/browser/DKIM/TLS/AUTH and security
with the documented advisory exception. Source/harness stable; limited static
review found no concrete P1/P2. Receipt: `target/bounce-notice-final-receipt.json`.
Acceptance wording is a post-run documentation change with separate checks.
Warnings/probes/recovery, incoming authentication/DSN/VERP, remaining lifecycle
and migration/real-MTA cutover remain open. This is not whole replacement.

## Previous threshold suspension increment (bounded local acceptance verified)

`P3-BOUNCE-DISABLE` implements configurable positive fractional thresholds
(default5), fresh-day RCPT-triggered Member suspension/reset and continued
delivery to healthy subscribers. Final frozen-source process probes passed
on SQLite and PostgreSQL across restart; stale reset 1→1 and both preference
race findings have behavioral RED→GREEN coverage. All 34 gates passed:
workspace 518/0/32, PostgreSQL 15/0/0, strict Clippy, build/fmt, security with
the documented advisory exception, client/browser/TLS/AUTH/DKIM. Receipts:
`target/bounce-disable-parent-gates-20260908-164331/`. This is bounded local
acceptance, not universal concurrency safety or production cutover. Notifications,
warning/probe scheduling, re-enable/removal, VERP and authenticated DSNs are not
implemented by this increment; the full replacement goal remains open.

## Previous direct SMTP bounce score increment (bounded local acceptance verified)

`P3-DIRECT-BOUNCE-SCORE` adds default-off scoring for new direct configured-relay
permanent RCPT failures only. It includes durable daily deduplication, stale reset,
current-member isolation and immutable API reads. It does not disable delivery,
authenticate incoming DSNs, implement VERP or run warning/probe workflows.
Canonical `process_bounces` and Mailman whole-day config strings are exercised
through actual pinned mailmanclient settings calls, not a substitute raw-HTTP
setter. Final frozen-candidate gates passed: workspace **512/0/30**, PostgreSQL
**13/0/0**, all 30 recorded gates and real HTTP/client→LMTP→SMTP→restart scoring
probes on both databases. The audit exception remains explicit. See the parity
ledger and `target/bounce-score-parent-gates-20260908-112513/` for receipts.
This is bounded progress toward replacement, not completion of the product.

This is the implementation ledger for the owner's request to turn listmngr into
an alternative to the Mailman stack. It does not narrow `PLAN.md`, declare an MVP
sufficient, or equate the completion of a repair wave with product acceptance.
The source-backed audit at base `38a92db` established real administrative CRUD,
LMTP/DB queue/moderation/SMTP plumbing, but **not replacement readiness**.

## Current goodbye increment (bounded local acceptance verified)

Optional durable `send_goodbye_message` now covers actual Member removals,
including administrative/mass/sync, confirmed and authenticated browser leaves,
and list teardown. It is default-off, bounded built-in private MIME, not a custom
template/localization or general per-request override implementation. Parent's
real mailmanclient probe found and repaired the missing address-based DELETE
alias; only explicit pre-confirmed/pre-approved administrative calls are supported
there. Independent SQLite and owned PostgreSQL listening HTTP → stopped spool →
process restart → SMTP passed. Final workspace **505 passed / 0 failed / 30 ignored**,
mandatory PostgreSQL **13 passed / 0 failed / 0 ignored**, real TLS/AUTH,
mailmanclient/Chromium/DKIM and build/security gates passed with the documented
audit exception and unchanged source/harness fingerprints. See `P3-GOODBYE` for
the complete rerun receipt, prior failure history and scope.

Bounce/VERP scoring-disable-probe, incoming sender authentication, remaining
lifecycle/invitation notices, incumbent migration and real MTA/cutover acceptance
remain open. No replacement-readiness claim follows from this slice.

## Prior SMTP AUTH increment (bounded local acceptance verified)

AUTH PLAIN now runs only after REQUIRED verified TLS, through real core config,
runner, durable delivery and the shared SMTP envelope/DATA path. Credentials are
bounded/control-free, secret-file capable and redacted in config/Debug/errors.
The owned runner fixture proves exact synthetic AUTH and message bytes; negative
AUTH controls remain pending/retry without envelope or mailbox bounce. Sequential
RED→GREEN evidence and limitations are in P2-SMTP-AUTH and the target handoff.
No LOGIN/XOAUTH2, implicit/opportunistic TLS or production relay cutover is added.
The current parent candidate passed 495 workspace tests (30 ignored), 13 explicit
PostgreSQL tests, the independent OpenSSL real-process AUTH and secret-file
matrices, client/browser/DKIM/security gates. Long credential command negotiation
also has a parent RFC4954 RED→GREEN correction. See P2-SMTP-AUTH for receipts and
the coordinator timeout/continuation provenance. The following 486/13 gates are
historical. Production relay cutover and full replacement readiness remain open.

## Prior REQUIRED STARTTLS candidate (bounded local acceptance verified)

The bounded outbound transport now supports explicit `required` with verified
TLS 1.2/1.3, bundled public roots and optional private CA-file additions, validated
DNS/IP identity and no plaintext fallback. The existing numeric relay endpoint
contract is retained. Owned runner/TCP/TLS and independent OpenSSL real-process
exact-message/no-mail/no-bounce fixtures pass. Parent combined acceptance covers
486 workspace tests, 13 PostgreSQL tests, client/browser/security/DKIM gates and
the repaired DB/DKIM retry classification. See `P2-STARTTLS` in FEATURE_PARITY
for exact evidence and the recorded orchestration/preflight repair. Prior
480-test acceptance is historical, not this candidate's total.
At that checkpoint SMTP AUTH was absent; opportunistic/implicit TLS, inbound TLS, live Postfix/Exim cutover and
full replacement readiness remain open. Historical assertions below that all
STARTTLS is absent are superseded only for this required outbound slice.

## Completion rules

### Latest source-backed operational increment

Visible recipient moderation is implemented across persisted config, REST and
inbound hold, with Mailman's `count >= limit` boundary and zero disabled.
Malformed headers fail closed when enabled. Parent review also reproduced and
repaired an outbound DKIM trailing-whitespace body hash defect using RFC simple
body canonicalization, without changing MIME bytes. Fresh whole-candidate gates
pass: workspace 480/0/30, PostgreSQL 13/0/0, real PostgreSQL HTTP/LMTP/SMTP
recipient tracer, mailmanclient, Chromium and seven independently verified SMTP
signing profiles. See the current `FEATURE_PARITY.md` section for commands,
negative controls and evidence. This is not replacement readiness. Historical
DKIM retry artifacts referenced below are absent and that retry is not running.

Outbound DKIM is locally verified across ordinary, owner, private rejection and
digest delivery. Seven fresh TCP SMTP fixture captures independently verify with
dkimpy 1.1.8, including eight rejection controls per capture. The former pending
DKIM checkpoint below is historical, superseded by this fresh run. This does not add
incoming authentication, trusted bounce attribution, STARTTLS/AUTH or live MTA
cutover. HTTP completion receipts and configurable welcome messages were accepted
in later slices than the historical email-only checkpoint below.

### Prior operational checkpoints

Email join/leave confirmation gained a durable private completion
receipt to the token-stored mailbox, atomically with membership/token/audit/ACK.
At that checkpoint HTTP confirmations and configurable welcome/goodbye were
unchanged; later `P3-WELCOME`/`P3-GOODBYE` supersede those notice gaps, while the wider
lifecycle remains open. See `P3-EMAIL-CONFIRM-RECEIPT` for that
candidate's evidence. The prior `P2-DMARC-MUNGE` slice provides opt-in individual
delivery From rewriting, not archive/digest author rewriting or DNS authentication.

Public/email join now enforces existing bans before challenge publication and at
confirmation, including bans added after a token was issued. Banned requests
remain generic, leave remains available, and ban writes share the workflow
reservation. Privileged member CRUD/imports and global-ban administration remain
unchanged. See `P3-SUBSCRIPTION-BAN-ADMISSION` for the latest candidate evidence;
the increments below are earlier evidence, not current workspace totals.

Explicit moderator rejection now atomically publishes a guarded private author
notice through the existing outgoing runner; discard stays silent. Owned SQLite
and SMTP fixtures cover publication, audit rollback, replay, suppression and null
reverse-path delivery. Sender authentication, automatic policy/hold notices,
templates and full backscatter protection remain open. See
`P3-MODERATOR-REJECTION-NOTICE` for final-candidate evidence.

List-scoped posting bans now have REST create/list/delete on both prefixes,
bounded Rust-regex validation, canonical exact-mailbox matching and atomic
contextual audit. Fixture consumers prove suppression of all posting children
and restored fanout after deletion. Global-ban administration and automatic
policy-rejection notices remain absent; see `P3-LIST-POSTING-BANS` for
that increment's gates. Per-list size moderation is also available as the
separately bounded `P2-MESSAGE-SIZE` control.

Experimental owner routing now implements intake → transactional outgoing
handoff to owners plus moderators, without subscriber/archive/digest fanout.
Job-bound provenance controls private-header sanitization and null-sender
outgoing preparation; missing/unsafe rosters are shunted. The complete
configurable owner chain, notices lifecycle and real transport acceptance remain
open. See `P2-OWNER-FORWARD` for current test scope; earlier results below remain
historical evidence rather than the latest candidate's totals.

`aliases regen` now publishes explicit Postfix map generations, and actual
`postmap` lookup agrees with the LMTP recipient handler on the tested positive
and negative matrix. See `POSTFIX_MAPS.md` and the current `P2-MTA-MAPS` evidence
in `FEATURE_PARITY.md`. This does not close deployment or replacement readiness.

Highest-impact transport gaps retained from source review:

- Outbound posts/digests use `ListId::bounces_address()` in
  `crates/runners/src/outbound.rs`; the inbound resolver now durably retains bare
  bounce-address reports in a separate inbox, readable through CLI queue commands.
  The mail role still has no automatic bounce consumer or cleanup. Authenticated VERP,
  DSN processing and disable/probe policy remain a release blocker.
- `crates/mail/src/smtp.rs` implements plaintext delivery to an explicitly
  trusted relay, not SMTP STARTTLS/AUTH. Signing/authentication and DMARC/ARC
  obligations remain open; map lookup cannot establish deliverability.
- Full owner-chain policy, the notice lifecycle, actual MTA cutover and the
  remaining migration/client/browser obligations are not closed by owner forwarding.
- The former short-lease heartbeat workspace failure is addressed by separating
  virtual-time scheduler checks from actual SQLite authority/runtime controls,
  without widening production timeouts. That earlier full workspace run passed:
  375 passed, 0 failed, 30 ignored. Build, format, strict Clippy, deny, audit and
  actual Postfix map lookup also passed. See `P2-LEASE-HEARTBEAT`; this is local
  candidate evidence, not PostgreSQL or complete product acceptance.

### Standing rules

- One behavior at a time: failing regression, implementation, focused GREEN,
  integration, independent review, final executable acceptance.
- Preserve raw MIME bytes where required, privacy, atomic audit/business writes,
  lease fencing, unknown-outcome quarantine, and scoped identity authorization.
- Disposable fixtures only. Never mutate development or production data to test.
- No commit, push, live mail activation, or production migration is authorized by
  this ledger. The owner requested implementation, not these external actions.
- A green suite covers implemented assertions, not missing product workflows.
- README, ARCHITECTURE, FEATURE_PARITY must distinguish static implementation,
  focused tests, composed runtime evidence, and whole-product acceptance.

## Work ownership

Initial safety workers use detached worktrees based on the audit commit. Parent
owns canonical integration, shared documentation and all final gates. Worker
patches remain uncommitted. No worker may edit the canonical checkout or another
worker's checkout; no nested implementation processes are permitted.

| Slice | Initial owner | State |
|---|---|---|
| Header privacy, anonymous identity, duplicate List-Post loop handling | mail-safety worker | integrated; focused SMTP/loop tests pass |
| Typed LMTP recipient failures, batch-intake transaction/cancellation | lmtp-safety worker | integrated; typed, rollback, timeout tests pass |
| Global identity mutation bounds, address relink consistency | identity-safety worker | integrated; repository/API tests pass; parent also fixed legacy admin bounds |
| Ambiguous recipient inspection and explicit audited resolution | parent | focused verification |
| Shared docs, patch composition, PostgreSQL/MTA/browser acceptance | parent | pending |

## Full replacement obligations

None of the following rows is closed by the presence of a file or route. Preserve
all remaining detailed requirements of PLAN.md when refining these slices.

| Behavior | Required positive / negative / durable evidence | State |
|---|---|---|
| Mail privacy and anonymous lists | SMTP bytes conceal author/control headers; ordinary mode differential; MIME retained | in progress |
| Intake correctness | valid vs missing vs DB fault RCPT; batch timeout rollback and per-recipient reply accounting | in progress |
| Loop suppression | cooked mail round trip with multiple/folded/case-varied markers never redistributes | in progress |
| Identity authorization | shared user across lists cannot grant global mutation; relink/unlink and audit rollback | in progress |
| Queue operations | inspect ambiguous; explicit duplicate-risk recovery; stale worker cannot overwrite; audit rollback | in progress |
| Per-list posting policy | announcement owner/member differential; bans/header predicates/emergency policy | pending |
| Subscription and unsubscription | pending confirmation, expiry, single-use token, approval/invitation and email/API completion | pending |
| Email commands | join/leave/request/confirm/owner routing; robust command parsing; no command loops | pending |
| System notices | hold/reject/welcome/goodbye/invite/probe; durable outbox; no backscatter to forged/untrusted senders | partial: guarded rejection, challenge/completion and opt-in welcome/goodbye notices; remaining invitation/probe/admin notices, custom templates and sender-authenticated anti-backscatter still pending |
| Bounce handling | authenticated VERP + DSN fixtures; scoring, stale reset, warn/disable/remove; spoof rejection | pending |
| Digest delivery | regular/MIME/RFC1153 differential; threshold/periodic scheduling; restart/idempotence | pending |
| MIME policy and templates | part/depth limits, filtering, footers/personalization; scoped template resolution | pending |
| Transport and mail authentication | SMTP TLS/AUTH, DKIM verification of emitted bytes, DMARC/ARC; fail closed on negotiation | pending |
| Archive persistence | accepted-only ingest, thread/reference IDs, dedup, never/private policy, restart | pending |
| Archive user workflows | read/search/thread/attachments/mbox import/export; safe rendering and membership authorization | pending |
| Account/session security | signup/verify/login/reset, CSRF/session rotation/revocation, TOTP/WebAuthn/OIDC per contract | pending |
| Administration UI | list/domain/member/policy/moderation/templates/bans/account/system screens; browser acceptance | pending |
| Subscriber UI | discover/subscribe/confirm/preferences/leave; archive navigation; mobile/accessibility | pending |
| REST interoperability | real pinned mailmanclient through user/address/preferences/pending/held and lifecycle; differential responses | pending |
| Migration | Mailman 2.1/3 import preserving identity, roles, preferences and archive; dry-run/idempotence/reconciliation | pending |
| Persistence lifecycle | concurrent field updates, list deletion with history, retention/GC, backups and isolated restore | pending |
| Deployment/operations | actual Postfix/Exim round trip, non-root/read-only image, Linux hardening, telemetry and failure diagnosis | pending |
| Final acceptance | frozen composed source, complete gates, hostile/fault probes, real clients and browsers | pending |

## Current parent-owned RED/GREEN evidence

Per-list posting defaults and the announcement preset now control the real inbound
queue transition (`cargo test --locked -p listmngr-runners --test announcement`,
2 passing tests after RED). Nullable defaults inherit site policy. Existing
announcement lists gain moderation defaults through migration 0008. A real
PostgreSQL lock-wait regression reproduced a stale config PATCH overwriting an
unrelated field, then passed after moving the read behind a writer reservation:
`cargo test --locked -p listmngr-db --test list_posting_settings postgres_waiting
-- --ignored --nocapture` (1 passing test in an isolated schema). SQLite policy
persistence, invalid values, clearing and audit rollback also pass.

The pure digest renderer has 3 passing MIME/parser tests. Durable confirmation,
digest and archive work is active in separate seeded worktrees; it has NOT yet
been composed or verified in the canonical tree. These statements refine the
open rows above, not close their full obligations. Latest canonical Clippy passes;
the API REST suite has 31 passes. Final full-tree/backend acceptance is still open.

- `cargo test --locked -p listmngr --test queue_recovery`: first RED with
  unrecognized `queue recipients`; then GREEN for outcome inspection.
- `cargo test --locked -p listmngr --test queue_recovery retry_requires`: RED
  before `queue resolve`; then GREEN for explicit retry acknowledgement and
  preserving a known-sent recipient.
- `cargo test --locked -p listmngr-db --test queue_operations`: RED for shunted
  recipient recovery; then GREEN including audit-failure rollback and confirmed
  sent/failed resolution without a replay.
- These are focused evidence only. Full final-candidate gates, independent review
  and PostgreSQL coverage of new behavior are not yet claimed.
