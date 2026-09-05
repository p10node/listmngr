# Feature parity and acceptance evidence

[PLAN.md](PLAN.md) is the normative product contract. This ledger separates the
current development checkpoint, unclosed acceptance, and historical verification.
Code presence and a passing neighboring test are not full parity or release evidence.

## Current checkpoint — migration 0004

The implemented mail slice is **opt-in plaintext trusted-relay LMTP → inbound
policy → held moderation or outbound queue → SMTP**, with exact-byte database
intake, durable recipient attempts, and conservative uncertainty quarantine.
Enablement requires both `mta.enabled` and
`mta.smtp_tls = "plaintext_trusted_relay"`. This is **not production-ready, full
Phase 2 acceptance, or a Mailman replacement**.

| Phase | Status | Boundary |
|---|---|---|
| 0 | Implemented baseline; enclosing operational gates open | Historical local container/unit checks; no hosted CI or booted target-host systemd acceptance claimed. |
| 1 | Implemented, with bounded local acceptance | CRUD, REST/CLI, preference layering and pinned-client subset; current SQLite/client gates pass, PostgreSQL acceptance predates 0004. |
| 2 | Partial | Current bounded mail role and attempt repair; R1/O1 remain P1, current PostgreSQL attempt gate has no PASS. |
| 3 | Not implemented | Subscription workflows, email commands, bounces, digests. |
| 4 | Not implemented | Administration/member UI. |
| 5 | Not implemented | Archive. |
| 6 | Not implemented | Advanced features and Mailman migration. |
| 7 | Not complete | Release hardening and 1.0. |

### Current verification results

These are recorded parent/worker results, not fresh executions by the documentation
writer. A command below is a reproducible entry point, not a claim that every gate
passed on the latest bytes.

| Gate | Latest evidence and qualification |
|---|---|
| Focused O2/O3 tests | PASS for SQLite real-TCP sink/audit rollback/reopen/reclaim, cancellation, mixed/reserved outcomes, failed reservation and final-250/no-QUIT regressions listed below. |
| `cargo test --locked --workspace --all-targets` | Parent reports PASS for current attempt candidate. External PostgreSQL tests are ignored in this suite. |
| `cargo build --locked --workspace` | Parent reports PASS for current attempt candidate. |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Parent reports PASS for current attempt candidate. |
| `scripts/test-postgres.sh` | **Current attempt gate timed out (300 seconds); no PASS.** The prior seven-contract PASS predates migration 0004 and does not verify the new attempt contract. |
| Real `mailmanclient==3.3.5`, Phase 1 + held | Parent reran the maintained uv command below on the migration-0004 candidate before commit: exit 0, both flows PASS; fixture server and database cleaned up. |
| fmt/artifacts/Python/actionlint/deny/audit | Parent reran the maintained commands below before commit: all exit 0. Audit retains `RUSTSEC-2023-0071`; deny retains duplicate-dependency warnings. |
| Container/systemd/hosted CI | Historical evidence only. No current image/runtime, hosted CI or target-host systemd acceptance claimed. |

## Current bounded implementation and acceptance IDs

“Implemented” below means the named subset exists, not that the enclosing PLAN
acceptance has passed. PostgreSQL evidence is historical unless explicitly stated.

| ID | Implemented subset | Repository evidence / focused command |
|---|---|---|
| **P2-STORE** | Exact-byte DB intake, distinct submission UUIDs, SHA-256 blob identity; bounded Message-ID parsing/archive hash; standalone immutable filesystem library. | `crates/db/src/mail_queue.rs`, `crates/mail/src/{metadata,store}.rs`; `cargo test --locked -p listmngr-db --test mail_queue`; `cargo test --locked -p listmngr-mail --all-targets` |
| **P2-QUEUE** | Atomic enqueue/audit, claim, retry/shunt, expired-lease recovery and token fencing, subject to O1. | `crates/db/tests/{mail_queue,mail_queue_security}.rs`; isolated queue contract in `scripts/test-postgres.sh` |
| **P2-CLI** | Validated inbound injection, bounded ls, metadata show and opt-in exact raw export. | `crates/cli/src/queue.rs`; `cargo test --locked -p listmngr --test queue`; CLI unshunt absent |
| **P2-RUNTIME** | Monotonic heartbeat, repository `unshunt`, `complete_with_children`, `pending_recipients`, `begin_delivery`, `finish_delivery`; opt-in supervision and real mail workers. | `crates/db/tests/mail_queue_runtime.rs`, `crates/runners/src/`, `crates/cli/tests/mailpath_e2e.rs`; workspace tests |
| **P2-MODERATION** | Transactional durable hold/review and out-job/recipient snapshot on accept; no notices/bounces. | `crates/db/src/moderation.rs`, `crates/db/tests/moderation.rs`, `crates/api/tests/held.rs` |
| **P2-POLICY** | Pure bounded posting decisions and enabled regular-recipient selection honoring own-post preference. Unsupported header-match handling fails closed to hold. | `crates/pipeline/src/policy.rs`; `cargo test --locked -p listmngr-pipeline --all-targets`; not full chains/DMARC |
| **P2-TRANSPORT** | LMTP session framing/deadlines/reply cardinality, bounded plaintext SMTP responses and per-recipient outcomes, injection-safe header cooking; wired to the binary's enabled mail role. | `crates/mail/src/{lmtp,smtp,cook}.rs`, `crates/mail/tests/`; library and real-process tests; R1 still open |

