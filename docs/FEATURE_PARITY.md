# Feature parity and acceptance evidence

Canonical requirements live in [PLAN.md](PLAN.md). This is a living evidence ledger, not a release announcement. Status meanings:

- **verified**: the named check has passed on the candidate;
- **implemented, evidence pending**: artifacts/code exist but the acceptance gate is not yet recorded as passing;
- **partial**: only the listed subset exists;
- **not implemented**: intentionally outside the current phase.

## Roadmap status

| Phase | Status | Scope and evidence boundary |
|---|---|---|
| 0 | implemented, hosted CI evidence pending | Local Rust, PostgreSQL, real-client, live Docker, and Linux unit-validation gates pass; no hosted CI run is claimed. |
| 1 | verified | Phase 1 repositories, REST/CLI surface, PostgreSQL + SQLite CRUD, exhaustive preference layering, and `mailmanclient==3.3.5` compatibility passed the commands recorded below on the candidate worktree. |
| 2 | not implemented | LMTP intake, processing pipeline, moderation delivery, SMTP output, and generated MTA maps. |
| 3 | not implemented | Workflows, bounces, digests. |
| 4 | not implemented | Administration UI. |
| 5 | not implemented | Archive. |
| 6 | not implemented | Advanced features and migration. |
| 7 | not implemented | Release hardening and 1.0. |

## Phase 0 acceptance traceability

| ID | Contract | Repository evidence | Verification command | Current status |
|---|---|---|---|---|
| **P0-08** | Blocking CI runs fmt, locked build, clippy, tests, a PostgreSQL-backed gate, deny, audit, and cache. | `.github/workflows/ci.yml`, `scripts/test-postgres.sh`, `scripts/test-mailmanclient.py` | `actionlint`; `TEST_POSTGRES_URL=… scripts/test-postgres.sh`; Cargo/Python gates | local commands verified; hosted CI pending (no Git remote configured) |
| **P0-09** | Compose credentials align without committed secrets; root context is filtered; application becomes healthy. | `.env.example`, `.dockerignore`, `deploy/docker-compose.yml`, `deploy/Dockerfile` | isolated Compose config + `up -d --build --wait`; HTTP probes; `docker inspect` (protocol below) | verified locally: real scratch image, healthy service, HTTP 200, UID 1000, read-only, dropped capabilities |
| **P0-10** | A runnable hardened systemd unit defines state/work directories and required protections. | `deploy/systemd/listmngr.service`, `deploy/README.md` | `systemd-analyze verify` on Linux with the real built executable mounted at `/usr/local/bin/listmngr` | unit validation verified on systemd 252; running service/syscall-filter validation still requires the target Linux host |
| **P0-11** | Contributor, architecture, security, parity, deployment, and MTA-boundary docs are actionable and avoid later-phase claims. | `README.md`, `CLAUDE.md`, `docs/*.md`, `security.txt`, `deploy/README.md` | `scripts/check-phase0-artifacts.sh` plus documentation review | contract verified; external review pending |
| **SEC-10** | Supply chain and deployments are least-privilege and reproducible: full AGPL text, locked/pinned inputs, deny/audit, static non-root image, read-only runtime, systemd hardening. | `LICENSE`, `Cargo.lock`, `deny.toml`, CI, Docker/Compose/systemd files | contract script, `cargo deny check`, `cargo audit`, image inspection and runtime probes | local supply-chain/container/unit checks verified; documented inactive RSA exception retained; target-host systemd runtime and hosted CI pending |

A route, file, or passing SQLite test alone is not parity evidence. PostgreSQL checks must consume `TEST_POSTGRES_URL`; container checks must exercise the built `scratch` image; later phase statuses change only with behavior-level evidence.

## Phase 1 acceptance traceability

Reverified locally on 2026-09-05. Database credentials are intentionally omitted; substitute a disposable local PostgreSQL URL for `<postgres-url>`.

