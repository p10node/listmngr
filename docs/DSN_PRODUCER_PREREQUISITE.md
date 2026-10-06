# Outbound DSN producer prerequisite

Historical prerequisite receipt. The implemented outbound carrier is now described
in [DSN_ISSUANCE.md](DSN_ISSUANCE.md); the original no-token checkpoint below is
retained as historical evidence, not the current runtime inventory.

Acceptance ID: `P2-DSN-PRODUCER-PREREQUISITE`.
Status: bounded local transport-prerequisite acceptance verified.
**This is not an authenticated DSN carrier or a Mailman-replacement completion.**

## Bounded implementation

```toml
[mta]
# Mail-role enablement and explicit supported TLS policy remain separate.
smtp_single_recipient = false
```

When explicitly true, the real outbound runner sends non-null list envelopes
(including digest deliveries) as sequential one-recipient SMTP sessions. Each
session establishes its own connection and uses the same configured TLS/AUTH
policy. The current ordinary `<list-bounces@host>` sender remains unchanged.
Omission/false retains the existing multi-recipient session. Private workflow
and owner mail with `MAIL FROM:<>` retain batching even when enabled.

The message is prepared and signed once; every recipient receives the same DATA
bytes. The flag does not personalize MIME, issue tokens, add ENVID/NOTIFY/ORCPT,
change aliases or authenticate returned original headers. Increased connection,
TLS/AUTH and DATA transmission costs are deliberate; this is not a throughput
optimization or connection-pooling feature.

Before the first SMTP command, the existing lease-fenced, atomically audited
`begin_delivery` reserves the **whole pending set**. Per-session results retain
recipient order and are completed together through the existing
`finish_delivery_with_smtp`. A failed later connection/greeting is a known
transient for that session, not a reason to resend an earlier accepted recipient.
Lost final DATA acknowledgement stays ambiguous; only known transient recipients
are eligible for automatic retry. The existing runtime heartbeat wraps the whole
operation; this adds no separate lease/clock policy.

Cancellation or failed final durable completion conservatively leaves **every
reserved recipient ambiguous**, including already accepted and not-yet-attempted
sessions. There is no incremental per-session commit. That deliberately trades
retry liveness for existing no-blind-replay safety. Reopening SQLite and claiming
the expired job does not send those recipients again. Operator ambiguity recovery
remains an explicit existing action, not an automatic guarantee of no duplicates.

## Why no token yet

Inspection covered `docs/PLAN.md`, `docs/MAILMAN_REPLACEMENT.md`, `CLAUDE.md`,
core configuration/secret handling, outbound cooking/signing/transport,
`mail_queue::begin_delivery`/`finish_delivery_with_smtp`, SMTP bounce persistence
and `docs/SECURITY.md`. Existing attempt tokens authorize a queue lease; existing
bounce events retain direct SMTP failure metadata. Neither is a durable DSN
issuance ledger with expiry and list/membership incarnation authority.

Rather than invent an in-memory HMAC issuer or trust mutable stored context as a
new capability, this increment takes the explicitly permitted smaller,
consumer-integrated prerequisite. No secret config, HMAC helper, token validation
route, migration or automatic incoming DSN mutation is added. Existing direct
configured-relay SMTP-failure scoring is unchanged and is not incoming DSN scoring.

## Exact remaining producer and consumer work

1. Define a versioned, domain-separated carrier and durable audited issuance
   authority committed before SMTP. Bind delivery identity, authoritative list
   identity/incarnation, exact canonical recipient identity and membership
   incarnation where later scoring requires it, issued-at/expiry, nonce and key
   identifier. A list/address deleted and recreated under the same spelling must
   not inherit old authority. Resolve context from authoritative stored
   job-to-message/provenance associations, not public mutable lease metadata or
   returned MIME claims.
2. Fence issuance with the current lease and post-lock live-clock checks; make
   retries/restarts idempotent for the same issuance. Define expiry, rotation,
   retention/spool cleanup and backup/restore behavior, and an explicit policy
   for tokens issued before a send that never completes. A reservation alone
   does not establish that the remote relay accepted the message.
3. Load protected bounded keys without logging secrets; use reviewed HMAC,
   constant-time verification and unambiguous bounded encoding. Enforce SMTP
   local-part/address limits without embedding recipient plaintext or silently
   truncating claims. Exercise each binding independently, wrong keys, malformed
   tokens, expiry, recreated identities and audit rollback on both DB engines.
4. Put issued carrier bytes on actual outgoing SMTP envelopes, demonstrate
   stable replay after restart and distinct per-recipient/list/delivery tokens,
   and preserve null-envelope exclusions. No incoming token route or automatic
   scorer should be enabled before durable issuance authority is established.
5. Separately implement bounded intake/routing, durable issuance lookup,
   duplicate/replay policy and carefully authorized scoring. **HMAC proves local
   issuance, not reporting-MTA identity or truth of its failure claim.** Token
   possession alone does not authenticate a DSN sender. Real Postfix/Exim
   routing, external-MTA interoperability and production cutover remain open.

## Executed focused evidence