Migrations `0001_mail_queue.sql`, `0002_mail_policy.sql`,
`0003_delivery_ambiguous_status.sql`, and `0004_delivery_attempt_token.sql` are
additive. The original Phase 1 semantic corpus is preserved; separate snapshots
extend both corpus consumers (`repositories.rs` and `schema_contract.rs`).
Filesystem-store selection, filesystem/DB lifecycle and garbage collection remain
absent; DB intake does not establish selectable-store parity.

### Held moderation: H1–H4

| ID | Bounded result | Tests and evidence boundary |
|---|---|---|
| **H1** | Accept/defer comments persist on native/compat and JSON/form requests. | `accept_and_defer_preserve_comments_on_both_prefixes_and_encodings`: historical RED empty comment → GREEN. |
| **H2** | Pending-state fence, disposed defer returns 409 without stray writes. | `disposed_defer_conflicts_without_stray_writes`, `racing_accept_and_defer_have_a_serializable_result`; SQLite serialized/concurrent controls and historical isolated PostgreSQL held contract PASS. |
| **H3** | User/token/peer-IP audit context and comment are transactional with moderation effects. | `moderation_audit_preserves_edge_context` (historical RED → GREEN), `moderation_audit_failure_rolls_back_every_business_write` (SQLite sabotage/rollback). PostgreSQL moderation-audit sabotage and broader domain/mismatched-ID coverage remain open. |
| **H4** | Real pinned-client Phase 1 + held flow PASS on the current migration-0004 candidate, independently rerun by parent before commit. | `scripts/test-mailmanclient.py`, `scripts/mailmanclient_held.py`; real binary, disposable SQLite, LMTP nonmember submission and TCP SMTP sink, no seeded held/message/queue rows. |

H4 checks count/list/get, all tested held properties and raw preview, scope denial,
unsupported options, defer comment retention, accept/replay with one sink delivery,
and reject/discard without delivery. The `/3.1` client's `name@host` paths use
flavor-aware `parse_list_path`; native IDs and authorization are not relaxed.
Action/comment only is supported; forwarding fails closed. Moderation replay is
not exactly-once SMTP acceptance.

### O2/O3 durable attempts and restart evidence

`begin_delivery` commits ambiguous/in-flight reservations, owning attempt tokens,
and audit before SMTP commands (TCP connect may happen first). Only the owning
lease can resolve a reservation through `finish_delivery`. Known transient results
explicitly restore pending; omitted reserved results remain ambiguous, while
omitted unreserved results remain pending. Outcome/audit rollback, cancellation,
and reclaim do not erase durable uncertainty. Ambiguous recipients are excluded
from automatic retry, including after repository unshunt. They can remain on a
done job; done is not a delivery receipt. Even never-sent attempts may require
manual reconciliation; there is no resolution CLI/UI or exactly-once guarantee.

