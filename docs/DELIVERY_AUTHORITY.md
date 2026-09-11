# Shared queue delivery authority (P2-DELIVERY-AUTHORITY)

## Contract and boundaries

A leased queue operation can wait during its final audit insert. The shared
queue methods now sample the live clock after the final awaited write/audit and
before commit. Exact expiry (`now >= deadline`) returns Conflict and rolls back
all staged business/audit changes.

Covered methods: begin_delivery (ordinary and workflow notice), heartbeat,
claim (new grant), shared transition (ack/retry/shunt), complete_with_children.
The existing final check in finish_delivery_with_smtp_bounces remains covered.
Existing authority is read from the current queue row under its lock, not the
stale deadline copied into a caller's Lease. Heartbeat checks the resulting
monotonic renewed deadline; claim checks its new deadline. No changes to
unshunt, attempt limits, SKIP LOCKED, or SMTP bytes/TLS/AUTH policy.

The outbound runner uses the live-clock repository and returns before SMTP
commands if begin_delivery fails. A TCP connection can already have been opened;
this does not mean all network activity is prohibited before reservation.

This is a last pre-commit validation, not a guarantee that commit/fsync/network
latency completes before expiry. Atomic rollback is proven for rejection at the
validation boundary. As before, explicit-time callers without a configured clock
retain deterministic caller-time behavior; production runners use `.live()`.

## Focused execution (passed)

Five sequential RED→GREEN groups are retained in
`target/delivery-authority/{red,green}-{begin,transition,children,heartbeat,claim}.log`.
Parent commands after writer release and fixture hardening:

```sh
cargo fmt --all --check
cargo test --locked -p listmngr-db --test mail_queue_lock_clock
# TEST_POSTGRES_URL must point to an explicitly owned disposable fixture only.
cargo test --locked -p listmngr-db --test mail_queue_lock_clock \
  final_audit_postgres::postgres_final_audit_wait_fences_queue_authority \
  -- --ignored --exact --nocapture
cargo clippy --locked -p listmngr-db --all-targets --all-features -- -D warnings
```

Parent evidence: `target/delivery-authority/parent-{focused,pg-final,clippy-final}.log`.
SQLite: 10 passed, 0 failed, 2 PostgreSQL tests explicitly ignored. Focused
PostgreSQL: 1 passed, 0 failed, 0 ignored; 20 observed audit-wait cases covering
10 operations with exact-expiry and still-valid controls. Each rejection checks
queue row, recipient state, child/recipient/audit counts and rollback authority.
A stale Lease after successful heartbeat still uses the renewed DB deadline.

The PostgreSQL test owns an isolated schema. A trigger blocks the final relevant
audit on an advisory lock. The observer waits for that actual server-side wait,
then advances the injected clock and releases the blocker. Its key is the held
connection's backend PID, and the observer also requires the current database
and a granted lock on the fixture schema's queue_jobs relation. UUIDv7 timestamp
prefixes are not unique concurrent-fixture keys. No sleep-based expiry oracle.
The parent used the worker-owned PostgreSQL 17 container, never a development
DB. An initial native libpq initdb attempt lacked the postgres server executable;
that failed setup is not a PostgreSQL test result.

Source review found no introduced P1/P2 in the shared-method production changes.
The reviewer did not run tests. The parent subsequently strengthened only the
PostgreSQL observer's key/database/schema binding and reran focused tests and
strict Clippy. This test-only delta is distinct from the earlier review snapshot.

## Composed evidence

Accepted frozen run: `target/delivery-authority-gates-20260910-024940/`.
Driver: `target/delivery-authority-gates.py`.

- 23/23 command exits are zero; the complete named matrix is required.
- Workspace: 611 passed, 0 failed, 44 explicitly ignored.
- Mandatory PostgreSQL: 27 passed, 0 failed, 0 ignored, including the exact new
  final-audit test (not a zero-test filtered invocation).
- 378 source/harness paths match before/after hashes and the parent's readback.
- Artifact gate, fmt, locked build, full workspace tests, strict all-feature
  workspace Clippy, deny and audit pass. The documented RUSTSEC-2023-0071
  inactive SQLx/MySQL exception is unchanged.
- Native API→LMTP→SMTP/restart regressions pass on SQLite and owned PostgreSQL,
  including omitted/enabled/disabled recipient isolation, mixed outcomes and
  no old-message replay. Unicode, CLI DSN inspection, browser and pinned
  mailmanclient regressions pass. These existing transport regressions do not
  replace the separate DB-level adversarial audit-wait oracle.
