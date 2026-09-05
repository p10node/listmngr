# listmngr

`listmngr` is a security-focused mailing-list manager written in Rust, version **0.1.0 (unreleased development)**, licensed **AGPL-3.0-or-later**. Phase 1 has recorded local acceptance evidence. The current development checkpoint adds a **bounded, opt-in plaintext trusted-relay LMTP → held moderation → SMTP path**, durable queue attempts and conservative uncertainty quarantine through migration `0004_delivery_attempt_token.sql`.

**This is not production-ready, full Phase 2 acceptance, or a Mailman replacement.** R1 (partial LMTP batch commit on timeout) and O1 (lease clock across database lock waits) remain open P1 findings. Current locked workspace tests/build/Clippy passed as reported by the parent verifier; the current PostgreSQL attempt gate timed out and has **no PASS**. Earlier PostgreSQL and pinned-client passes predate migration 0004. See [`docs/FEATURE_PARITY.md`](docs/FEATURE_PARITY.md) for the evidence boundary; [`docs/PLAN.md`](docs/PLAN.md) remains the normative product target.

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

The mail role is disabled by default. Enabling `mta.enabled` also requires
`mta.smtp_tls = "plaintext_trusted_relay"`; unsupported TLS modes fail configuration
validation rather than silently downgrading. Use only an isolated development
fixture or a trusted, restricted relay network. Mail transport TLS and SMTP AUTH
are not implemented. Keep the deployment MTA snippets disabled pending acceptance
and operational review.

`serve` connects the LMTP session library, durable database intake, pure inbound
posting policy, and inbound/outbound workers with lease renewal and shutdown
supervision. Held REST under `/api/v1` and `/3.1` supports read/count and
accept/reject/discard/defer with authorization, pending-state fencing, persisted
comments, and transactional user/token/peer-IP audit attribution. Unsupported
forwarding fields/actions fail closed. Reject records a disposition; it does not
send a rejection notice or bounce.

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
options, accept/replay with one sink delivery, and reject/discard without delivery.
These are bounded fixture assertions, not full Mailman compatibility.

## Configuration and deployment

Configuration is TOML plus `LISTMNGR__SECTION__KEY` environment overrides. Prefer `database.url_file` or a root-readable environment file in production; `conf` output redacts credentials. See:

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for crate, process, data, and deployment boundaries;
- [`deploy/README.md`](deploy/README.md) for Compose, systemd, and intentionally disabled MTA snippets;
- [`docs/SECURITY.md`](docs/SECURITY.md) and [`security.txt`](security.txt) for the threat model and private reporting path;
- [`docs/PLAN.md`](docs/PLAN.md) for the canonical roadmap and acceptance IDs.

## License and versioning

All workspace packages are version `0.1.0`; no released tag is implied. The project is licensed under GNU Affero General Public License v3 or later. The complete license is in [`LICENSE`](LICENSE); rationale is in ADR-0003.
