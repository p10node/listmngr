# Owner emergency moderation

Acceptance ID: `P4-WEB-EMERGENCY` — bounded local acceptance verified.

## Behavior

A verified list owner or server owner can use **List administration → List
settings → Emergency moderation** to hold otherwise eligible new posts. This
reuses the existing policy: null reverse paths, bans and loop handling run first;
emergency holds precede ordinary posting acceptance. No policy ordering changed.

This is **not a delivery shutdown**. Already queued mail and explicit moderator
approvals can still be delivered. Disabling emergency moderation does not release
existing held messages. The UI explains this distinction and shows the persisted
Yes/No value. Default behavior remains unchanged.

The POST field is optional. Omission preserves the current value and omits the
key from the audit patch; explicit false disables it. Only exact true/false are
accepted, with blanks, alternate spellings/cases, whitespace and duplicate keys
rejected. The existing live authority, CSRF/Origin, locked list validator and
atomic attributed audit path remain authoritative. No schema or dependency change.

## Executed evidence

Sequential behavioral RED→GREENs used:

`cargo test --locked -p listmngr-api --test webui emergency::`

1. POST with emergency failed HTTP422 versus303 before extraction support, then
   passed enabled/disabled and legacy-omission persistence/audit checks.
   Logs: `target/web-emergency-post-{red,green}.log`.
2. The test was extended to assert labels, current selections and shutdown caveat;
   it failed on the missing label before UI implementation and passed afterward.
   Logs: `target/web-emergency-ui-{red,green}.log`.

Supplementary strict-boolean/duplicate checks assert unchanged state and audit.
The existing owner matrix now includes emergency in valid forms, authority and
CSRF/Origin denials, and changes it in the injected-audit-failure rollback case.
Legacy omission is tested with both existing true and false values. A dedicated
owned-schema/router PostgreSQL test runs the emergency controls and is registered
in `scripts/test-postgres.sh`; its schema is dropped after success or a spawned
test assertion panic. The canonical Chromium script saves true and reloads it,
then restores false for the rest of its flow.

Final command: `python3 -u target/web-emergency-gates.py`.
Final directory: `target/web-emergency-gates-20260909-133035/`.

- **17/17 commands passed**; `final.json` reports pass and stable source.
- `cargo test --locked --workspace --all-targets`: **575 passed, 0 failed,
  41 ignored**. Ignored opt-in tests are not counted as passing.
- Separate mandatory PostgreSQL gate: **24 passed, 0 failed, 0 ignored**, using
  an owned disposable local PostgreSQL14 cluster, with no SQLite fallback.
- Required artifact, format, locked workspace build, strict all-target/all-feature
  Clippy and dependency checks passed, including `cargo deny check` and
  `cargo audit --ignore RUSTSEC-2023-0071`. The existing documented inactive
  SQLx/MySQL lock-only exception remains.
- Pinned mailmanclient and canonical Chromium integrations passed.
- `target/web-emergency-process.py` passed six native browser→LMTP→held/SMTP
  cases on **each** database: disabled baseline, enabled hold, unaffected sibling,
  enabled restart, disabled-restored delivery and disabled restart. The fixture
  sets posting defaults to accept, proving emergency is the reason for holds.
  Two held messages remain pending after disabling and another restart; held
  messages create no delivery child jobs. Fresh payloads and SMTP Message-ID
  multiplicity are checked, as are two attributed native browser edits. API/CLI
  and browser operations own writes; SQL oracles are read-only. The application,
  SMTP sink and owned PostgreSQL cluster were stopped and scratch removed.
- **356 source/harness paths** matched before/after the frozen final run and an
  independent read-back. Acceptance documents were edited afterward and checked
  separately; the earlier manifest is not claimed to include those later bytes.

Receipts: `target/web-emergency-{sqlite,postgres}-receipt.json`.
Screenshots: `target/web-emergency-{sqlite,postgres}.png` and the final gate's
`browser/14-list-settings.png`. The SQLite render was visually inspected.
These `target/` harnesses/receipts are ignored local evidence, not guaranteed in a
clean clone. Rust controls and canonical browser/PostgreSQL registration remain
in repository source.

## Earlier failed run and repair

`target/web-emergency-gates-20260909-131034/` is **not passing acceptance**.
Its PostgreSQL test returned HTTP429 at login after independent feature matrices
were appended to the same router and exhausted its authentication bucket. The
repair isolates emergency in its own schema/router rather than weakening the
product rate limiter, sleeping out its window or altering development data. The
complete final gate above was rerun after this test-only correction. The earlier
successful process probe alone was not used to excuse the failed mandatory gate.

## Limits

This increment does not add a global SMTP pause, recall, automatic mass release,
new moderator-approval semantics, exhaustive race/expiry evidence, Internet
mailbox delivery or real-MTA cutover. The dedicated process scenarios do not
newly exercise approval while emergency is enabled; that limitation and queued
mail behavior are explained from the existing control paths, not advertised as
new execution coverage. Full Mailman replacement remains open as recorded in
`MAILMAN_REPLACEMENT.md`.
