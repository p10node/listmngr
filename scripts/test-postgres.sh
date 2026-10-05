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
cargo test --locked -p listmngr --test doctor \
  regressions::postgres_doctor_contract -- --ignored --exact
cargo test --locked -p listmngr-runners --test nntp \
  postgres_gatenews_race_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test webhooks \
  postgres_webhooks_contract -- --ignored --exact
cargo test --locked -p listmngr-runners --test webhooks \
  postgres_webhook_claim_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  webhooks::postgres_webhooks_web_contract -- --ignored --exact
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
cargo test --locked -p listmngr-db --test workflows \
  postgres_confirmed_join_then_leave_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test notice_language \
  postgres_notice_language_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test alter_messages_settings \
  postgres_alter_messages_settings_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test usenet_settings \
  postgres_usenet_settings_contract -- --ignored --exact
cargo test --locked -p listmngr-import --test import21 \
  postgres_import21_contract -- --ignored --exact
cargo test --locked -p listmngr-import --test import3 \
  postgres_import3_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test imported_users \
  postgres_imported_user_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test imported_moderation \
  postgres_imported_moderation_contract -- --ignored --exact
cargo test --locked -p listmngr-import --test db3 \
  postgres_import3_db_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test imported_interactions \
  postgres_imported_interactions_contract -- --ignored --exact
cargo test --locked -p listmngr-import --test hyperkitty \
  postgres_hyperkitty_contract -- --ignored --exact
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
  outbound::site_notice_tests::postgres_site_notice_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  signup::postgres_signup_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  reset::postgres_reset_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  addresses::postgres_addresses_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  tokens::postgres_tokens_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  delete_account::postgres_delete_account_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  totp::postgres_totp_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  passkeys::postgres_passkeys_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  oidc::postgres_oidc_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  list_settings_groups::postgres_settings_groups_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  members_admin::postgres_members_admin_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  held_queue::postgres_held_queue_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  list_create_index::postgres_list_create_index_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  domains_users::postgres_domains_users_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  system::postgres_system_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  moderation_cross::postgres_moderation_cross_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  gdpr::postgres_gdpr_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  archive_render::postgres_archive_render_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  archive_search::postgres_archive_search_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  archive_ui::postgres_archive_ui_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  archive_interactions::postgres_archive_interactions_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  archive_post::postgres_archive_post_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  archive_export::postgres_archive_export_contract -- --ignored --exact
cargo test --locked -p listmngr-api --test webui \
  archive_admin::postgres_archive_admin_contract -- --ignored --exact
cargo test --locked -p listmngr-runners --lib \
  remote_archivers_tests::postgres_remote_archivers_contract -- --ignored --exact
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
cargo test --locked -p listmngr-db --test posting_rate \
  postgres_posting_rate_contract -- --ignored --exact
cargo test --locked -p listmngr-db --test master_key \
  postgres_master_key_contract -- --ignored --exact
run domains add "$domain" --description 'Phase 0 PostgreSQL CI contract'
run lists create "$list" --display-name 'Phase 0 CI contract'
run domains ls | grep -F "$domain" >/dev/null
run lists ls | grep -F "$list" >/dev/null
run lists remove "$list"
run domains rm "$domain"
trap - EXIT HUP INT TERM

printf '%s\n' 'PostgreSQL migration/connectivity/CRUD gate: PASS'
cargo test --locked -p listmngr-db --test user_create_verified \
  postgres_user_create_verified_contract -- --ignored --exact