All fixtures use loopback listeners, in-memory SQLite or an owned `tempfile`
SQLite file. No development `data/`, `.env`, existing secrets or deployment was
used. The temporary database pool is explicitly closed before directory cleanup.
No unrelated process was terminated; no commit or push was made.

Evidence directory: `target/dsn-producer-prerequisite/` (local, not committed).

- `red-envelope.log`: `cargo test --locked -p listmngr-runners --lib single_recipient_config_reaches_real_smtp_envelopes -- --nocapture` failed **behaviorally** before implementation: enabled mode sent `RCPT TO:<b@example.invalid>` where the real sink required `DATA` after recipient A. Disabled batching had already passed in that test.
- `green-envelope.log`: the same command passed after config→runtime→transport implementation, with the exact fixture EHLO/MAIL/RCPT/DATA lines from the socket.
- `green-boundaries.log`: initial five focused tests passed; default/strict bool, unchanged null sender, mixed DATA outcomes/selective retry and cancellation/SQLite reopen were supplementary regression tests (first run GREEN, not retrospective RED evidence).
- `impacted-tests.log`: `cargo test --locked -p listmngr-core -p listmngr-mail -p listmngr-runners --all-targets` passed **218/0/2** (passed/failed/ignored), 36 test binaries. This run contains the initial five new tests; the later reconnect/greeting regression was added while this broader run was executing and is covered by the final focused command below. No production behavior changed during the broader run.
- `final-focused.log`: `cargo test --locked -p listmngr-runners --lib single_recipient -- --nocapture --test-threads=1` passed **6/0/0**, including the later-session greeting loss and refused reconnect with prior `Sent` preservation. Sequential execution makes socket envelope evidence readable without interleaving.
- `clippy.log` and final `final-clippy.log`: `cargo clippy --locked -p listmngr-core -p listmngr-mail -p listmngr-runners --all-targets --all-features -- -D warnings` passed (exit 0), including all final test code.
- `cargo fmt --all --check` and `git diff --check` passed (exit 0). Final documentation-only edits are separately whitespace-checked at handoff.

These are focused worker gates, not a fresh full-workspace/PostgreSQL acceptance
receipt. The two ignored tests remain ignored; they are not counted as passes.

New tests assert actual ordinary and null MAIL FROM values, exact RCPT multiplicity,
identical payload bytes and persisted recipient outcomes. The null-sender test
uses the same private transport consumer directly; existing owner/workflow tests
supply provenance regression coverage, not a newly claimed end-to-end notice
publication matrix. The new socket fixtures use trusted plaintext, not a new
singleton TLS/AUTH interoperability matrix. No new PostgreSQL singleton/restart
execution or browser→LMTP cutover is claimed by the worker. Parent composed
verification below is separate from these focused worker results.

## Frozen parent acceptance

Final run: `target/single-recipient-gates-20260910-015656/`.
`final.json` records `pass=true` and `source_stable=true`; all **23/23** commands
returned zero, with **375** source/documentation/oracle paths unchanged.
Workspace tests passed **604/0/43**; mandatory PostgreSQL passed **26/0/0**.
Build, format, strict workspace Clippy, artifact/diff checks, online `cargo deny
check`, online `cargo audit --ignore RUSTSEC-2023-0071`, real mailmanclient,
Chromium and inherited emergency/Unicode process regressions passed. Ignored
tests are not counted as passes.

The independent `target/single-recipient-parent-process.py` runs the production
CLI/server, creates membership through the API, injects actual LMTP and captures
SMTP on an owned loopback relay. On **each** SQLite/PostgreSQL fixture:

- Omitted default and explicit false each produce one session with three RCPTs
  and one DATA transaction for two accepted recipients.
- True produces three singleton sessions and two DATA transactions; the third
  recipient receives RCPT550 and no DATA.
- MAIL FROM, exact recipient spelling, body bytes, Message-ID and durable
  sent/failed rows agree. The stored failure is RCPT550 and remains unprocessed;
  transport isolation does not enable scoring.
- Real service stop/start persists the database while changing the flag.
  Cumulative captures, including a final shutdown check, exclude old-message
  replay. The PostgreSQL cluster/database is created exclusively for this gate
  and is stopped and removed afterward; no development database is used.

This new parent tracer uses the API directly, not browser configuration of the
flag. It uses trusted plaintext, not a new singleton TLS/AUTH interoperability
matrix. Worker future-cancellation/SQLite-reopen evidence remains distinct from
these graceful executable restart cases. Neither proves complete MTA cutover.

Source-bound read-only review of the four changed Rust files and this contract
found no introduced P1/P2. Full verdict:
a local review note (`subagent-summary-0-20260909_180131_371152.txt`), not kept in the repository.

The earlier `target/single-recipient-gates-20260909-180039/` remains **failed**, not
acceptance: incremental dependency-graph IO failure, advisory-network failures
and browser timeouts. The affected DB target and emergency browser tracer later
passed unchanged, and GitHub DNS/Git access recovered. The final full run passed
without source fixes, timeout increases, cache deletion or offline advisories;
the original intermittent cause was not conclusively established.

Only documentation is updated after the frozen gate; post-documentation checks
and source/hash reconciliation are recorded separately at
`target/single-recipient-final-closure.json`. No HMAC or incoming authority is
claimed by any of this evidence.
