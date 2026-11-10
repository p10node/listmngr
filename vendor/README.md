# Vendored crates

`libsqlite3-sys/` is libsqlite3-sys 0.39.1 from crates.io (MIT, bundled SQLite
3.54.0, which fixes CVE-2026-119375 / RUSTSEC-2026-0459). Changes against the
published crate:

- `Cargo.toml`: `version` changed from `0.39.1` to `0.30.1`, because
  `sqlx-sqlite` 0.8.6 requires `libsqlite3-sys = "0.30.1"` and Cargo only accepts
  a `[patch]` that satisfies the requirement.
- The unused `sqlcipher/`, `Cargo.lock`, `Cargo.toml.orig` and `Upgrade.md`
  were removed.

Remove this directory and the `[patch.crates-io]` entry in the root `Cargo.toml`
as soon as `sqlx-sqlite` accepts `libsqlite3-sys >= 0.39`.
