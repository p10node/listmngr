# Owner welcome/goodbye controls

Acceptance: `P4-WEB-NOTICES` — bounded local acceptance verified.

## Use and semantics

Open **List administration → List settings** as a verified list owner or server
owner. **Send welcome messages** and **Send goodbye messages** are independent
Yes/No switches over the existing list policies. Their existing default remains
false. They govern future completed subscriptions/removals; configuration edits
do not notify existing subscribers or recall already queued notices.

`SettingsForm` accepts optional boolean values. Omission preserves the current
value and leaves the key out of the audit patch; explicit false disables the
supplied policy. Only exact true/false are accepted. Blank, numeric, alternate
case/spelling, whitespace and duplicate field forms are rejected. Browser selects
show persisted state using explicit labels.

The existing `browser_update_list_settings` transaction still checks current
owner/session authority, locks the fresh list row, invokes the shared validator
and commits configuration together with its attributed `list.config` audit.
No schema, dependencies, transport, notice templates or publication semantics
changed. The successful membership transition still owns durable publication.

## Tests actually run

Three sequential behavioral RED→GREENs:

- `cargo test --locked -p listmngr-api --test webui goodbye::goodbye_browser_leave_is_private_audited_and_only_after_confirmed_post -- --exact`
  POST with `send_goodbye_message` failed HTTP422 versus303 before extraction
  support, then passed through confirmed browser leave and notice publication.
  Logs: `target/web-notices-goodbye-{red,green}.log`.
- `cargo test --locked -p listmngr-api --test webui notices::owner_welcome_setting_controls_new_subscription_notices -- --exact`
  POST with `send_welcome_message` failed HTTP422 versus303, then passed through
  actual subscription publication and duplicate-conflict silence. A supplementary
  assumption that duplicate bulk subscribe succeeds was corrected to the existing
  Conflict contract; that fixture error is not feature RED evidence.
  Logs: `target/web-notices-welcome-{red,green}.log`.
- `cargo test --locked -p listmngr-api --test webui notices::owner_notice_form_labels_and_current_selections -- --exact`
  failed on missing labels, then passed with both true and false selections.
  Logs: `target/web-notices-ui-{red,green}.log` (GREEN ran the notices module).

Supplementary controls cover both/individual omission, explicit false/true,
exact supplied-key audits, strict/duplicate form rejection and no publication
from settings edits. The existing owner matrix now carries both flags through
CSRF/Origin, foreign-list, revoked role/verification/session, server-owner and
injected-audit-failure rollback checks. The registered isolated-schema PostgreSQL
wrapper also runs welcome publication and notice controls; it does not rerun the
SQLite browser-leave test on PostgreSQL.

Frozen final command: `python3 -u target/web-notices-gates.py`.
Evidence directory: `target/web-notices-gates-20260909-121615/`.

- **17/17 gate commands passed**, with successful `final.json` and per-command
  `results.json`.
- `cargo test --locked --workspace --all-targets`: **574 passed, 0 failed,
  40 ignored**. Opt-in ignored tests are not counted as passing.
- Mandatory `scripts/test-postgres.sh`: **23 passed, 0 failed, 0 ignored**, on an
  owned disposable local PostgreSQL14 cluster. No SQLite fallback.
- `scripts/check-phase0-artifacts.sh`, `cargo fmt --all --check`,
  `cargo build --locked --workspace`,
  `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
  `cargo deny check`, and `cargo audit --ignore RUSTSEC-2023-0071` passed. The
  existing documented inactive SQLx/MySQL lock-only exception remains.
- Pinned mailmanclient and canonical Chromium integration passed. Chromium saves
  true, reloads/asserts both switches, then submits false to restore the fixture.
- `target/web-notices-process.py` passed on both SQLite and PostgreSQL: native
  browser login/settings → actual mailmanclient subscribe/unsubscribe APIs →
  durable publication → exact private SMTP. Six scenarios per database cover
  default-off, enabled, sibling default-off, enabled restart, disabled and disabled
  restart. Each backend observed exactly four expected notices, null envelopes,
  exact stored bytes matching SMTP DATA with recipient multiplicity, two attributed
  browser config edits, and no bounce events. Fixture writes use normal CLI/API/
  browser paths; SQL oracles are read-only. Runtime processes, SMTP sinks and the
  owned PG cluster were stopped, and temporary database directories were removed.
- **354 source/harness paths** were identical before/after the frozen run and
  independently matched afterward. The acceptance documentation was edited after
  this freeze and is checked separately, not attributed to the older manifest.
- Focused independent static review found no P1/P2. Its complete report is
  `target/web-notices-review.json`; all six reviewed full-file hashes were matched
  by the parent. The reviewer did not execute the parent-owned runtime gates or
  review the entire repository.

Process receipts: `target/web-notices-{sqlite,postgres}-receipt.json`.
Screenshots: `target/web-notices-{sqlite,postgres}.png` and the final gate's
`browser/14-list-settings.png`. The SQLite render was visually inspected.
These local `target/` receipts/harnesses are ignored development artifacts, not
files guaranteed in a clean clone. Rust regression coverage and the canonical
browser/PostgreSQL integration are retained in source.

## Remaining boundaries

This does not prove every membership entry point on every backend, exhaustive
races, late-COMMIT expiry, queued-notice replay across delivery failure, Internet
mailbox delivery, real Postfix/Exim deployment or full Mailman parity. The process
restart cases prove persisted configuration and new lifecycle operations after
restart, not a new test of an interrupted pending notice. Router-level browser
leave publication, native browser form operation and actual API→SMTP publication
are separate named evidence boundaries.

Incoming authentication/DSN/VERP/ARC, token/probe recovery, remaining account/admin
features, templates/localization, migration and production cutover obligations in
`MAILMAN_REPLACEMENT.md` remain open.
