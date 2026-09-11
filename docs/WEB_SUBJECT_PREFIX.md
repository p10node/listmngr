# Owner web subject prefix

Acceptance: **P4-WEB-SUBJECT-PREFIX — bounded local acceptance verified**.
This is not completion of the Mailman replacement roadmap.

## Behavior

Owners can edit **List administration → List settings → Subject prefix**.
The labelled input escapes HTML. Supplying an empty value clears the prefix;
omission preserves current storage and does not insert that key into the audit
patch. Spaces, tabs, plus signs and Unicode are not normalized. Shared CR/LF
validation rejects the complete update atomically. Existing session, live owner
and audit-transaction boundaries remain in force. No migration or new dependency.

## Frozen canonical gates

`target/web-prefix-gates-20260909-143205/final.json`: pass and source_stable true.
The parent read actual step exits and verified the manifest against current code:

- 17/17 steps exit zero.
- Locked workspace all-target tests: 582 passed, 0 failed, 43 ignored.
- Mandatory PostgreSQL gate: 26 passed, 0 failed, 0 ignored, including the dedicated
  `subject_prefix_controls::postgres_subject_prefix_controls` matrix.
- 362 source/harness hashes identical before/after gates. Only the three documented
  post-gate README/architecture/parity files differed at worker handoff; the
  worker's final nine-path manifest matched current files before parent doc edits.
- Artifact, fmt, locked build, strict all-feature Clippy, deny, audit, mailmanclient,
  native Chromium and inherited process regression gates passed.
- Audit retains the documented RUSTSEC-2023-0071 exception. Dependency duplicate
  warnings are allowed; this is not a warning-free dependency claim.

Historical sequential RED→GREEN commands:

```sh
cargo test --locked -p listmngr-api --test webui subject_prefix::owner_subject_prefix_post -- --exact
cargo test --locked -p listmngr-api --test webui subject_prefix::owner_subject_prefix_label_and_escaping -- --exact
```

Logs: `target/web-prefix-{red,green}-{post,input}.log`. The first RED rejected the
new POST field (422 rather than 303); after that adapter passed, the separate UI
RED detected the missing label. Supplementary controls first ran GREEN and are
not retrospective RED evidence. They cover omissions/empty/valid values, CR/LF,
duplicates, CSRF, cross-list authority, demoted/unverified owners, changed-write
audit sabotage, rollback and retry. PostgreSQL owns a schema and router; no
production rate limit was weakened.

The worker released source ownership in `target/web-prefix-handoff.md` after
finishing. Its gate process was confirmed absent before parent closure edits.
Worker manifests and patch remain historical artifacts, not hashes of these later
evidence-only documentation additions.

## Independent parent mail tracers

Separate from inherited emergency regression probes, the parent executed actual
native Chromium forms → LMTP intake → pipeline → loopback SMTP capture using
owned fixtures on **both SQLite and PostgreSQL**. Each backend passed seven cases:

1. Explicit default baseline.
2. Native save of `[Web %d] `, proving literal percent notation, not interpolation.
3. Unaffected sibling list with a different prefix.
4. Changed prefix survives process restart.
5. Explicit empty prefix removes it from subsequent composed mail.
6. Empty setting survives restart.
7. Exact leading/trailing spaces in `  [Final]  `.

Assertions compare exact ASCII Subject header bytes, body bytes and received
message multiplicity; old captures cannot satisfy fresh case identifiers.
PostgreSQL is a newly initialized, owned cluster, not a development database.

Artifacts and commands:

```sh
python3 target/web-prefix-parent-process.py
python3 target/web-prefix-parent-postgres-driver.py
```

- `target/web-prefix-parent-sqlite-receipt.json`: PASS, seven cases.
- `target/web-prefix-parent-postgres-receipt.json`: PASS, seven cases.
- `target/web-prefix-parent-postgres-evidence.json`: all init/start/create/process/
  stop exits zero; source/binary/probe unchanged and owned scratch removed.
- `target/web-prefix-parent-sqlite.png`: parent inspected labelled input/help;
  no observed clipping at the captured desktop viewport.

These ignored local scripts and receipts are local evidence, not a promise that
a fresh checkout has these artifacts. Canonical source tests and the browser
script remain in the repository. Repeated requested ad-hoc driver checks passed
but are not additional canonical full-suite acceptance runs.

## Compatibility limits

The composer treats the prefix literally. This increment does not add Mailman's
`Re:` repositioning, article-number expansion, missing-Subject insertion or full
subject-munging parity. It affects future composition, not previously sent mail.

At this accepted snapshot, Unicode storage and HTML roundtrip were proven, not
Unicode SMTP interoperability: the composer emitted a non-ASCII prefix as raw
header bytes and the SMTP client did not negotiate SMTPUTF8. A permissive SMTP
sink would not establish RFC6531 compliance. The subsequent bounded RFC2047
increment is documented in [UNICODE_SUBJECT_PREFIX.md](UNICODE_SUBJECT_PREFIX.md)
with separate evidence; it is not retroactively included in this snapshot.

No historical invalid-row repair, automatic resend, new in-flight revocation
contention proof, real-MTA cutover or whole-product completion is claimed.
