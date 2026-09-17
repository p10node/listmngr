#!/bin/sh
# Every PostgreSQL-backed test in the workspace, on one disposable server.
#
# `scripts/test-postgres.sh` is the mandatory fast gate (a chosen set of
# contracts). This runs the whole `#[ignore]`d PostgreSQL population: each
# test creates and drops its own schema on TEST_POSTGRES_URL, so the server
# only needs to be disposable, never empty. The four ignored tests that
# need other fixtures (a browser, `postmap`) are skipped by name.
set -eu

: "${TEST_POSTGRES_URL:?TEST_POSTGRES_URL must point to a disposable PostgreSQL server}"
case "$TEST_POSTGRES_URL" in
  postgres://*|postgresql://*) ;;
  *) printf '%s\n' 'TEST_POSTGRES_URL must be a PostgreSQL URL' >&2; exit 2 ;;
esac
case "$TEST_POSTGRES_URL" in
  *'***'*|*'<password>'*|*REDACTED*)
    printf '%s\n' 'TEST_POSTGRES_URL contains a redaction placeholder' >&2
    exit 2
    ;;
esac

# This gate never falls back to SQLite: every test below reads the URL
# explicitly and fails when the server is unavailable.
exec cargo test --locked --workspace --all-targets -- --ignored \
  --skip chromium_browser_acceptance \
  --skip chromium_acceptance_journey \
  --skip real_postfix_lookup_agrees_with_runtime_recipient_validation \
  --skip real_postmap_compiles_hash_maps_that_answer_exact_lookups