| ID / scenario | Bounded evidence |
|---|---|
| **O2/O3** final-250 and missing-final-reply with failed ACK audit | `outbound::durability_tests::{final250_audit_failure_restart_never_replays_data,missingfinalreply_audit_failure_restart_never_replays_data}`: SQLite RED second DATA → GREEN zero second DATA after pool reopen/new lease; inspectable ambiguity and original bytes retained. |
| **O2/O3** cancellation after DATA | `canceled_after_data_restart_never_replays`: actual DATA barrier, cancel before final reply, reopen/reclaim, no second DATA; additional passing coverage, not a separate historical RED claim. |
| **O2/O3** reservation rollback and mixed outcomes | `reservation_audit_failure_fences_smtp_and_rolls_back`, `reserved_mixed_results_retry_only_known_transient_and_fence_old_lease`: no SMTP command after failed begin audit; only known transient retries; omitted reserved recipient remains ambiguous; stale/unrelated tokens cannot resolve it. |
| **O2** unreserved mixed/missing control | `mixed_and_missing_outcomes_persist_and_retry_only_pending_recipients` in `crates/runners/src/outbound_tests.rs`; missing unreserved recipients stay pending, unlike reserved uncertainty. |
| **O3** final DATA publication | `crates/mail/tests/smtp_final_result.rs::final250_returns_without_waiting_for_quit`: historical RED → GREEN; known acceptance returns within the bounded test deadline without QUIT I/O. |
| **O2/O3 PostgreSQL** | `outbound::durability_tests::postgres_isolated_audit_failure_restart_never_replays_data` in `crates/runners/src/outbound_durability_tests.rs` covers both final replies with unique schemas, real TCP and PostgreSQL ACK-audit sabotage. Wired into the maintained PG gate; **current execution timed out, not passed**. |
| **P2-RUNTIME / restart-lock** | `transient_delivery_waits_for_sqlite_writer_before_reading_snapshot`: historical RED → GREEN after `BEGIN IMMEDIATE`, with a real second-connection writer lock and retry-state assertion. This fixes snapshot upgrade failure, not O1. |
| **P2-RUNTIME / restart** | `durable_intake_survives_a_real_process_restart` in `crates/cli/tests/mailpath_e2e.rs`; zero accepted DATA before restart and exact raw retention. Historical ten parallel-suite repetitions passed; current workspace suite includes the restart test. |

## Residual P1s and enclosing blockers

| ID / gate | Status | Unclosed obligation |
|---|---|---|
| **R1 — partial LMTP batch commit timeout** | **OPEN P1** | `lmtp.rs` times out the entire deliver future; `inbound.rs` commits sequentially. A commits, B stalls, timeout discards A's known result. Preserve per-recipient progress outside the canceled future and verify the partial-commit case. |
| **O1 — lease clock after database lock waits** | **OPEN P1** | Supplied time is captured before lock acquisition, including `begin_delivery`; fence evaluation after a wait can use stale time. Production/synthetic clock seam and real two-connection deadline-under-lock tests remain required. |
| **O2/O3 backend acceptance** | Blocked | SQLite repair evidence is bounded; the current PostgreSQL attempt timeout is not a PASS or complete review closure. |
| Full Phase 2 and R/O review denominator | Open | No broad closure inferred from adjacent green tests. Full PLAN transport/policy/pipeline/MTA/operational acceptance is not delivered. |
| Mail security and later features | Not implemented in this slice | Transport TLS/SMTP AUTH, DKIM/DMARC/ARC, bounce processing/notices, digests, subscription workflows, archive, administration UI and migration. |
| Recovery and operations | Open | No automatic uncertainty resolution, store GC, full latest-candidate image/runtime or target-host systemd acceptance; no hosted CI PASS claimed. |

## Maintained verification commands

Run from the repository root. PostgreSQL commands require a deliberately disposable
backend supplied via `TEST_POSTGRES_URL`; the script migrates/writes fixtures and
must not target a development or production database. Credentials are not included
here. Historical temporary helpers/log files are not prerequisites or authoritative
commands.

```sh
cargo fmt --all --check
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
scripts/check-phase0-artifacts.sh
python3 -m unittest discover -s scripts/tests -v
python3 scripts/check-production-crates.py
actionlint
cargo deny check
cargo audit --ignore RUSTSEC-2023-0071
# Install the pinned requirements in a venv, or use uv:
uv run --with-requirements tests/compat/requirements-mailmanclient.txt python scripts/test-mailmanclient.py
# Set TEST_POSTGRES_URL securely to a disposable backend first:
scripts/test-postgres.sh
```

The PG script explicitly invokes repeated migration/CRUD, exact semantic schema,
scoped API authorization, isolated queue, monotonic heartbeat, fenced delivery,
held review, and durable-attempt sabotage contracts; there is no SQLite fallback.
Default workspace success does not execute its ignored external-backend tests.
The real-client harness starts and cleans up its own loopback server, SQLite DB,
LMTP fixture and SMTP sink without reading development `.env` or credentials.
Local success never substitutes for a hosted CI result.

