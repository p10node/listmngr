# Changelog

## [Unreleased]

### Added
- Phase 0 Rust workspace, CLI, health/readiness/metrics, pinned CI and hardened deployment artifacts; acceptance remains tracked per ID in `docs/FEATURE_PARITY.md`.
- Phase 1 domain model, portable SQLx repositories, REST APIs, scoped tokens, audit log, preference layering, typed ETags, CLI, PostgreSQL/SQLite CRUD, and `mailmanclient==3.3.5` compatibility acceptance.
- Disposable real-client acceptance harness and CI wiring for live PostgreSQL schema semantics and production-crate checks.
- CLI hidden/stdin/Unix-FD password input, typed redacted exit categories, HTTP service status, and regression coverage for token persistence/revocation/expiry, IDNA lookup, and network probe boundaries.

### Added
- `/web/account/sessions` lists the reader's own signed-in browsers and ends one or every other session, each revocation audited in its own transaction (`P4-ACCOUNT-SESSIONS`).

### Changed
- Browser pages render from one Askama template set with compile-time auto-escaping (`P4-SHELL`); handlers build view models and no longer concatenate markup. Output stays byte-compatible with the previous escaper.
- The browser shell takes its strings from the shared Fluent catalog and negotiates the document language per request from `Accept-Language`, then `site.default_language`, then English; `en` and `vi` ship.
- The stylesheet is design tokens both colour schemes bind, so the browser surface follows `prefers-color-scheme` without per-page markup.

### Fixed
- Scoped-user API authorization uses portable PostgreSQL/SQLite bind parameters; live PostgreSQL regression checks allowed and forbidden users under both API prefixes.
- Production-crate gate accepts Cargo's null package metadata without skipping empty current-phase crates; regression tests cover both outcomes.
- PostgreSQL contract helpers satisfy the blocking Clippy gate without weakening its assertions or lint policy.
- Alpine builder installs exact-version musl C headers needed by `ring` and bundled SQLite; the scratch runtime remains unchanged.
- CLI status no longer reports success from database connectivity while the HTTP service is stopped. Wildcard listeners probe loopback; proxies and redirects are disabled.
- CLI member find/delete validate normalized IDNA addresses; adapter validation and missing-resource errors retain their stable exit categories.

### Security
- `/api/docs` no longer loads Swagger UI from a CDN (`P1-API-DOCS-ORIGIN`). The API reference is rendered from this server's own OpenAPI document, contains no script, and carries the strict browser CSP; `/openapi.json` still serves any external explorer.
- htmx 2.0.10 is vendored (0BSD) and served from this origin with its SHA-384 pinned by a test; no browser page loads a third-party asset, and pages keep `default-src 'none'` with no script element.
- `rustls` moved to 0.23.45 in the lockfile for RUSTSEC-2026-0285 (TLS 1.3 handshake messages accepted across encryption-level boundaries).
- Static musl/scratch non-root container, filtered Docker context, runtime-only PostgreSQL credentials, hardened systemd unit, and explicit deny/audit policy.
- Full GNU Affero General Public License v3 text and RFC 9116-style `security.txt` disclosure metadata.
- Removed argv password input, bounded password streams without truncation, and made secret-file read diagnostics generic. Automation callers now feed passwords via stdin.

### Decisions
- Package version starts at 0.1.0 for unreleased Phase 1 development; it is not a release or phase-completion tag.
- Project license is AGPL-3.0-or-later.
- Available stable dependency versions replace unavailable future PLAN pins; see ADR-0003.
