# Unicode subject-prefix RFC2047 encoding

Acceptance ID: P2-UNICODE-SUBJECT-PREFIX.
Status: **bounded local acceptance verified**, including corrected composed gates.

The reviewed worktree delta is integrated into `crates/mail/src/cook.rs` and
`crates/mail/tests/unicode_subject_prefix.rs`. No dependency/schema/transport
change. Parent checked both files against candidate hashes and the previous
canonical cook source against the dirty seed before transferring this delta.

## Behavior

When the configured prefix contains non-ASCII text, the composer groups the
Subject with its continuation lines, decodes it using pinned mail-parser, and
adds the prefix only if that exact decoded prefix is absent. It emits the entire
result as UTF-8 Base64 RFC2047 words using the existing mail-builder encoder.
Chunks end on UTF-8 scalar boundaries and contain at most 42 bytes. Encoded words
are at most 68 bytes; Subject lines at most 77, continuations at most 69 (excluding
line endings). This is not a new aggregate header/allocation quota.

ASCII, empty and absent prefixes retain the previous path. An absent Subject is
not invented. Other retained fields and MIME/body bytes are not reserialized.
CR/LF injection guards remain. No Re: repositioning or article-number expansion.
The MIME parser is tolerant: malformed encodings do not gain a lossless-decoding
or complete RFC5322-validation guarantee. Duplicate Subject fields are processed
individually, not normalized into a single field.

This fixes the Unicode-prefix producer, not SMTPUTF8 negotiation, international
SMTP envelopes, other raw Unicode headers or binary-body SMTP interoperability.

## Evidence

The isolated candidate had three sequential behavioral RED→GREEN cycles:
raw non-ASCII output; duplicate prefix on repeat cooking; and an overlong
Q-encoded word/line from the initial serializer at a Unicode boundary. The final
implementation uses bounded Base64 words rather than relaxing those assertions.

The parent independently ran the six new tests in the worktree with unchanged
candidate hashes, then transferred the exact reviewed bytes to canonical and ran:

```sh
cargo test --locked -p listmngr-mail
cargo clippy --locked -p listmngr-mail --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo build --locked --workspace
```

All passed. Tests cover Vietnamese/emoji/decomposed accents, mixed Q/B existing
subjects, folding, LF/CRLF, long prefixes and scalar boundaries, repeat cooking,
missing/empty fields, injection rejection and exact binary MIME preservation.

A separate parent native Chromium → LMTP → pipeline → loopback SMTP tracer passed
seven cases on SQLite. Python's independent `email` parser decoded the received
RFC2047 subject to the exact Vietnamese/emoji prefix plus original subject;
all fixture headers were ASCII, body bytes and message multiplicity matched.
Changed/empty restart and sibling/default ASCII controls also passed.
Receipt: `target/unicode-prefix-parent-sqlite-receipt.json`.
Command: `target/replacement-verifier-venv/bin/python target/unicode-prefix-parent-process.py`.
The first attempt used the system interpreter and failed before fixture startup
because mailmanclient was unavailable; the pinned existing verifier venv passed.
This environment error is not a product RED or a passing run.

The first canonical run, `target/unicode-prefix-gates-20260909-151318/`, failed
the shared DB subject-prefix consumer assertion on both SQLite and PostgreSQL:
it still required raw UTF-8 Subject bytes. Native Unicode SMTP tracers passed on
both databases, but that does not supersede the failed gate. The run also observed
the subsequent DB-test edit before its final fingerprint and is not acceptance.

The parent reproduced the exact SQLite failure (exit101), then updated only the
Unicode consumer oracle in `crates/db/tests/subject_prefix.rs`: decoded subject
must match the saved prefix plus original text, wire bytes must be ASCII and the
body must remain exact. ASCII byte assertions and all persistence/validation/
audit rollback assertions remain; an additional emoji value exercises the same
contract. `target/unicode-prefix-db-oracle-green.log` records 3 passed, 1 explicitly
ignored PostgreSQL test. Strict DB Clippy and fmt passed. This is an intentional
wire-contract reconciliation, not a relaxed check or a DB production-code change.

## Final corrected frozen run

`target/unicode-prefix-gates-20260909-153157/final.json`: pass=true,
source_stable=true; parent checked all 18 actual step exits and current hashes.

- Workspace: 588 passed, 0 failed, 43 ignored.
- Mandatory PostgreSQL: 26 passed, 0 failed, 0 ignored, including the corrected
  shared subject-prefix contract. No silent SQLite fallback.
- 367 source/harness hashes identical before/after and to canonical at closure.
- Native Chromium→LMTP→SMTP Unicode tracers: seven cases per database, actual
  ASCII headers decoded independently using Python email, exact body/multiplicity,
  changed/empty restart and sibling controls. Both backend receipts say PASS.
- Artifact/fmt/locked build, full strict Clippy, deny, audit, inherited SQLite
  emergency regression, mailmanclient, native browser and diff gates passed.
  Audit keeps the documented RUSTSEC-2023-0071 exception.
- Owned PostgreSQL scratch was removed. The integrated worktree was removed only
  after all deliverable bytes and original candidate notes were proved preserved.
  Existing unrelated worktrees and canonical dirty data were not cleaned/reset.

These final evidence-only README/architecture/parity/acceptance-doc changes follow
the frozen gates and are separately verified; they do not retroactively change
the first failed run or its hashes. No real external mail, deployment, commits
or full Mailman replacement claim. Local candidate evidence is preserved under
`target/unicode-prefix-candidate-evidence/`; it is not a fresh-checkout artifact.