## Historical acceptance — not latest-candidate proof

### Phase 0 baseline (2026-09-05)

| ID | Contract | Repository evidence | Verification command | Historical checkpoint status |
|---|---|---|---|---|
| **P0-08** | Blocking CI runs fmt, locked build, clippy, tests, a PostgreSQL-backed gate, deny, audit, and cache. | `.github/workflows/ci.yml`, `scripts/test-postgres.sh`, `scripts/test-mailmanclient.py` | `actionlint`; `TEST_POSTGRES_URL=… scripts/test-postgres.sh`; Cargo/Python gates | local commands verified; hosted CI pending |
| **P0-09** | Compose credentials align without committed secrets; root context is filtered; application becomes healthy. | `.env.example`, `.dockerignore`, `deploy/docker-compose.yml`, `deploy/Dockerfile` | isolated Compose config + `up -d --build --wait`; HTTP probes; `docker inspect` (historical topology summarized below) | verified locally: real scratch image, healthy service, HTTP 200, UID 1000, read-only, dropped capabilities |
| **P0-10** | A runnable hardened systemd unit defines state/work directories and required protections. | `deploy/systemd/listmngr.service`, `deploy/README.md` | `systemd-analyze verify` on Linux with the real built executable mounted at `/usr/local/bin/listmngr` | unit validation verified on systemd 252; running service/syscall-filter validation still requires the target Linux host |
| **P0-11** | Contributor, architecture, security, parity, deployment, and MTA-boundary docs are actionable and avoid later-phase claims. | `README.md`, `CLAUDE.md`, `docs/*.md`, `security.txt`, `deploy/README.md` | `scripts/check-phase0-artifacts.sh` plus documentation review | contract verified; external review pending |
| **SEC-10** | Supply chain and deployments are least-privilege and reproducible: full AGPL text, locked/pinned inputs, deny/audit, static non-root image, read-only runtime, systemd hardening. | `LICENSE`, `Cargo.lock`, `deny.toml`, CI, Docker/Compose/systemd files | contract script, `cargo deny check`, `cargo audit`, image inspection and runtime probes | local supply-chain/container/unit checks verified; documented inactive RSA exception retained; target-host systemd runtime and hosted CI pending |


Historical local evidence includes blocking Rust/Python/artifact/actionlint gates,
deny/audit with the existing inactive `RUSTSEC-2023-0071` exception, a real static
musl/scratch Compose image (healthy, HTTP 200, UID/GID 1000, read-only root,
capabilities dropped), and Linux systemd 252 unit validation with the real binary.
Unit validation was not a booted service/syscall-filter test. Reproduction topology
and safe disposable-project handling are described in `../deploy/README.md`.

### Phase 1 baseline and CLI integration (2026-09-05)