- The native PostgreSQL fixture directory and the earlier owned PostgreSQL 17
  container are removed; no development database was used.

Evidence: `final.json`, `results.json`, `source-{before,after}.json`,
`workspace.log`, `postgres.log` and per-command logs in that run directory.
The final receipt explicitly requires complete_matrix, source_stable and
new_postgres_test_executed as well as zero command exits. Background-process
notifications or observer timeouts alone are not acceptance evidence.

Acceptance prose was updated after the frozen run, confined to README,
ARCHITECTURE, FEATURE_PARITY and this document; those post-gate documentation
bytes are not described as having been present during the frozen execution.

## Remaining scope

This does not implement DSN issuance, HMAC, authenticated incoming correlation,
replay protection or atomic incoming scoring/notices/ack. No token route is open.

The archive free-ACK caller and the digest/workflow/moderation transactions are
now covered by the separate producer increments below, not by the shared queue
suite alone. This does not establish authority for future producers automatically.
DSN issuance still needs
producer-bound list/subscription/recovery incarnations and a durable authority
ledger. Full Mailman replacement, migration and real-MTA cutover remain open.

## Archive producer increment (P2-ARCHIVE-AUTHORITY, bounded acceptance verified)

Archive captures the stored deadline under its existing queue lock and calls the
shared final-deadline validator after ACK's awaited audit, immediately before
commit. The free ACK helper is unchanged; shared queue behavior is unchanged.
List-before-queue ordering, explicit timestamps and production live clocks remain.
No schema change or other producer repair is included.

Evidence directory: `target/archive-final-audit/`. `red-sqlite.log` and
`red-postgres-retry.log` fail because expired completion returned `Ok(())`.
The initial `red-postgres.log` is fixture startup failure, not behavioral RED;
disabling Unix sockets allowed the long-path native fixture to start. First GREEN
attempts exposed a test expectation missing the existing `[audit]` subject prefix
and a test-only const-fn lint; production subject semantics were preserved.

Final commands/logs (all exits zero):

```sh
cargo fmt --all --check # fmt-final.log
cargo test --locked -p listmngr-db --test archive_final_audit \
  --test sibling_lease_clock --test mail_queue_lock_clock # focused-final.log
python3 target/archive-final-audit/pg-test.py # postgres-final.log
cargo clippy --locked -p listmngr-db --all-targets --all-features -- -D warnings
  # clippy-final.log
cargo test --locked -p listmngr-archive -p listmngr-runners --test archive
  # archive-final.log
```

The fixture runner uses `/opt/homebrew/opt/postgresql@14/bin`, owns a disposable
cluster and isolated schema, and shuts down/removes its cluster in `finally`.
The PostgreSQL test is registered in `scripts/test-postgres.sh` and observes eight
actual ACK-audit advisory-lock waits bound to blocker PID, current database and
fixture queue relation. No sleep-based expiry oracle. Exact expiry at original
and renewed deadlines rejects; valid and stale-Lease/renewed-DB controls succeed,
for public indexing and current `never` policy. Rollback compares full queue job,
stored archive row fields (including changed existing reply threads), audit/job/
recipient/message counts; valid retries publish expected cooked metadata.
SQLite deterministic resampling covers expiry, valid retry and stale renewal for
both policies. This is DB producer evidence, not new LMTP/SMTP/MTA acceptance or
a promise about commit/fsync finishing before expiry.

### Archive final composed acceptance

Frozen run: `target/archive-authority-gates-20260910-034840/`, driven by
`target/archive-authority-gates.py`. All 25 named gates passed; workspace612/0/45
and mandatory PostgreSQL28/0/0, including the exact archive audit-wait test.
All 382 source/harness paths matched before/after and parent readback. Build,
fmt, artifact gate, full workspace tests, strict all-feature workspace Clippy,
deny/audit, browser, pinned mailmanclient and existing mail regressions passed.
The RUSTSEC-2023-0071 inactive SQLx/MySQL audit exception is unchanged.

The native process fixture `target/archive-authority-process.py` exercises
API→LMTP→SMTP, service restart and public→never→public publication on SQLite and
owned PostgreSQL. Independent archive API/SQL reads retain the distinct earlier
body, exclude the never-policy post, and publish the later body. SMTP mixed
outcomes, original message bodies and cumulative no-replay controls remain.
This is not an expired-lease injection into a running service; that negative
boundary is exercised by the separate real PostgreSQL transaction fixture.

