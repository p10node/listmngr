# Contributor and automation guide

## Source of truth and scope

- `docs/PLAN.md` is the canonical product contract; `docs/FEATURE_PARITY.md` is the living evidence ledger.
- Implement one vertical behavior at a time with a failing test first.
- Phase boundaries are strict. In particular, do not add or claim LMTP delivery, runners, SMTP delivery, or other Phase 2 behavior while closing Phase 0/1 artifacts.
- A present file or route is not completion evidence. Record the exact passing command or CI run before changing a status to verified.

## Required gates

Run from the repository root:

```sh
scripts/check-phase0-artifacts.sh
cargo fmt --all --check
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo deny check
cargo audit --ignore RUSTSEC-2023-0071 # documented inactive SQLx/MySQL lock-only edge
```

PostgreSQL is a separate mandatory gate and may not silently fall back to SQLite:

```sh
TEST_POSTGRES_URL='postgres://listmngr:<local-password>@127.0.0.1:5432/listmngr' \
  scripts/test-postgres.sh
```

Use `--locked` for Cargo builds/tests in CI. Keep action references at reviewed commit SHAs, tools at exact versions, and container bases at immutable manifest digests where practical.

## Docker and environment rules

- Build from root: `docker build -f deploy/Dockerfile .`; never use `docker build deploy`.
- Validate with `docker compose --env-file .env -f deploy/docker-compose.yml config`.
- Rust does not load `.env`. For host commands, explicitly source it and export `LISTMNGR__DATABASE__URL="$LISTMNGR_HOST_DATABASE_URL"`.
- `postgres` is container-network DNS; host commands use `127.0.0.1`.
- Never commit `.env`, production URLs, passwords, API tokens, or private keys. Examples must be conspicuously disposable.

## Security and persistence invariants

- Never log configuration secrets, DSNs, bearer tokens, passwords, or raw key material.
- Every persistent business write and its audit event must commit in one transaction.
- Preserve non-root/read-only container operation and systemd hardening. Changes to syscall/address-family restrictions require runtime verification.
- The MTA snippets in `deploy/` are documentation-only until Phase 2; do not enable them against the Phase 0/1 binary.

## Documentation discipline

When behavior or acceptance changes, update `README.md`, `docs/ARCHITECTURE.md`, and the acceptance ID row in `docs/FEATURE_PARITY.md` in the same change. Version `0.1.0` means unreleased Phase 1 development; the license identifier is `AGPL-3.0-or-later` and the full legal text must remain in `LICENSE`.
