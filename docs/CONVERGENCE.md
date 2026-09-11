## Canonical UI/login + held-recipient convergence

The UI is now an **integrated candidate**, not an accepted whole Mailman product.
The exact UI seed-relative delta and held-recipient semantic delta were applied
in canonical, preserving newer email commands, notice provenance, archive cooking
and live lease/final-ACK paths. `ReviewAction::Accept { max_attempts }` carries
intent only. API review and browser review resolve the effective roster under
SQLite writer reservation / PostgreSQL DML-conflicting table locks, then commit
Out + Digest + policy-enabled Archive, disposition and contextual audit together.
The composed policy/audit regression checks all three children, including zero
immediate recipients and rollback. Original SMTP spelling and frontend CSRF /
exact Origin checks remain intact. Login verifies Argon2 outside writer locks,
then binds exact credentials/address/predecessor to atomic issuance.

Migrations 0000–0012 coexist without renumbering 0009. Historical Phase 1 and
Phase 2 corpora are unchanged; `phase4-composed-schema.snapshot` is the additive
workflow/digest/archive/email/session ledger used by schema/repository tests.

Evidence: `/tmp/listmngr-canonical-convergence/`. `before.json` snapshots the
pre-integration source; `final-tests.log` records workspace all-targets PASS:
341 passed, 18 opt-in ignored across 63 targets. Locked workspace build PASS
(`final-build.log`); focused DB (51), API (59), runner (29) and archive gates also
passed. These overlapping runs are not summed. Chromium self-service PASS with
installed Google Chrome (`browser-chrome.log`, screenshots in `browser/`);
initial bundled-browser attempt failed because its executable was missing, not
because a form assertion failed. The browser uses a disposable DB token bridge,
not standalone SMTP acceptance. Final verification logs use the `verified-`
prefix; they follow removal of one unused test import reported by full Clippy.
No deliberate negative controls or source conflict markers remain.

Remaining gates/boundaries: composed PostgreSQL workspace and browser/login
DML-lock matrices, compatibility/deployment/hosted CI acceptance. Previous donor
PostgreSQL logs below are historical, not reruns of this canonical source.
Coarse PostgreSQL locks, GET-authorization concurrency, archive browsing/search
UX, complete administration and broader product/security parity remain open.
**Standalone email-only live acceptance remains BLOCKED: permission denied.**
No denied harness or equivalent live workflow was executed. No commit/push or
source-worktree cleanup was performed.