Read-only review `deleg_3cef8c0a` found no new concrete P1/P2 within its six-path
scope; final hashes matched all six reviewed paths. Reviewer execution did not
include tests, and its runner dependency inspection was explicitly limited.
Parent runtime gates supply the separate executed evidence, not broader static
review. The composed native PostgreSQL fixture was stopped and removed.

The gate's actual verdict AST passed 54 inert harness-unit controls for complete
success, each failed/missing gate, duplicate result, source drift and missing
new PostgreSQL execution. These controls are not application tests. Final
acceptance prose changes only README, ARCHITECTURE, FEATURE_PARITY and this file;
post-documentation closure is checked separately from the frozen run.

## Digest, workflow and moderation increments

`P2-DIGEST-DELIVERY-AUTHORITY`, `P2-WORKFLOW-DELIVERY-AUTHORITY` and
`P2-MODERATION-HOLD-DELIVERY-AUTHORITY` have bounded local acceptance.

- Digest collection preserves list-before-queue ordering and captures the current
  persisted deadline before ACK clears it. Its final fence follows ACK audit.
  Empty recipients and unleased digest publication/bump semantics are preserved.
- Workflow completion uses the common leased commit helper after the existing
  command reservation. Final expiry rolls back tokens, membership, notices and
  queue/audit effects, including no-child early returns. Unleased APIs are unchanged.
- Moderation hold ACKs before inserting the held record. Its final fence therefore
  follows the later `moderation.hold` audit, not only `queue.ack`. Unleased review
  transactions are unchanged.

Worker evidence: `target/aggregate-authority/HANDOFF.json` and
`target/workflow-moderation-authority/HANDOFF.json`; parent matched all nine changed
source/test hashes. Each producer has an observed SQLite behavioral RED before
its fix; digest also has a pre-fix PostgreSQL RED. Later matrices are supplemental
controls, not retroactive independent REDs. An intermediate DOUBLE snapshot decode
failure was a test-fixture issue, not an expiry regression.

The mandatory backend script executes `postgres::postgres_digest_final_audit_wait`,
`postgres::postgres_workflow_final_audit_wait` and
`postgres::postgres_moderation_final_audit_wait` in their respective integration
targets with `--ignored --exact --nocapture`. The final log contains 8 digest,
24 workflow and 8 moderation observed waits. All include exact-expiry rejection,
valid completion, stale snapshots after renewal and retry after rollback. Moderation
blocks both ACK and hold audits independently. Workflow/moderation snapshots compare
every column in relevant state tables; digest compares full posts/source job/settings
but only counts in other tables. Do not describe that as whole-database row equality.

### Final aggregate composed acceptance

Frozen run: `target/aggregate-authority-gates-20260910-045720/`; driver:
`target/aggregate-authority-gates.py`. Its receipt and all 25 zero-exit commands were
read back, with workspace615/0/48 and mandatory PostgreSQL31/0/0. All 390
source/harness hashes matched before/after and parent readback. Artifact gate,
locked fresh build, format, full workspace tests, strict all-feature workspace
Clippy, deny, audit, browser and pinned mailmanclient passed. The documented
RUSTSEC-2023-0071 inactive SQLx/MySQL exception remains unchanged.

The rebuilt binary ran `target/aggregate-authority-process.py` on SQLite and owned
PostgreSQL: two distinct posts collected via LMTP, service shutdown, CLI issue
publication, zero issues on duplicate publication, restart, three digest modes
and independent SMTP body/recipient readback. Existing archive public/never/public,
mixed SMTP outcomes and cumulative old-message no-replay controls remain. This is
positive runtime evidence; adversarial expiry remains a separate DB-layer probe.

Digest review `deleg_8a632849` and workflow/moderation review `deleg_f03334f8` found
no new P1/P2 in their bounded scopes; complete reports and final fingerprints were
read and matched. Reviewers did not execute tests. The actual final-verdict AST
passed 65 inert controls for complete success, failed/missing/duplicate stages,
source drift and absent/duplicated PG evidence; those are harness tests, not mail
delivery evidence. Worker ad-hoc reports and the denied parent temporary script
are not acceptance evidence.

The exact native PG directory recorded in `results.json` was confirmed removed
after successful shutdown. No development DB, commit, push or deployment was used.
Post-gate prose changes are restricted to README, ARCHITECTURE, FEATURE_PARITY and
this file; their hashes belong to the separate documentation closure receipt,
not the earlier frozen snapshot. DSN ledger/HMAC/intake, full Mailman replacement
and real-MTA cutover remain open.