| ID | Contract | Repository evidence | Verification command | Historical checkpoint status |
|---|---|---|---|---|
| **P1-DB** | Complete Phase 1 schema and portable Domain/User/Address/List/Member/Preferences/Token/Audit CRUD on SQLite and PostgreSQL. | `crates/db/migrations/0000_init.sql`, `crates/db/src/lib.rs`, `crates/db/tests/repositories.rs`, `crates/db/tests/schema_contract.rs` | `cargo test --locked -p listmngr-db --test repositories`; `TEST_POSTGRES_URL='<postgres-url>' scripts/test-postgres.sh`; `TEST_POSTGRES_URL='<postgres-url>' cargo test --locked -p listmngr-db --test schema_contract live_postgres_matches_the_exact_sqlite_semantic_corpus -- --ignored --exact` | verified |
| **P1-PREF** | Nullable system → user → address → member preference layers resolve by precedence for all Phase 1 fields. | `crates/core/tests/preferences_exhaustive.rs`, `crates/db/tests/preference_layering.rs` | `cargo test --locked -p listmngr-core --test preferences_exhaustive`; `cargo test --locked -p listmngr-db --test preference_layering` | verified |
| **P1-REST** | `/3.1` and `/api/v1` share CRUD handlers with distinct serializers; form compatibility, pagination, typed response ETags, and full list config are behavioral. | `crates/api/src/lib.rs`, `crates/api/tests/rest.rs`, `crates/api/tests/openapi.rs`, `tests/compat/fixtures/mailman-3.3/` | `cargo test --locked -p listmngr-api --all-targets` | verified |
| **P1-AUTH** | Bearer scopes and list/domain bounds fail closed; Basic is restricted to enabled `/3.1` compatibility calls from allowlisted socket peers; pre/post-auth rate limits apply. | `crates/api/src/lib.rs`, `crates/api/tests/rest.rs`, `crates/api/tests/phase0_security.rs`, `crates/api/tests/postgres_auth.rs` | `cargo test --locked -p listmngr-api --all-targets`; `TEST_POSTGRES_URL='<postgres-url>' scripts/test-postgres.sh` explicitly runs scoped-user API authorization on PostgreSQL | verified |
| **P1-CLI** | Phase 1 commands use shared repositories; passwords avoid argv; typed errors, HTTP status, normalized IDNA lookup, and one-time token issuance are covered. | `crates/cli/src/{main,errors,status}.rs`, `crates/cli/tests/{cli,contracts,security,status}.rs` | `cargo test --locked -p listmngr --all-targets`; maintained commands above and historical CLI evidence below | verified locally |
| **P1-COMPAT** | Real `mailmanclient==3.3.5` creates a domain and list, subscribes a member, round-trips config, reads the roster, and cleans up. | `tests/compat/mailmanclient_phase1.py`, `tests/compat/requirements-mailmanclient.txt`, `scripts/test-mailmanclient.py` | After a locked build: `uv run --with-requirements tests/compat/requirements-mailmanclient.txt python scripts/test-mailmanclient.py` (or install the same requirements in a venv and run `python3 scripts/test-mailmanclient.py`) | verified |
| **P1-AUDIT** | Persistent business writes and their audit entry commit atomically; owned preferences/tokens are cleaned up transactionally. | `crates/db/src/lib.rs`, `crates/db/tests/repositories.rs`, `crates/api/tests/rest.rs` | `cargo test --locked -p listmngr-db --test repositories`; `cargo test --locked -p listmngr-api --test rest` | verified |


Historical Phase 1 gates passed on SQLite and a disposable PostgreSQL 17 backend,
including scoped authorization and exact schema semantics, and against real
`mailmanclient==3.3.5`. CLI integration added hidden/stdin/FD passwords, redacted
errors, status/proxy/redirect and IPv4/IPv6 controls, IDNA lookup/deletion, token
hash/revocation/expiry checks, and retained atomic role-scoped synchronization.
Earlier suite-size counts are intentionally omitted: they are not current counts.
No historical image/client pass establishes acceptance of migration 0004.

### Protocol/runtime repairs (2026-09-06, pre-0004)

These review IDs and useful regression names are retained for traceability. They
record the earlier RED → GREEN library/runtime repairs, not blanket closure of
the current R/O review. R1 and O1 above remain distinct unresolved findings.

