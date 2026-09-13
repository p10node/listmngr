# Read-only DSN inspection

Acceptance ID: P2-DSN-INSPECTION.
Status: bounded local acceptance verified on the corrected frozen source.

## Operator behavior

`listmngr queue show <job-id> --dsn` reads an existing bounce-queue message and
prints JSON containing `untrusted: true` and recipient claims (`final_recipient`,
`action`, `status`). It does not acknowledge, lease, score, disable, notify, retry,
or resolve a delivery. Missing/ordinary jobs and unsupported/malformed reports
fail without displaying raw diagnostic contents. `--dsn` conflicts with `--raw`.
Default metadata inspection and explicit raw-byte export remain unchanged.

Only explicit DSN inspection displays recipient claims. Human explanation,
Diagnostic-Code, returned original message and other extension fields are not
included. Recipient values are untrusted typed strings, not normalized mailboxes,
verified identities or correlation capabilities.

## Deliberately bounded parser contract

`crates/mail/src/dsn.rs` uses the existing pinned mail-parser MIME representation,
then validates/unfolds the delivery-status field blocks separately:

- Outer multipart/report, report-type=delivery-status, two or three direct parts.
- Explicit text/plain human part, followed by message/delivery-status; an optional
  final message/rfc822 or text/rfc822-headers part is ignored, not searched for
  recipient authority. Nested delivery-status structures are not supported.
- Explicit unique Content-Type and absent or 7bit Content-Transfer-Encoding on
  those parts; duplicated parsed Content-Type attributes/CTE headers, unsupported
  encodings and missing closing boundary are rejected. Independently scanned
  raw CRLF boundary lines (only SP/TAB padding) must match every direct part's
  header/body-end offsets and part count; best-effort MIME truncation is rejected.
  This is not a complete
  MIME-conformance validator or a mail-authentication mechanism.
- ASCII delivery-status body with CRLF field lines and whitespace unfolding;
  dns Reporting-MTA plus at least one recipient block. Every recipient must have
  exactly one rfc822 Final-Recipient, recognized Action and numeric Status.
  Duplicate field names (including extensions) or an invalid trailing block reject
  the entire report; no partial recipient result is returned.
- Action is case-insensitive and output lowercase. Status is three components:
  class 2/4/5, followed by two one-to-three-digit components, preserved as text.
  Values are inspected, not interpreted as authorization or delivery identity.
- Inclusive budgets: 256 KiB raw before MIME parsing; 64 KiB status body before
  unfolding; 100 recipients; 32 fields per block; 998 bytes per physical status
  line excluding CRLF. Raw-message fetching still follows the existing DB API;
  these parser budgets are not DB-fetch/process-memory quotas.

RFC3464 section 2.3.3 explicitly allows failed + 4.x after retry is abandoned.
Do not collapse Action into Status class. The inspector preserves separate claims
and does not infer whether they justify any automated action.

Reference: https://www.rfc-editor.org/rfc/rfc3464.txt (sections 2.1–2.3).
The RFC was retrieved directly into the local target reference file; synthetic
fixtures exercise its field grammar and failed/4.x rule, not a live reporting MTA.

## Evidence and limits

Review P2 was reproduced: `Status: 5.1.1--r--garbage` could hide a following
duplicate Status. `target/dsn-framing-red.log` records the actual assertion
failure, not merely the static finding. Raw framing/offset agreement repairs
the false-boundary class. A second RED (`target/dsn-padding-red.log`) caught
overbroad whitespace trimming; padding now accepts only SP/TAB. Eight parser
tests and both native CLI tests plus three bounce-inbox regressions pass.
The initial `target/dsn-inspection-gates-20260909-164017/final.json` is PASS and
source-stable but predates this correction; it is superseded, not acceptance of
the corrected inspector. It finished naturally; no denied termination ran.

- Native CLI RED: unknown --dsn rejected with CLI-USAGE; GREEN: real durable
  intake handler → bounce queue → newly built CLI → independent JSON/state reads.
  An initial wrong Cargo package name and fixture type-inference compile error
  were setup mistakes, not behavioral RED evidence.
- Sequential parser RED→GREEN: duplicate/required field checks and folded values;
  malformed MIME/unterminated report and resource limits. Supplementary inclusive
  limits, multi-recipient and invalid-tail tests retain whole-input rejection.
- CLI positive/negative differential: Alice failed/5.1.1 versus Bob delayed/4.2.2;
  existing Alice member/preferences survive unchanged. Ordinary/opaque bounce
  rejection, --raw conflict and exact export preserve state, raw and audit.
- Focused logs under `target/dsn-*.log` are local evidence, not fresh-checkout
  artifacts. Corrected composed acceptance is recorded below.

## Corrected composed acceptance

`python3 -u target/dsn-inspection-gates.py` completed with exit 0 and final
`pass=true, source_stable=true` at
`target/dsn-inspection-gates-20260909-171235/`. All 20 commands passed;
372 source/harness/document paths matched before, after and at parent verification.
Workspace: 598 passed / 0 failed / 43 ignored. Mandatory owned PostgreSQL gate:
26 passed / 0 failed / 0 ignored. Build, fmt, strict all-feature workspace Clippy,
artifact/diff contracts, cargo-deny and cargo-audit passed; the documented
RUSTSEC-2023-0071 inactive SQLx/MySQL lock-only exception remains.

The eight focused parser tests and five CLI/inbox tests ran explicitly as well
as in the workspace. Native DSN CLI fixtures are SQLite, not a new PostgreSQL
CLI matrix. Existing Unicode browser→LMTP→SMTP tracers on both engines,
emergency runtime, mailmanclient and Chromium acceptance also passed as
regressions; these do not establish authenticated incoming DSN processing.
PostgreSQL was a newly initialized loopback-only fixture, stopped and removed.

Bounded independent static rereview reported no remaining P1/P2. Parent verified
its five source/document hashes (`target/dsn-inspection-review.json`); the review
did not run tests. Acceptance prose was updated after the gate, with artifact,
fmt/diff and source-vs-document-only closure checks performed separately.

No HMAC return path, token route, sender authentication, incoming bounce worker,
replay fence, incarnation/expiry/key rotation, transactional DSN scoring or
full Mailman replacement is claimed. Outbound-issued correlation remains required
before any future DSN-to-member mutation path. Existing SMTP-failure scoring and
operator acknowledge behavior are untouched. No live data or real mail is used.
