# Owner web posting limits

Acceptance ID: `P4-WEB-POSTING-LIMITS`.
Status: bounded local acceptance verified, not complete Mailman replacement.

## Behavior

Open **List administration → List settings** as a verified list owner or verified
server owner. The form exposes two existing list policies:

- **Maximum message size (KiB)** (`max_message_size`): hold an original post only
  when its byte size exceeds the configured KiB limit. Headers and attachments
  count. Zero disables this per-list policy, not the server's intake limit.
- **To/Cc recipient hold threshold** (`max_num_recipients`): hold at or above the
  visible mailbox count. Repeated addresses count; Bcc, body text and subscriber
  roster size do not. Unknown/malformed visible headers are held when enabled.
  Zero disables this check.

Both fields accept integers in `0..2147483647`. Browser min/max/step/required
attributes are usability constraints, not the security boundary: the existing
repository validator independently enforces the range. The form carries the
current values and accessible labels/help. No schema, dependencies, transport
settings or default delivery policy changed.

## Compatibility, authority and atomicity

The existing owner settings POST uses optional integer fields. Omission of either
field preserves its current stored value and omits that key from the audit patch;
an explicit zero disables only the supplied policy. This preserves old form
clients. Unrelated list fields are read from the current locked row, not restored
from an earlier page render. Explicit values submitted from an old page still
represent an ordinary owner edit, not an optimistic-concurrency protocol.

The existing CSRF/configured-Origin and live session checks remain in place.
`browser_update_list_settings` verifies current owner authority, updates through
the ordinary list validator and commits configuration plus the attributed
`list.config` audit in one transaction. Owner revocation, unverified identity,
foreign-list writes and revoked credentials cannot use these fields as a bypass.
An injected audit failure rolls back changed limit values along with other edits.

## Executed evidence

Sequential behavioral TDD (not retrospective attribution):

1. `cargo test --locked -p listmngr-api --test webui posting_limits::owner_can_save_posting_limits -- --exact`
   failed with HTTP422 versus303 before optional-field support, then passed.
   Logs: `target/web-posting-limits-post-{red,green}.log`.
2. `cargo test --locked -p listmngr-api --test webui posting_limits::owner_form_exposes_current_posting_limits_and_units -- --exact`
   failed on the missing size label, then passed after rendering the controls.
   Logs: `target/web-posting-limits-ui-{red,green}.log` (GREEN ran the module).

Supplementary controls cover omission of both/one field, explicit zero, distinct
nonzero values, exact maximum, overflow, negative/fractional/blank/invalid values,
duplicates, preserved unrelated settings and exact supplied-key audit content.
The existing owner matrix now exercises the new values through CSRF/Origin,
foreign-list/role/verification/credential denials and audit-failure rollback.
The PostgreSQL test owns an isolated schema and drops it after success or a child
assertion failure. It is registered in the mandatory `scripts/test-postgres.sh`.

Final frozen run:

- Command: `python3 -u target/web-posting-limits-gates.py`.
- Directory: `target/web-posting-limits-gates-20260909-113407/`.
- `results.json`: **17/17 commands passed**; `final.json`: pass and source_stable.
- `cargo test --locked --workspace --all-targets`: **571 passed, 0 failed,
  40 ignored**. Ignored opt-in tests are not counted as passing.
- Separate mandatory PostgreSQL gate: **23 passed, 0 failed, 0 ignored**, on an
  owned disposable local PostgreSQL 14 cluster, never falling back to SQLite.
- Required artifact check, workspace format/build, strict all-target/all-feature
  Clippy, `cargo deny check` and `cargo audit --ignore RUSTSEC-2023-0071` passed.
  The existing documented inactive SQLx/MySQL lock-only exception is retained.
- Actual pinned mailmanclient integration and the canonical Chromium browser
  suite passed. The browser script now fills and independently reloads both
  numeric controls, then restores zero through the native form.
- `target/web-posting-limits-process.py` passed **12 cases per engine** on SQLite
  and PostgreSQL: browser save → persistent configuration → real LMTP intake →
  held decision or real SMTP DATA. Cases distinguish size equality/overflow,
  recipient below/equality/malformed, unaffected sibling lists and restarts in
  enabled/disabled states. Exact fresh body bytes, per-recipient Message-ID
  multiplicity, no child jobs for held posts and three attributed browser edits
  are independently checked. SQL assertions are read-only; scenario writes use
  CLI/API/native forms. Process fixtures, SMTP sinks and the owned PG cluster
  were stopped and temporary scratch removed.
- Source/harness manifests contained **352 paths** and matched before/after the
  frozen run and a subsequent independent read-back. Acceptance documentation
  was updated afterward; it is not falsely included in the earlier frozen hash.
- A separate focused read-only static review found no P1/P2 in the new form,
  tests and relevant shared authority/validator/audit path. It did not execute
  the parent's runtime gates or review the entire repository.

Process receipts: `target/posting-limits-{sqlite,postgres}-receipt.json`.
Screenshots: `target/posting-limits-{sqlite,postgres}.png` and the frozen run's
`browser/14-list-settings.png`. These are local development evidence under the
ignored build directory, not distributable artifacts guaranteed in a clean clone.
The Rust controls and canonical browser/PostgreSQL registration are in source.

## Boundaries

This increment exposes already-implemented posting policies. It does not change
held-review bypass semantics, recall previously published mail, provide incoming
sender authentication, implement exhaustive RFC header conformance, establish a
new lock-contention/late-COMMIT expiry matrix, or prove real Postfix/Exim cutover.
The local SMTP fixture is not delivery to an Internet mailbox. Full account,
list administration, template/localization, migration, DSN/VERP/ARC and remaining
replacement obligations in `PLAN.md` and `MAILMAN_REPLACEMENT.md` remain open.

The preceding `P4-LIST-COPY` run was also recovered and verified directly:
`target/list-copy-parent-gates-20260909-100711/` completed 63/63 gates,
workspace568/0/39, PostgreSQL22/0/0 and both-backend browser/SMTP/restart. Its 349
frozen paths matched before this increment. The historical pending handoff is
superseded, not treated as a fresh whole-product acceptance claim.
