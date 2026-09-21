#!/bin/sh
# Every PostgreSQL-backed test in the workspace, on one disposable server.
#
# `scripts/test-postgres.sh` is the mandatory fast gate (a chosen set of
# contracts). This runs the whole `#[ignore]`d PostgreSQL population: each
# test creates and drops its own schema on TEST_POSTGRES_URL, so the server
# only needs to be disposable, never empty. The ignored tests that
# need other fixtures (a browser, `postmap`, a benchmark, Mailman's own
# `testing/` directory, a running Mailman 3 core) are skipped by name.
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
  --skip search_p95_is_under_100ms_over_100k_posts --skip import_100k_posts_in_under_ten_minutes \
  --skip real_postfix_lookup_agrees_with_runtime_recipient_validation \
  --skip real_postmap_compiles_hash_maps_that_answer_exact_lookups \
  --skip import21_reads_mailman3s_own_fixture \
  --skip import3_reads_a_real_mailman3_core