| ID | Contract | Repository evidence | Verification command | Current status |
|---|---|---|---|---|
| **P1-DB** | Complete Phase 1 schema and portable Domain/User/Address/List/Member/Preferences/Token/Audit CRUD on SQLite and PostgreSQL. | `crates/db/migrations/0000_init.sql`, `crates/db/src/lib.rs`, `crates/db/tests/repositories.rs`, `crates/db/tests/schema_contract.rs` | `cargo test --locked -p listmngr-db --test repositories`; `TEST_POSTGRES_URL='<postgres-url>' scripts/test-postgres.sh`; `TEST_POSTGRES_URL='<postgres-url>' cargo test --locked -p listmngr-db --test schema_contract live_postgres_matches_the_exact_sqlite_semantic_corpus -- --ignored --exact` | verified |
| **P1-PREF** | Nullable system → user → address → member preference layers resolve by precedence for all Phase 1 fields. | `crates/core/tests/preferences_exhaustive.rs`, `crates/db/tests/preference_layering.rs` | `cargo test --locked -p listmngr-core --test preferences_exhaustive`; `cargo test --locked -p listmngr-db --test preference_layering` | verified |
| **P1-REST** | `/3.1` and `/api/v1` share CRUD handlers with distinct serializers; form compatibility, pagination, typed response ETags, and full list config are behavioral. | `crates/api/src/lib.rs`, `crates/api/tests/rest.rs`, `crates/api/tests/openapi.rs`, `tests/compat/fixtures/mailman-3.3/` | `cargo test --locked -p listmngr-api --all-targets` | verified |
| **P1-AUTH** | Bearer scopes and list/domain bounds fail closed; Basic is restricted to enabled `/3.1` compatibility calls from allowlisted socket peers; pre/post-auth rate limits apply. | `crates/api/src/lib.rs`, `crates/api/tests/rest.rs`, `crates/api/tests/phase0_security.rs`, `crates/api/tests/postgres_auth.rs` | `cargo test --locked -p listmngr-api --all-targets`; `TEST_POSTGRES_URL='<postgres-url>' scripts/test-postgres.sh` explicitly runs scoped-user API authorization on PostgreSQL | verified |
| **P1-CLI** | Phase 1 domain/list/member/user/token commands operate through the same repositories. | `crates/cli/src/main.rs`, `crates/cli/tests/cli.rs` | `cargo test --locked -p listmngr --test cli` | verified |
| **P1-COMPAT** | Real `mailmanclient==3.3.5` creates a domain and list, subscribes a member, round-trips config, reads the roster, and cleans up. | `tests/compat/mailmanclient_phase1.py`, `tests/compat/requirements-mailmanclient.txt`, `scripts/test-mailmanclient.py` | After a locked build: `uv run --with-requirements tests/compat/requirements-mailmanclient.txt python scripts/test-mailmanclient.py` (or install the same requirements in a venv and run `python3 scripts/test-mailmanclient.py`) | verified |
| **P1-AUDIT** | Persistent business writes and their audit entry commit atomically; owned preferences/tokens are cleaned up transactionally. | `crates/db/src/lib.rs`, `crates/db/tests/repositories.rs`, `crates/api/tests/rest.rs` | `cargo test --locked -p listmngr-db --test repositories`; `cargo test --locked -p listmngr-api --test rest` | verified |

## Local verification protocol (2026-09-05)

- `scripts/check-phase0-artifacts.sh`, `python3 -m unittest discover -s scripts/tests -v`, `python3 scripts/check-production-crates.py`, and `actionlint` pass. The anti-stub check explicitly reports future-phase empty crates instead of claiming their implementations exist.
- `cargo fmt --all --check`, `cargo build --locked --workspace`, `cargo test --locked --workspace --all-targets`, `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`, `cargo deny check`, and `cargo audit --ignore RUSTSEC-2023-0071` pass. The normal workspace suite has 84 passing tests; the three PostgreSQL tests are ignored there and invoked explicitly by the separate backend gate.
- `TEST_POSTGRES_URL='<disposable-postgres-url>' scripts/test-postgres.sh` passes against a dedicated PostgreSQL 17 container on loopback port 55432. It runs repeated migration/CRUD, exact semantic schema comparison, and API list/domain-scoped user authorization on both prefixes with allowed/denied users. There is no SQLite fallback.
- The real-client harness passes with `mailmanclient==3.3.5`, a fresh SQLite database, and an ephemeral loopback HTTP port. It closes its server and temporary directory; no development `.env`, database, or credentials are consumed.
- Docker uses an isolated project, not the development Compose project: set a disposable `POSTGRES_PASSWORD` and `LISTMNGR_PORT=58000`, then run `docker compose --env-file /dev/null -p listmngr-main-gate -f deploy/docker-compose.yml config --quiet` and the same prefix with `up -d --build --wait --wait-timeout 180`. `/healthz` and `/readyz` on port 58000 return 200. `docker inspect` reports `user=1000:1000`, `readonly=true`, `CapDrop=["ALL"]`, `no-new-privileges:true`, and `healthy`. The copied runtime executable is a statically linked Linux aarch64 ELF. Only this disposable project's volume is removed during test cleanup.
- Linux unit validation: copy `/listmngr` from the tested image, mount it read-only at `/usr/local/bin/listmngr` alongside the committed unit in a Linux container with systemd 252, and run `systemd-analyze verify /etc/systemd/system/listmngr.service` (exit 0). This does not claim a booted systemd service or tested syscall-filter runtime.
- Regression evidence: the anti-stub null-metadata fixture failed with `AttributeError` before the fix; missing CI probes failed the wiring test; the real Alpine build failed on missing `assert.h` before pinned musl headers were added; PostgreSQL scoped user GET failed with 500 before `$1`/`$2` replaced SQLite placeholders. All corresponding final checks pass. The first scoped-API rerun exposed fixture cleanup FK ordering, corrected without changing production deletion semantics.

## Version and license baseline

All workspace crates are currently `0.1.0`, representing unreleased Phase 1 development rather than a completed/tagged release. The project license is `AGPL-3.0-or-later`; `LICENSE` contains the full AGPLv3 text. `Cargo.toml` and `Cargo.lock` are executable dependency/version sources of truth, as recorded by ADR-0003.
