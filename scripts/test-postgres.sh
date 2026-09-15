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
cargo test --locked -p listmngr-db --test queue_operations \
  postgres_isolated_resolution_is_atomic_and_single_winner -- --ignored --exact
cargo test --locked -p listmngr-db --test digests \
  postgres_isolated_digest_publish_rollback_and_single_winner -- --ignored --exact
cargo test --locked -p listmngr-db --test list_posting_settings \
  postgres_waiting_patch_does_not_overwrite_other_committed_fields -- --ignored --exact
cargo test --locked -p listmngr-db --test moderation_rules \
  postgres_moderation_rules_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test header_matches \
  postgres_header_matches_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test bans \
  postgres_site_bans_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test tasks \
  postgres_task_sweep_and_notify_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test notice_language \
  postgres_notice_language_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test alter_messages_settings \
  postgres_alter_messages_settings_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test subject_prefix \
  postgres_subject_prefix_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test mail_queue_runtime \
  postgres_isolated_heartbeat_deadline_is_monotonic -- --ignored --exact
cargo test --locked -p listmngr-db --test mail_queue_runtime \
  postgres_isolated_finish_delivery_is_fenced_and_quarantines_ambiguous -- --ignored --exact
cargo test --locked -p listmngr-api --test held \
  postgres_isolated_held_review_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  sessions::postgres_session_inventory_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  profile::postgres_profile_contract -- --ignored --exact
cargo test --locked -p listmngr-runners --lib \
  outbound::dsn_issuance_tests::dsn_issuance_postgres -- --ignored --exact
cargo test --locked -p listmngr-runners --lib \
  outbound::durability_tests::postgres_isolated_audit_failure_restart_never_replays_data -- --ignored --exact
cargo test --locked -p listmngr-db --test mail_queue_lock_clock \
  expiry_during_postgres_row_lock_wait_fences_all_mutations -- --ignored --exact
cargo test --locked -p listmngr-db --test mail_queue_lock_clock \
  final_audit_postgres::postgres_final_audit_wait_fences_queue_authority -- --ignored --exact
cargo test --locked -p listmngr-db --test archive_final_audit \
  postgres::postgres_archive_final_audit_wait -- --ignored --exact
cargo test --locked -p listmngr-db --test digest_final_audit \
  postgres::postgres_digest_final_audit_wait -- --ignored --exact --nocapture
cargo test --locked -p listmngr-db --test workflow_final_audit \
  postgres::postgres_workflow_final_audit_wait -- --ignored --exact --nocapture
cargo test --locked -p listmngr-db --test moderation_final_audit \
  postgres::postgres_moderation_final_audit_wait -- --ignored --exact --nocapture
cargo test --locked -p listmngr-db --test sibling_lease_clock \
  postgres_sibling_list_queue_and_pool_waits_are_fenced -- --ignored --exact
cargo test --locked -p listmngr-db --test smtp_bounces \
  preference_races::postgres_preference_disable_wins_before_scoring -- --ignored --exact
cargo test --locked -p listmngr-db --test smtp_bounces \
  preference_races::postgres_metadata_patch_preserves_committed_disable -- --ignored --exact
cargo test --locked -p listmngr-db --test smtp_bounces \
  notice_cases::postgres_disable_notice_snapshot_and_atomic_failure -- --ignored --exact
cargo test --locked -p listmngr-db --test smtp_bounces \
  increment_cases::supplement::postgres_increment_notice_atomic_regression -- --ignored --exact
cargo test --locked -p listmngr-db --test bounce_maintenance \
  postgres::postgres_maintenance_atomicity_and_concurrent_winner -- --ignored --exact
cargo test --locked -p listmngr-db --test web_bounce_recovery \
  postgres_recovery_controls -- --ignored --exact
cargo test --locked -p listmngr-api --test web_own_postings \
  controls::postgres_own_postings_controls -- --ignored --exact
cargo test --locked -p listmngr-api --test web_list_copy \
  controls::postgres_list_copy_controls -- --ignored --exact
cargo test --locked -p listmngr-runners --test list_copy \
  postgres_list_copy_controls -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  posting_limits::postgres_posting_limits_controls -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  emergency::postgres_emergency_controls -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  subject_prefix_controls::postgres_subject_prefix_controls -- --ignored --exact
run domains add "$domain" --description 'Phase 0 PostgreSQL CI contract'
run lists create "$list" --display-name 'Phase 0 CI contract'
run domains ls | grep -F "$domain" >/dev/null
run lists ls | grep -F "$list" >/dev/null
run lists remove "$list"
run domains rm "$domain"
trap - EXIT HUP INT TERM

printf '%s\n' 'PostgreSQL migration/connectivity/CRUD gate: PASS'
