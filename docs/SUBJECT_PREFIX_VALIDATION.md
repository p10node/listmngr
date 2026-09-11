# Subject-prefix configuration validation

Acceptance: `P1-SUBJECT-PREFIX-VALIDATION` — bounded local acceptance verified.

## Fix

The existing header composer rejects CR/LF in a subject prefix, but the common
list configuration transaction previously accepted those strings. This could
save configuration that fails later during mail preparation. The composer was
already fail-closed; this is not evidence that an injected header was delivered.

`crates/db/src/lib.rs`, `Lists::update_tx`, now rejects CR/LF when the
`subject_prefix` key is supplied. It returns the ordinary validation error and
preserves the entire prior list and audit. No trimming or escaping is performed.
Unicode, spaces/tabs and an empty prefix retain their bytes. Multiline
`description`/`info` remain valid. Existing header defense remains unchanged.

The common transaction is used by the existing config APIs, rather than adding
a frontend-only filter. Both `/api/v1` and `/3.1`, JSON and URL-encoded forms,
PATCH and PUT return HTTP400 for the tested invalid prefixes. Valid Unicode
prefix changes and explicit clearing succeed.

## Executed tests

Historical RED and GREEN:

`cargo test --locked -p listmngr-db --test subject_prefix`

- `target/subject-prefix-red.log`: the real composer rejected the fixture's
  CRLF prefix, while the repository accepted it; the rejection assertion failed.
- `target/subject-prefix-green.log`: the same test passed after the fix.

Supplementary regressions, added after that RED→GREEN:

- `crates/db/tests/subject_prefix.rs`: four differential valid values persisted
  and passed into the actual composer, with exact header/body output comparison.
  Bare CR, LF, CRLF folding and header/body breaks, plus wrong JSON types, reject
  mixed-field updates without changing the complete list snapshot or audit count.
  Multiline prose and prefix omission remain supported.
- SQLite audit-insert sabotage rejects a valid changed prefix and preserves the
  old prefix/audit; removing the fixture trigger permits the valid retry.
- An isolated PostgreSQL schema runs the validation/consumer contract, with
  cleanup after success or a spawned assertion panic. The canonical
  `scripts/test-postgres.sh` explicitly invokes it; no SQLite fallback.
- `cargo test --locked -p listmngr-api --test rest subject_prefix::`: the real
  router checks both API flavors and both input encodings/methods, including
  complete config readback after rejected mixed-field updates.

## Final gates

Command: `python3 -u target/subject-prefix-gates.py`.
Evidence: `target/subject-prefix-gates-20260909-135939/`.

- `final.json`: PASS, source stable; `results.json`: all **17 commands exit0**.
- Workspace: **579 passed, 0 failed, 42 ignored**. Ignored tests are not PASS.
- Separate mandatory PostgreSQL gate: **25 passed, 0 failed, 0 ignored**.
- Required artifact check, format, locked workspace build, strict all-target /
  all-feature Clippy, `cargo deny check`, and
  `cargo audit --ignore RUSTSEC-2023-0071` passed. The pre-existing documented
  SQLx/MySQL lock-only advisory exception remains; it is not an exception-free audit.
- Canonical mailmanclient/Chromium and the existing emergency browser→LMTP→SMTP
  probes on both databases passed as regressions. They are **not a new
  prefix-specific HTTP→SMTP tracer**.
- All **359 source/harness paths** matched before/after and independent readback.
  README/architecture/parity and this evidence document were updated afterward;
  post-documentation checks are separate from the frozen code run.
- Owned PostgreSQL cluster stopped and its scratch directory was removed.

The local ignored `target/` driver imports the earlier gate helper; its presence
is not guaranteed in a clean clone. Retained Rust tests and PostgreSQL script
registration are the repeatable repository-level coverage.

## Limits and existing data

No browser control, migration, dependency or transport change. No historical
invalid row is scanned, rewritten or deleted, and no failed job is automatically
replayed. An operator must explicitly replace any historical invalid prefix via
the existing config API and evaluate queue recovery separately. Unrelated PATCH
operations do not silently repair historical values. This guard aligns CR/LF
admission with the existing composer; it is not a complete new RFC header or
internationalization validator. No new PostgreSQL audit-trigger sabotage,
concurrent prefix-specific race, prefix-specific SMTP delivery, live-MTA cutover
or full Mailman replacement claim is made.
