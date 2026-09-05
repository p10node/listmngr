# Architecture

This document describes the implemented Phase 0/1 runtime and experimental opt-in Phase 2 mail role. Phase 2 acceptance and later roadmap components remain incomplete.

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
| `listmngr-web` | Web adapter boundary; full administration UI is Phase 4 |
| `listmngr` (`cli`) | Binary commands, configuration bootstrap, migration, and HTTP process startup |

## Process and request model

`listmngr serve` runs one Tokio process. In the current phase it starts HTTP endpoints for liveness (`/healthz`), database-backed readiness (`/readyz`), metrics, API compatibility, and OpenAPI. HTTP adapters authenticate and authorize before calling repositories. With `mta.enabled`, it additionally runs experimental LMTP intake and inbound/outbound queue workers. Digest, bounce, and archive workers are not implemented.

`/healthz` proves that the process can answer. `/readyz` proves database-backed readiness. `listmngr status` is an HTTP client of these two endpoints, in that order, not a second database connection. It uses `web.listen`, maps wildcard binds to IPv4/IPv6 loopback, disables environment proxies and redirects, and limits each request to two seconds. Exit codes distinguish transport failure (3), unhealthy HTTP status (4), and not-ready HTTP status (5). The same binary probe runs in the shell-free container. None of these signals is a substitute for end-to-end mail delivery evidence.

## Data and transaction boundaries

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
- The role requires `mta.enabled` and explicit
  `mta.smtp_tls = "plaintext_trusted_relay"`; transport TLS and SMTP AUTH are not
  implemented. Do not expose this experimental role as an untrusted public MTA.
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

internet mail -> Postfix/Exim --[Phase 2 LMTP, currently disabled]--> listmngr
```

The Docker builder uses a pinned Alpine Rust image and emits a musl-linked release binary. The final `scratch` image contains the binary and CA roots only, runs as numeric UID/GID 1000, drops capabilities in Compose, and does not need a shell or `curl`. The systemd alternative uses the same non-root trust boundary plus syscall, address-family, kernel, device, home, and filesystem restrictions.

The builder also pins `musl` and `musl-dev` to `1.2.5-r12`: the Rust base includes a compiler but not the C headers required by `ring` and bundled SQLite. Package tools/headers remain outside the runtime image. The real-client acceptance harness (`scripts/test-mailmanclient.py`) uses a separate loopback process and disposable SQLite database, while `scripts/test-postgres.sh` independently exercises PostgreSQL CRUD, schema, authorization, queue, held, and attempt contracts; see the evidence ledger for which checkpoint passed. CI runs these behavioral probes in addition to Rust and anti-stub gates.

The real-client harness feeds its fixture password through stdin. The PostgreSQL-only gate uses `domains ls` after migration to check repository access; it deliberately does not invoke the HTTP `status` command without a running server. CLI network fixtures exercise both address families, unhealthy/not-ready/timeout outcomes, proxy isolation, and redirect rejection independently of a database.

## Security boundaries

Bearer tokens carry scopes and optional list/domain bounds. Mailman Basic compatibility is disabled by default, is accepted only below `/3.1`, and, when configured, trusts the actual socket peer CIDR—not forwarded headers. `/api/v1` accepts Bearer authentication and successful typed GET responses carry content-derived ETags; `/3.1` retains the Mailman-compatible JSON shape instead. Secrets are returned once and only digests persist. Reverse proxies must sanitize forwarding headers and terminate TLS according to the operator threat model. See `SECURITY.md` for threats and residual risks.
