# Changelog

## [Unreleased]

### Added
- Phase 0 Rust workspace, CLI, health/readiness/metrics, pinned CI and hardened deployment artifacts; acceptance remains tracked per ID in `docs/FEATURE_PARITY.md`.
- Phase 1 domain model, portable SQLx repositories, REST APIs, scoped tokens, audit log, preference layering, typed ETags, CLI, PostgreSQL/SQLite CRUD, and `mailmanclient==3.3.5` compatibility acceptance.
- Disposable real-client acceptance harness and CI wiring for live PostgreSQL schema semantics and production-crate checks.
- CLI hidden/stdin/Unix-FD password input, typed redacted exit categories, HTTP service status, and regression coverage for token persistence/revocation/expiry, IDNA lookup, and network probe boundaries.

### Added
- Posting from the web (`P5-WEB-POST`): new threads and replies from the archive by signed-in members with a verified subscribed address, composed by the server and injected into the `in` queue with a web-origin context that the `in` runner admits like an `Approved:` post for unmoderated members.
- Archive interactions (`P5-INTERACTIONS`): one vote per reader per post with scores on every post, tags on threads with their own thread lists, an owner's category per thread, and a reader's favourite threads with their page; votes, tags and categories audited in the same transaction.
- Archive browsing (`P5-UI`): an overview with figures, months, recent and active threads and top posters; thread lists by latest activity and by month with a signed-in reader's unread marks cleared by the thread page; sender pages keyed by an address digest; the search page's result count and `<mark>` highlights; and Atom and RSS feeds.
- Archive search (`P5-SEARCH`): a tantivy index under `[archive] index_path` kept current by the archive runner in batches, ranked every-word search on the archive page with policy-checked hits and a database fallback, and `listmngr archive reindex`.
- Archive rendering (`P5-RENDER`): sender, date and parent indexed per post, threads as trees, attachments stored and served from their own path, text and safe-subset Markdown rendering with quote folding and address obfuscation, owner reattachment, and an opt-in proxied Gravatar.
- Phase 4 acceptance (`P4-ACCEPTANCE`): a phone-viewport Chromium journey (signup → verify → login → create list → subscribe and confirm → held post accepted → setting saved → logout) with axe-core on every stop, Lighthouse accessibility ≥ 95 on four pages and database assertions, plus a CI `browser` job running it and the slice harness with pinned tools.
- Data export and erasure (`P4-GDPR`): a reader's own JSON export from the account page, a server owner's export and typed-back erasure of any account (last owner excepted), and `listmngr user export` / `user erase` on the command line.
- Cross-list moderation (`P4-MODERATION-CROSS`): `/web/moderation` carries every held post and subscription request across the reader's lists with the list queue's forms, each naming its list, and decisions made there return there.
- System page and audit log (`P4-SYSTEM`): versions, the redacted configuration, queue depths by state with the oldest ready job's wait and the runners holding leases, MTA map status, and an audit log viewer filtered by action prefix and target.
- Domains and accounts in the browser (`P4-DOMAINS-USERS`): a domain index with an add form, a domain page with owners, template overrides on the list editor, the DKIM DNS record of each configured signing key and a typed-back deletion; an account search and an account page that saves the display name and server-owner flag, marks addresses verified or not, and lists memberships.
- List creation and the directory (`P4-LIST-CREATE-INDEX`): a create-list form for server and domain owners (name, domain, display name, first owner, style, directory visibility, description; inline refusals; one audited transaction that also seats the owner and regenerates the MTA maps), a directory that searches and filters by domain with the reader's role on each list and an opt-in scope for their unadvertised lists, and a list summary naming the addresses and policies that the people with a role on an unadvertised list can open.
- Moderator queues (`P4-HELD-QUEUE`): held posts with a rendered preview, decisions one or many at a time with a reason and a forward, the sender moderated or banned from the post, a header-rule shortcut, keyboard shortcuts from a first-party script, a subscription requests queue, and counts on the moderation index.
- Member management in the browser (`P4-MEMBERS`): rosters of every role with an htmx search swap and a full-page fallback, per-member options (posting policy, role, delivery and the other member-level preferences) with the effective values shown, bounce score reset, mass subscription from a textarea or a file with Mailman's workflow flags and per-address outcomes, mass removal by selection or pasted addresses, and a CSV export.
- List settings in the browser (`P4-LIST-SETTINGS`): the nine Postorius groups as forms on the REST patch engine with a write-nothing preview and inline refusals; header rules (add, edit, reorder, remove, test a value); list and site bans; a template catalogue and editor with placeholder preview; digest send/bump; archiver toggles; deleting a list by typing its id back.
- OpenID Connect sign-in (`P4-OIDC`): `[[web.oidc]]` providers on the login page, Authorization Code with PKCE and a verified ID token, just-in-time accounts for verified emails, auto-link by verified address, link/unlink under the account with the last-way-in rule; a provider sign-in still meets the two-step policy.
- Passkeys (`P4-WEBAUTHN`): register WebAuthn credentials under the account, sign in with one alone, remove with the password; a passkey counts as the second factor. First-party `passkeys.js` on the two pages that offer them, under `script-src 'self'`.
- Two-step sign-in with TOTP and recovery codes (`P4-TOTP`): enrolment with an inline QR code, a second login step, replay and brute-force limits, and the `security.require_2fa_for` policy that keeps server owners out of privileged pages until they enrol. `security.rate_limit.login` is now honoured instead of a fixed five per minute.
- `/web/account/delete`: self-service account deletion confirmed with the password — memberships, addresses, tokens, credential and sessions go in one audited transaction; the last server owner is refused (`P4-ACCOUNT-DELETE`).
- `/web/account/tokens`: mint, list and revoke API tokens within the reader's authority — list owners bind list-level scopes to their lists, server owners mint unbound ones — with the secret shown once (`P4-ACCOUNT-TOKENS`).
- `/web/account/addresses`: add addresses to the account (proven by the mailed token), choose the primary one, and remove others; removal keeps list subscriptions and forgets verification (`P4-ACCOUNT-ADDRESSES`). Signing up with a verified address nobody owns now requires proving it again.
- `/web/reset` and `/web/reset/confirm`: password reset by mailbox proof with a mailed single-use token, enumeration-safe responses, every session of the account ended, and one audited transaction per step (`P4-ACCOUNT-RESET`). The login page links reset and signup; its stale "not available" note is gone.
- `/web/signup` and `/web/verify`: self-service account creation with a mailed single-use verification token, enumeration-safe responses, and one audited transaction per step; `web.signup = false` switches it off (`P4-ACCOUNT-SIGNUP`).
- Site notices: mail the site sends outside any list, from `site.site_owner`, with site-scoped templates `site:user:action:verify` and `site:user:action:reset` in `en` and `vi`, delivered by the out runner with a null reverse path and signed for the owner's domain when a DKIM key exists (`P4-SITE-NOTICES`). No producer ships yet.
- `/web/account/profile` edits the reader's display name, interface language and time zone, audited in one transaction; the chosen language then wins over `Accept-Language` while signed in (`P4-ACCOUNT-PROFILE`). The same validator now guards the REST user patch.
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
- `deny.toml` ignores RUSTSEC-2023-0071 (`rsa` Marvin attack) with the recorded reason: the only `rsa` use is public-key signature verification of RS256 passkeys; no RSA private key exists in listmngr.
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
