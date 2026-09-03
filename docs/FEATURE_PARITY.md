# Feature parity and acceptance evidence

Canonical requirements live in [PLAN.md](PLAN.md). This is a living evidence ledger, not a release announcement. Status meanings:

- **verified**: the named check has passed on the candidate;
- **implemented, evidence pending**: artifacts/code exist but the acceptance gate is not yet recorded as passing;
- **partial**: only the listed subset exists;
- **not implemented**: intentionally outside the current phase.

## Roadmap status

| Phase | Status | Scope and evidence boundary |
|---|---|---|
| 0 | implemented, evidence pending | Bootstrap artifacts exist; all P0/SEC gates below must pass together in CI/live Docker before a completion claim. |
| 1 | partial, evidence pending | Domain/DB/REST/CLI surface exists; full PostgreSQL CRUD and `mailmanclient` acceptance remain unproven here. |
| 2 | not implemented | LMTP intake, processing pipeline, moderation delivery, SMTP output, and generated MTA maps. |
| 3 | not implemented | Workflows, bounces, digests. |
| 4 | not implemented | Administration UI. |
| 5 | not implemented | Archive. |
| 6 | not implemented | Advanced features and migration. |
| 7 | not implemented | Release hardening and 1.0. |

## Phase 0 acceptance traceability

| ID | Contract | Repository evidence | Verification command | Current status |
|---|---|---|---|---|
| **P0-08** | Blocking CI runs fmt, locked build, clippy, tests, a PostgreSQL-backed gate, deny, audit, and cache. | `.github/workflows/ci.yml`, `scripts/test-postgres.sh` | `actionlint`; `TEST_POSTGRES_URL=… scripts/test-postgres.sh`; Cargo gates | artifact/actionlint/build/tests/deny verified; PG blocked locally; clippy fails existing Rust warnings |
| **P0-09** | Compose credentials align without committed secrets; root context is filtered; application becomes healthy. | `.env.example`, `.dockerignore`, `deploy/docker-compose.yml`, `deploy/Dockerfile` | `docker compose --env-file .env -f deploy/docker-compose.yml config`; `up --build --wait`; HTTP probes | config and independent static-musl build verified; daemon-dependent image/live probes blocked |
| **P0-10** | A runnable hardened systemd unit defines state/work directories and required protections. | `deploy/systemd/listmngr.service`, `deploy/README.md` | `systemd-analyze verify deploy/systemd/listmngr.service` on Linux | artifact contract verified; Linux tool unavailable locally |
| **P0-11** | Contributor, architecture, security, parity, deployment, and MTA-boundary docs are actionable and avoid later-phase claims. | `README.md`, `CLAUDE.md`, `docs/*.md`, `security.txt`, `deploy/README.md` | `scripts/check-phase0-artifacts.sh` plus documentation review | contract verified; external review pending |
| **SEC-10** | Supply chain and deployments are least-privilege and reproducible: full AGPL text, locked/pinned inputs, deny/audit, static non-root image, read-only runtime, systemd hardening. | `LICENSE`, `Cargo.lock`, `deny.toml`, CI, Docker/Compose/systemd files | contract script, `cargo deny check`, `cargo audit`, image inspection and runtime probes | contract/deny/static ELF verified; raw audit has documented inactive-graph exception; runtime/Linux probes blocked |

A route, file, or passing SQLite test alone is not parity evidence. PostgreSQL checks must consume `TEST_POSTGRES_URL`; container checks must exercise the built `scratch` image; later phase statuses change only with behavior-level evidence.

## Version and license baseline

All workspace crates are currently `0.1.0`, representing unreleased Phase 1 development rather than a completed/tagged release. The project license is `AGPL-3.0-or-later`; `LICENSE` contains the full AGPLv3 text. `Cargo.toml` and `Cargo.lock` are executable dependency/version sources of truth, as recorded by ADR-0003.
