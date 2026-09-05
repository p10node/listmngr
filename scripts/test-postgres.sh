#!/bin/sh
set -eu

: "${TEST_POSTGRES_URL:?TEST_POSTGRES_URL must point to the CI PostgreSQL service}"
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

run() {
  LISTMNGR__DATABASE__URL="$TEST_POSTGRES_URL" cargo run --locked --quiet -p listmngr -- "$@"
}

# This gate cannot silently fall back to SQLite: every command is explicitly
# configured from TEST_POSTGRES_URL and fails if PostgreSQL is unavailable.
domain=phase0-ci.invalid
list=contract.phase0-ci.invalid
cleanup() {
  run lists remove "$list" >/dev/null 2>&1 || true
  run domains rm "$domain" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

run migrate
# status probes a running HTTP service; this backend gate intentionally has none.
run domains ls >/dev/null
cargo test --locked -p listmngr-db --test repositories \
  postgres_repeated_migrate_schema_and_crud_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test schema_contract \
  live_postgres_matches_the_exact_sqlite_semantic_corpus -- --ignored --exact
cargo test --locked -p listmngr-api --test postgres_auth \
  postgres_scoped_user_routes_allow_inside_and_deny_outside_bounds -- --ignored --exact
cargo test --locked -p listmngr-db --test mail_queue \
  postgres_isolated_mail_queue_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test mail_queue_runtime \
  postgres_isolated_heartbeat_deadline_is_monotonic -- --ignored --exact
cargo test --locked -p listmngr-db --test mail_queue_runtime \
  postgres_isolated_finish_delivery_is_fenced_and_quarantines_ambiguous -- --ignored --exact
cargo test --locked -p listmngr-api --test held \
  postgres_isolated_held_review_contract -- --ignored --exact
cargo test --locked -p listmngr-runners --lib \
  outbound::durability_tests::postgres_isolated_audit_failure_restart_never_replays_data -- --ignored --exact
run domains add "$domain" --description 'Phase 0 PostgreSQL CI contract'
run lists create "$list" --display-name 'Phase 0 CI contract'
run domains ls | grep -F "$domain" >/dev/null
run lists ls | grep -F "$list" >/dev/null
run lists remove "$list"
run domains rm "$domain"
trap - EXIT HUP INT TERM

printf '%s\n' 'PostgreSQL migration/connectivity/CRUD gate: PASS'
