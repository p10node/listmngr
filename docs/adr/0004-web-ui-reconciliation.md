# ADR-0004: The web UI stays server-rendered; string-built pages are interim

- Status: Accepted
- Date: 2026-09-14

## Context

Three descriptions of the browser UI had drifted apart:

- `docs/PLAN.md` §2 and ADR-0002 specify Askama templates, vendored htmx and
  plain CSS with no Node runtime.
- One `docs/FEATURE_PARITY.md` row referred to a "Stage D SPA", while another
  row in the same ledger referred to the "SSR archive (D4)". The stage plan
  those letters came from was never committed to the repository.
- The shipped pages (`crates/api/src/webui.rs`, `crates/web`) build HTML with
  `format!` and a hand-written escaper: no template engine, no htmx, no CSP
  nonce. Twenty routes exist this way.

## Decision

1. ADR-0002 stands. The UI is server-rendered; there is no SPA and no Node
   toolchain. The "SPA" wording in the ledger was drift and is corrected.
2. Askama is the template engine. htmx 2.x is vendored with a pinned hash and
   used only for progressive enhancement; every form works without JavaScript.
   Pages carry a per-response CSP nonce; no inline script without it.
3. The `format!`-built pages are an interim slice. Work package `P4-SHELL`
   introduces the layout, tokens and template infrastructure and migrates the
   existing routes; later Phase 4 packages render every new screen through
   templates. Two rendering paths do not coexist after `P4-SHELL` merges.
4. Stage letters (A–F) are retired. Work packages are named `P<phase>-<NAME>`;
   A–C map to Phases 2–3, D to Phase 4, E to Phase 5, F to Phase 6.
5. One acceptance ID is one branch, merged to `main` after the gates pass.

## Consequences

- One validation path (server) and one rendering path (templates); browser
  tests remain HTTP-level plus Playwright without a bundler.
- `crates/web` shrinks to shared presentation helpers or is folded into the
  template crate; its escaper is replaced by Askama's auto-escaping.
- The ledger's `P4-WEB-*` rows keep their evidence; their pages are re-verified
  when migrated under `P4-SHELL`.