| Finding | Root cause | Regression test (RED before fix, GREEN after) |
|---|---|---|
| Q1 | `heartbeat` overwrote `lease_until` unconditionally; a shorter-TTL or out-of-order renewal could shrink an already-extended deadline and let another worker reclaim the job early. Fixed with a portable `CASE WHEN $1>lease_until THEN $1 ELSE lease_until END` clamp (works identically on SQLite and PostgreSQL; no `GREATEST`/multi-arg `MAX`). | `heartbeat_deadline_is_monotonic_and_never_shrinks` (`crates/db/tests/mail_queue_runtime.rs`); `postgres_isolated_heartbeat_deadline_is_monotonic` (same file, live PostgreSQL via `scripts/test-postgres.sh`) |
| T1 | `smtp::read_response` sliced a `&str` reply at fixed byte offsets (`[..3]`, `[4..]`); a non-ASCII byte at that boundary (e.g. an emoji greeting) panicked. Fixed by parsing bytes throughout and lossy-decoding only the trailing text. | `non_ascii_greeting_bytes_never_panic_and_are_treated_as_malformed` (`crates/mail/tests/smtp.rs`) |
| T2 | SMTP response reads were unbounded (`String` `read_line`, unbounded continuation-line accumulation, per-line-restarted timeout). Fixed with a byte-capped line reader, a cap on continuation lines and total bytes, and a single deadline covering the whole response. | `unbounded_continuation_lines_do_not_hang_or_grow_forever` |
| T3 | LMTP `DATA` (including the oversized-message discard/drain path) had no deadline at all; SMTP writes/flushes were unbounded. Fixed with a deadline threaded through the whole `DATA` read loop (421 + close on expiry) and timeout-wrapped SMTP writes/flushes. | `data_phase_stall_after_start_times_out_and_closes_instead_of_hanging`, `oversized_drain_also_respects_the_data_deadline` (`crates/mail/tests/lmtp.rs`); `a_relay_that_stops_reading_mid_data_does_not_hang_the_write_forever` (`crates/mail/tests/smtp.rs`) |
| T4 | SMTP multiline parsing trusted the first line's code and never checked continuation lines matched it; the DATA-completion check accepted any `2xx` as `Sent`. Fixed: continuation-line code mismatch is a hard parse error, and only an exact `250` completes DATA as `Sent`. | `mismatched_continuation_code_is_never_reported_as_sent`, `only_exact_250_completes_data_as_sent` |
| T5 | Hostname/envelope-sender/recipient values were spliced into SMTP command lines, and header names/values/subject-prefix into cooked message headers, without validating for CR/LF/`:` injection. Fixed with pre-I/O validation (`is_safe_smtp_text`, `is_safe_value`, `is_safe_header_name`); an unsafe recipient is isolated to a `PermanentFailure` for that entry only, an unsafe hostname/envelope-sender/header/prefix is a hard `Err` before any I/O. | `hostname_and_envelope_sender_crlf_injection_is_rejected_before_any_io`, `envelope_sender_crlf_injection_is_rejected_before_any_command_is_sent`, `recipient_crlf_injection_is_isolated_to_that_recipient` (`crates/mail/tests/smtp.rs`); `rejects_a_subject_prefix_carrying_a_header_injection`, `rejects_addition_header_names_or_values_carrying_crlf` (`crates/mail/tests/cook.rs`) |
| T6 | A later recipient's I/O failure blanket-overwrote every recipient's outcome (including an already-known `550` for a different recipient) via an outer catch-all; DATA-start `554` was misclassified transient. Fixed by threading `Vec<Option<RecipientStatus>>` through the transaction so only still-`None` (pending) slots are ever resolved by a later failure, plus proper 4xx/5xx classification for the DATA-start reply, plus a new `Ambiguous` status for a connection lost strictly after the full message was written (distinct from an ordinary pre-send `TransientFailure`). | `a_permanent_rcpt_failure_survives_a_later_recipients_connection_loss`, `data_start_554_is_a_permanent_failure_not_transient`, `connection_loss_after_full_data_write_is_ambiguous_not_sent_or_plain_transient` |
| T7 | LMTP accepted `EHLO` (RFC 2033 §4.1 forbids it; only `LHLO` is valid) and never advertised/emitted `ENHANCEDSTATUSCODES`. Fixed: `EHLO`/`HELO` are rejected without a 250, and every substantive reply (except the `LHLO` capability lines and `354`, which have no RFC 3463 convention) carries a class-matching enhanced status code. | `ehlo_is_rejected_only_lhlo_is_accepted`, `enhanced_status_codes_are_advertised_and_present_on_replies` |
| T8 | If a handler returned more outcomes than accepted recipients, the transport sent one reply per outcome instead of one per recipient, desynchronizing pipelining. Fixed by iterating exactly `0..recipients.len()` and ignoring extra hook results. | `excess_hook_results_never_produce_more_replies_than_recipients`, `duplicate_rcpt_still_gets_one_reply_per_command` |
| E1 | The existing dot-unstuffing/byte-preservation test only asserted the reply code, never the exact bytes the handler received. Fixed by making the test handler capture deliveries (`Arc<Mutex<..>>`, inspectable after the session ends) and asserting the exact unstuffed bytes. | `dot_unstuffing_and_binary_bytes_are_preserved_exactly` (rewritten) |


Subsequent pre-0004 evidence included seven explicit PostgreSQL contracts
(CRUD/schema/auth/queue/heartbeat/fenced-delivery/held review) and the real pinned
Phase 1 + held client flow. Those passed against task-owned fixtures. Older
permission-denied/unrun notes described earlier attempts and are not the latest
PG result: the **current migration-0004 attempt gate timed out**. No historical
PASS is promoted to a new-candidate PASS.

## Version and license baseline

Workspace packages are `0.1.0`, unreleased development, not a completed phase or
release tag. The license is `AGPL-3.0-or-later`; `LICENSE` contains the full AGPLv3
text. `Cargo.toml` and `Cargo.lock` are executable version/dependency sources of
truth (ADR-0003).
