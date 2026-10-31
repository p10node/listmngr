# listmngr — Detailed plan

> Mailman 3 alternative (Core + Postorius + HyperKitty + mailman-web + django-mailman3 + mailmanclient) written in Rust.
> Single binary, feature parity, modern UI, security-first.

## Implementation status (2026-09-14) — supersedes the 2026-09-06/07 checkpoints

**Update 2026-10-09 (`P10-1.2.1`):** `v1.2.0` was pushed (tag alone) but the release
run did not complete (`publish` failed; image/chart 1.2.0 exist on GHCR, no GitHub
release); pipeline fixed (`P10-IMAGE-NATIVE`: native build per architecture, publish
downloads only explicitly named artifacts) + `mime_delete` fuzz crash fix → patch
`1.2.1`, tagged locally, pushed alone. Remaining: backlog at the end of Phase 8.

**Update 2026-10-08 (`P10-1.2`):** Phase 9–10 closed; `1.2.0` is the third release
(Helm chart really installed on kind in CI, PostgreSQL + Postfix sidecar in the chart,
OCI publish to GHCR); tag `v1.2.0` created locally, pushed **alone** (GitHub skips the
event when >3 refs are pushed at once — the reason `v1.0.0`/`v1.1.0` never ran a
release). Remaining: backlog at the end of Phase 8.

**Update 2026-10-05 (`P8-LEDGER-RECONCILE`, `P8-1.1`):** Phase 0–8 closed per the
ledger; `1.0.0` is the first release (2026-10-04) and `1.1.0` the second
(2026-10-05), both tags created locally, pushing is the user's decision. What
remains after 1.1 is the backlog at the end of §7 Phase 8 (external review, MySQL, WASM, SDK, ZAP). The paragraphs below are
a 2026-09-14 snapshot, kept as-is for comparison; the sentence "Not yet: all of Phase 4–7
except …" is no longer true.

This document is the **canonical product contract and roadmap**, not an inventory
of delivered features. Evidence per acceptance ID lives in
[FEATURE_PARITY.md](FEATURE_PARITY.md); the implemented surface is described in
ACCEPTANCE_NOTES/ARCHITECTURE; `Cargo.lock` is the source of dependency versions (ADR-0003). The
2026-09-06/07 checkpoints (mail path "plaintext trusted relay", "no current
PostgreSQL PASS", R1/O1 OPEN) have been superseded; the old content is kept in git history.

Evidence exists (bounded local acceptance, ledger ID in parentheses):

- Phase 0/1: complete (`P0-*`, `P1-*`).
- Phase 2 mail path: LMTP (P2-TRANSPORT, P2-LMTP-PARAMETERS); store/queue/runtime
  (P2-STORE, P2-QUEUE, P2-RUNTIME, P2-LEASE-HEARTBEAT); chain/rule engine with
  15 rules, header-match and DMARC (P2-CHAIN-ENGINE, P2-CHAIN-RULES,
  P2-DMARC-MUNGE, P2-VALIDATE-AUTHENTICITY, P2-HEADER-MATCHES-REST); handler pipeline except
  `to-usenet`/`arc-sign` (P2-PIPELINE-HANDLERS, P2-MIME-DELETE,
  P2-HANDLERS-DECORATE, P2-COOK-HEADERS, P2-PERSONALIZE-VERP); templates
  (P2-TEMPLATES); held REST + notices; out runner (P2-DELIVERY-POLICY,
  P2-RECIPIENT-LIMIT, P2-STARTTLS, P2-SMTP-AUTH, P3-DKIM); RFC 2369/8058
  (P2-ONE-CLICK-UNSUBSCRIBE); MTA maps (P2-MTA-INTEGRATION); metrics (P2-METRICS);
  `/queues` (P2-QUEUES-REST).
- Phase 3: subscription policies/requests/invitations (P3-SUBSCRIPTION-POLICY,
  P3-SUBSCRIPTION-REQUESTS-REST, P3-ADMIN-SUBSCRIBE); email commands +
  autoresponder (P3-EMAIL-COMMANDS, P3-AUTORESPONDER, P3-HELP-REPLY); bounces
  (P3-BOUNCE-RUNNER, -DETECTORS, -PROBES, -MAINTENANCE, -SCHEDULER, -INBOX,
  -ACK, the notices); digests (P3-DIGEST-SETTINGS, P3-DIGEST-REST); task runner +
  `notify` (P3-TASK-RUNNER); `admin_notify_mchanges` (P3-ADMIN-NOTIFY-MCHANGES);
  i18n en/vi (P3-I18N); bans list + site (P3-LIST-POSTING-BANS,
  P3-SITE-BANS-REST).
- Phase 4: standalone SSR slices (P4-WEB-*: login/session/CSRF, scoped held review,
  members/policy, a small settings slice, own postings, bounce recovery) — all
  moved to askama in P4-SHELL: a single render path, shell i18n
  (`Accept-Language` → `site.default_language` → `en`), design tokens + dark
  mode, a11y baseline (axe: no critical/serious), htmx 2.0.10 vendored and served
  from the same origin but not yet loaded by any page. Accounts (P4-ACCOUNT-*), TOTP,
  passkeys (`passkeys.js` is the only first-party script, `script-src 'self'`)
  and OIDC are closed; no CSP nonce yet (not yet needed). See §7.0 and Phase 4.
- Phase 5: threading + HyperKitty-compatible Message-ID-Hash + basic archive
  reading (P2-ARCHIVE-AUTHORITY). No full search/UI yet.

R1 (partial LMTP batch commit) and O1 (lease clock after lock wait) in the
ledger's "Residual P1s" table were closed by the following increments:
`crates/runners/tests/inbound_safety.rs`
(`second_recipient_storage_failure_rolls_back_entire_batch`,
`data_database_wait_timeout_replies_for_all_without_late_intake`) and the
PostgreSQL contracts `mail_queue_lock_clock`, `sibling_lease_clock`, `email_command_clock`
in `scripts/test-postgres.sh`. On 2026-09-14 all PASS in
`cargo test --locked --workspace --all-targets --no-fail-fast` (145 binaries,
936 passed, 0 failed, 56 ignored) and `TEST_POSTGRES_URL=… scripts/test-postgres.sh`
(38 tests, disposable PostgreSQL 14.24, dropped afterwards). Ledger updated.

Phase 3: P3-CLIENT-SUITE, P3-DIGEST-SNAPSHOT and P3-SUBSCRIPTION-E2E are closed; P3-DUAL-BACKEND-CI only lacks a hosted CI run (the ledger records the wiring, does not claim a run). Not yet: all of
Phase 4–7 except the slices listed above (P4-SHELL, all of P4-ACCOUNT-*, P4-TOTP,
P4-WEBAUTHN, P4-OIDC, P4-LIST-SETTINGS, P4-MEMBERS, P4-HELD-QUEUE, P4-LIST-CREATE-INDEX, P4-DOMAINS-USERS, P4-SYSTEM, P4-MODERATION-CROSS, P4-GDPR, P4-ACCEPTANCE are closed — Phase 4 complete per the ledger; Phase 5: P5-RENDER, P5-SEARCH, P5-UI, P5-INTERACTIONS, P5-WEB-POST, P5-MBOX, P5-ADMIN, P5-REMOTE-ARCHIVERS, P5-ACCEPTANCE are closed — Phase 5 complete per the ledger). Details and order in §7.

## 0. One-minute summary

- **Replaces**: `mailman-core` + `postorius` + `hyperkitty` + `mailman-web` + `django-mailman3` + `mailmanclient` + uwsgi + qcluster/cron → 1 binary `listmngr`.
- **Keeps**: concept names (list, member, role, chain, rule, pipeline, handler, runner, template URI), REST `/3.1/` wire-compat, sub-addresses (`-request`, `-join`, `-leave`, `-owner`, `-bounces`, `-confirm`), `Message-ID-Hash`, `Archived-At`, archive URLs.
- **Upgrades**: DB-backed queue (no pickle/qfiles), Argon2id + TOTP + WebAuthn + OIDC, scoped API tokens, native DKIM/ARC/DMARC, CSRF/CSP/rate-limit, audit log, tantivy search, htmx SSR UI, Prometheus, SQLite dev mode.
- **Stack**: Rust 2024, tokio, axum, sqlx (Postgres + SQLite), askama + htmx, mail-parser/mail-send/mail-auth, tantivy.
- **Roadmap**: 8 phases (0→7). End of Phase 2 = sending/receiving mail through a list works. End of Phase 5 = full parity. Phase 7 = 1.0.

## 1. Goals & scope

### 1.1 Statement

Anyone evaluating or already running Mailman can choose listmngr **without losing a single feature**, with simpler deployment, a better UI and stronger security.

### 1.2 Design principles

| Principle                  | Concrete meaning                                                                                                                                               |
|----------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Parity first, extend later | Every Mailman 3.3.x feature has an equivalent. Concept names are kept → reading the Mailman docs still explains listmngr.                                      |
| Wire-compat                | REST `/3.1/` sufficient for `mailmanclient` (Python) to run; same sub-address & VERP format; archive URLs use `message_id_hash` (base32-sha1) like HyperKitty. |
| Single binary, zero-Python | No Django/Celery/uwsgi/cron. Static assets & templates embedded in the binary. 1 config file.                                                                  |
| Security by default        | Argon2id, 2FA, scoped tokens, DKIM/ARC, CSRF, CSP, rate-limit, audit log, HTML sanitize. Secure by default, nothing to switch on.                              |
| Crash-safe, multi-node     | Queue + message store in DB/object store. `FOR UPDATE SKIP LOCKED`. Multiple nodes on the same DB.                                                             |
| Observability              | `tracing` structured logs, `/metrics` Prometheus, `/healthz`, `/readyz`.                                                                                       |
| No unsafe                  | `unsafe_code = "forbid"` across the whole workspace.                                                                                                           |

### 1.3 Non-goals (v1)

- No Mailman 2.1 URL/UI compatibility (only 2.1 data import).
- No Python plugin API. Replaced by webhooks + Rust trait plugins (compile-time). WASM plugins → after 1.0.
- No MySQL/MariaDB in v1 (Postgres + SQLite). → 1.x.
- Does not replace the MTA entirely: Postfix/Exim is still needed to receive mail from the internet. listmngr = LMTP in + SMTP out. Built-in inbound SMTP = experimental (phase 6).
- No NNTP in the early phases (phase 6).

## 2. Tech stack

| Component                       | Crate / tool                                                                | Version             | Reason                                                                                                             |
|---------------------------------|-----------------------------------------------------------------------------|---------------------|--------------------------------------------------------------------------------------------------------------------|
| Runtime                         | `tokio`                                                                     | 1.53                | the async standard                                                                                                 |
| HTTP                            | `axum` + `tower-http` + `axum-extra`                                        | 0.8 / 0.7 / 0.12    | ergonomic, tower middleware                                                                                        |
| DB                              | `sqlx` (postgres, sqlite, runtime-tokio, tls-rustls, chrono, uuid, migrate) | 0.9                 | async, embedded migrations                                                                                         |
| Templates                       | `askama` + `askama_web` (axum-0.8)                                          | 0.16                | compile-time, type-safe                                                                                            |
| Frontend                        | htmx 2.x (vendored), plain CSS (design tokens), no Node runtime             | –                   | SSR, little JS, strict CSP                                                                                         |
| Mail parse/build                | `mail-parser`, `mail-builder`                                               | 0.11 / 0.5          | robust MIME                                                                                                        |
| SMTP out                        | `mail-send`                                                                 | 0.6                 | SMTP client + DKIM sign                                                                                            |
| Email auth                      | `mail-auth` (DKIM/ARC/SPF/DMARC, hickory DNS)                               | 0.12                | verify in + sign out + DMARC policy lookup                                                                         |
| Search                          | `tantivy`                                                                   | 0.26                | replaces Whoosh/Elasticsearch                                                                                      |
| CLI                             | `clap` (derive)                                                             | 4.6                 |                                                                                                                    |
| Config                          | `figment` (toml + env)                                                      | 0.10                | layered                                                                                                            |
| OpenAPI                         | `utoipa`                                                                    | 5.5                 | spec from code                                                                                                     |
| Password                        | `argon2` + `zxcvbn`                                                         | 0.6 / 3.1           | Argon2id                                                                                                           |
| 2FA                             | `totp-rs`, `webauthn-rs`                                                    | 6.0 / latest stable |                                                                                                                    |
| Session                         | `tower-sessions` + `tower-sessions-sqlx-store`                              | 0.15                | DB session                                                                                                         |
| Rate limit                      | `governor`                                                                  | 0.10                |                                                                                                                    |
| Secrets                         | `secrecy`, `zeroize`, `subtle`                                              | 0.10 / 1.9 / 2.6    | no logging, const-time                                                                                             |
| HTML sanitize                   | `ammonia`                                                                   | 4.1                 | archive render                                                                                                     |
| Markdown                        | `pulldown-cmark`                                                            | 0.13                | `archive_rendering_mode=markdown`                                                                                  |
| HTML→text                       | `html2text`                                                                 | 0.17                | `convert_html_to_plaintext`                                                                                        |
| Hash                            | `sha1` + `data-encoding`                                                    | 0.11 / 2.11         | `Message-ID-Hash` base32(sha1)                                                                                     |
| Validation                      | `garde`                                                                     | 0.23                |                                                                                                                    |
| Embed assets                    | `rust-embed`                                                                | 8.12                |                                                                                                                    |
| Errors/log                      | `thiserror`, `anyhow`, `tracing`, `tracing-subscriber`                      | 2 / 1 / 0.1         |                                                                                                                    |
| IDs/time                        | `uuid` (v7), `chrono`                                                       | 1.26 / 0.4          |                                                                                                                    |
| IDN                             | `idna`                                                                      | 1.1                 |                                                                                                                    |
| Pickle (import21, import3 --db) | own reader in `crates/import` (`pickle.rs`)                                 | –                   | Mailman 2.1 `config.pck`; Mailman 3 pickled columns + message store (serde-pickle 1.2.0 misreads the `OBJ` opcode) |
| Test                            | `testcontainers`, `proptest`, `cargo-fuzz`, `insta`, Playwright (dev only)  | –                   |                                                                                                                    |
| Supply chain                    | `cargo-deny`, `cargo-audit`, `cargo-sbom`, `cosign`                         | –                   |                                                                                                                    |

Notes:

- `rustls` is at 0.24-dev → use the stable version via the transitive deps of `sqlx`/`mail-send`, no direct pin.
- Rust 2024 edition has `async fn` in trait; `async-trait` is used only for `dyn` objects (handlers/rules registry).

## 3. Architecture

### 3.1 Workspace layout

```
listmngr/
├── Cargo.toml                  # workspace, [workspace.dependencies] pin version, lints
├── rust-toolchain.toml         # stable
├── deny.toml, rustfmt.toml, clippy.toml
├── CLAUDE.md, README.md, CHANGELOG.md
├── docs/
│   ├── PLAN.md                 # this file
│   ├── ARCHITECTURE.md
│   ├── FEATURE_PARITY.md       # living checklist, updated every PR
│   ├── SECURITY.md             # threat model + disclosure
│   ├── MIGRATION.md            # from Mailman 2.1 / 3.x
│   └── adr/                    # Architecture Decision Records (0001-db-queue.md ...)
├── crates/
│   ├── core/       listmngr-core       domain model, enums, config, error, ids, i18n catalog, template URI
│   ├── db/         listmngr-db         sqlx repos, migrations, queue, message store, audit
│   ├── mail/       listmngr-mail       parse/build, DKIM/ARC/DMARC/SPF, LMTP server, SMTP client, VERP, bounce detectors
│   ├── pipeline/   listmngr-pipeline   chains, rules, handlers, email commands, digests, templates render
│   ├── runners/    listmngr-runners    supervisor + runners (in/out/bounce/digest/retry/archive/command/nntp/task)
│   ├── archive/    listmngr-archive    threading, tantivy index, rendering, mbox import/export
│   ├── api/        listmngr-api        axum REST (/3.1 compat + /api/v1), OpenAPI, token auth
│   ├── web/        listmngr-web        SSR UI (askama + htmx), sessions, CSRF, static assets, OIDC
│   └── cli/        listmngr            binary: serve, migrate, lists, members, import21, ...
├── deploy/
│   ├── Dockerfile               # multi-stage, distroless/static, non-root
│   ├── docker-compose.yml       # postgres + postfix + listmngr
│   ├── postfix/                 # main.cf snippets, transport map regen
│   ├── exim/                    # router/transport snippets
│   └── systemd/                 # hardened unit
└── tests/                       # e2e (testcontainers: pg + smtp sink)
```

Dependency graph (one direction only):

```
cli ─┬─> api ─┐
     ├─> web ─┼─> archive ─> pipeline ─> mail ─┐
     └─> runners ┘                       db ───┼─> core
```

### 3.2 Process model

`listmngr serve` = 1 process, 1 tokio runtime, running concurrently:

| Component         | Description                                                                                                                                                         |
|-------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| HTTP server       | axum: `/` web UI, `/3.1/` REST compat, `/api/v1/` new REST, `/archives/`, `/metrics`, `/healthz`, `/readyz`, `/openapi.json`                                        |
| LMTP listener     | `:8024` (RFC 2033). Receives mail from the MTA → writes to the message store → enqueues `in`. Rejects early if > max size / list does not exist.                    |
| Runner supervisor | each runner = a tokio task (N workers per runner possible). Polls the queue table (`SKIP LOCKED`), exponential backoff when idle. Restarts the runner if it panics. |
| Scheduler         | periodic digests, `task` runner (expire pendings/workflow states, cache cleanup), stale bounce reset, transport map regen, index commit.                            |

- `listmngr serve --roles web,mta,runners` → splits roles when scaling horizontally (multiple nodes on the same DB + shared message store).
- Graceful shutdown (SIGTERM): stop accepting new jobs, drain in-flight (timeout), close listeners.
- No cron/celery/qcluster needed.

### 3.3 Message flow

```
Internet ─> Postfix ─LMTP─> listmngr :8024 ─> [message_store.put(raw) + queue(in)]
  │
  ├─ in runner ─────> resolve list + sub-address
  │     ├─ list@         → chain: default-posting-chain
  │     ├─ list-owner@   → chain: default-owner-chain → owner pipeline
  │     ├─ list-request@ /-join/-leave/-subscribe/-unsubscribe/-confirm+tok → queue(command)
  │     └─ list-bounces@ (+VERP) → queue(bounces)
  │
  ├─ chain (rules) ──> accept → queue(pipeline)
  │                    hold   → held_messages + notify owner + notify user
  │                    reject → bounce to sender (virgin queue)
  │                    discard→ log
  │
  ├─ pipeline runner ─> handlers in order → to-archive queue(archive)
  │                                       → to-digest (append digest mbox)
  │                                       → to-usenet queue(nntp)
  │                                       → to-outgoing queue(out)
  ├─ out runner ─────> SMTP relay, chunk by max_recipients, VERP/personalize, DKIM sign
  │                    transient fail → queue(retry) (backoff), permanent fail → bounce event
  ├─ bounce runner ──> detect (DSN + heuristics) → bounce_events → score → warn/disable/remove
  ├─ command runner ─> email commands → reply (virgin)
  ├─ archive runner ─> parse, thread, index (tantivy), attachments
  ├─ digest runner ──> by threshold/periodic → build MIME + RFC1153 → queue(out)
  ├─ virgin runner ──> system-generated mail (notices, replies) → cook-headers → out
  ├─ retry runner ───> re-enqueue out
  ├─ task runner ────> periodic expiry/cleanup
  └─ shunt/bad ──────> failed (poison) jobs kept, `listmngr unshunt`
```

### 3.4 Data model (Postgres; SQLite shares the schema)

Notation: `PK`, `FK→`, `UQ`, `JSONB` (SQLite = TEXT json).

**Identity**

| Table                        | Key columns                                                                                                                                     |
|------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------|
| `domains`                    | `id PK`, `mail_host UQ`, `description`, `alias_domain`, `created_at`                                                                            |
| `domain_owners`              | `domain_id FK→domains`, `user_id FK→users`                                                                                                      |
| `users`                      | `id uuid PK`, `display_name`, `is_server_owner bool`, `created_at`, `preferences_id FK`, `locale`, `timezone`                                   |
| `user_credentials`           | `user_id PK FK`, `password_hash (argon2id)`, `password_updated_at`, `failed_attempts`, `locked_until`                                           |
| `user_totp`                  | `user_id`, `secret (encrypted)`, `enabled_at`, `recovery_codes_hash[]`                                                                          |
| `user_webauthn`              | `id`, `user_id`, `credential (json)`, `name`, `created_at`, `last_used_at`                                                                      |
| `user_oidc`                  | `user_id`, `provider`, `subject UQ(provider,subject)`                                                                                           |
| `addresses`                  | `id PK`, `email UQ (case-insensitive)`, `original_email`, `display_name`, `user_id FK nullable`, `verified_on`, `registered_on`                 |
| `users.preferred_address_id` | FK→addresses                                                                                                                                    |
| `api_tokens`                 | `id`, `user_id`, `name`, `token_hash UQ`, `scopes text[]`, `list_id nullable`, `domain_id nullable`, `expires_at`, `last_used_at`, `revoked_at` |
| `sessions`                   | tower-sessions store                                                                                                                            |

**Lists & membership**

| Table                     | Key columns                                                                                                                                                                                                                                                                                                                                                              |
|---------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `mailing_lists`           | `id PK`, `list_id UQ (name.domain)`, `list_name`, `mail_host FK→domains`, `display_name`, `description`, `info`, `subject_prefix`, `advertised`, `anonymous_list`, `created_at`, `last_post_at`, `post_id`, `volume`, `next_digest_number`, `digest_last_sent_at`, `emergency`, `style_name`, + all §4.1 settings as typed columns; rarely used settings → `extra JSONB` |
| `list_acceptable_aliases` | `list_id`, `alias (pattern)`                                                                                                                                                                                                                                                                                                                                             |
| `list_nonmember_rules`    | `list_id`, `kind (accept/hold/reject/discard)`, `pattern`                                                                                                                                                                                                                                                                                                                |
| `members`                 | `id uuid PK`, `list_id FK`, `role (owner/moderator/member/nonmember)`, `address_id FK`, `user_id FK nullable`, `subscription_mode (as_address/as_user)`, `moderation_action nullable`, `display_name`, `preferences_id FK`, `bounce_score`, `last_bounce_received`, `last_warning_sent`, `total_warnings_sent`, `created_at`; `UQ(list_id, role, address_id)`            |
| `preferences`             | `id PK`, `acknowledge_posts`, `hide_address`, `preferred_language`, `receive_list_copy`, `receive_own_postings`, `delivery_mode`, `delivery_status` (all nullable → layered: member → address → user → system)                                                                                                                                                           |
| `header_matches`          | `id`, `list_id`, `position`, `header`, `pattern`, `action nullable`, `tag`, `chain nullable`                                                                                                                                                                                                                                                                             |
| `bans`                    | `id`, `list_id nullable (null = global)`, `email_or_regex`                                                                                                                                                                                                                                                                                                               |
| `templates`               | `id`, `name (list:member:regular:footer …)`, `scope (site/domain/list)`, `scope_id`, `language`, `uri nullable`, `body nullable`, `username/password (for http uri)`; `UQ(name,scope,scope_id,language)`                                                                                                                                                                 |
| `list_archivers`          | `list_id`, `name`, `enabled`                                                                                                                                                                                                                                                                                                                                             |
| `list_styles`             | code-defined (legacy-default, legacy-announce, private-default) + `custom_styles JSONB`                                                                                                                                                                                                                                                                                  |

**Workflow & moderation**

| Table             | Key columns                                                                                                                                              |
|-------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------|
| `pendings`        | `id`, `token_hash UQ`, `kind (subscription/unsubscription/held/probe/invite/verify)`, `payload JSONB`, `expires_at`, `created_at`                        |
| `workflow_states` | `token`, `name (subscription/unsubscription)`, `step`, `data JSONB`                                                                                      |
| `held_messages`   | `id`, `list_id`, `message_key FK→messages`, `sender`, `subject`, `reason`, `hold_date`, `msgdata JSONB`, `moderator_id nullable`, `disposition nullable` |
| `moderation_log`  | `held_id`, `action`, `reason`, `moderator_id`, `forward_to`, `at`                                                                                        |

**Mail infrastructure**

| Table                            | Key columns                                                                                                                                                                                                                    |
|----------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `messages` (message store index) | `id PK`, `message_id`, `message_id_hash UQ`, `list_id nullable`, `size`, `store_key` (fs/db/s3 ref), `headers JSONB (subset)`, `created_at`, `refcount`                                                                        |
| `message_blobs`                  | `store_key PK`, `raw bytea` (backend `db` only)                                                                                                                                                                                |
| `queue_jobs`                     | `id bigserial`, `queue (in/out/pipeline/…)`, `message_key`, `msgdata JSONB`, `run_after`, `attempts`, `max_attempts`, `locked_by`, `locked_at`, `last_error`, `created_at`; index `(queue, run_after) WHERE locked_by IS NULL` |
| `bounce_events`                  | `id`, `list_id`, `email`, `timestamp`, `message_id`, `context (normal/relay/probe)`, `processed bool`                                                                                                                          |
| `digest_mbox`                    | `list_id`, `volume`, `number`, `messages (append-only refs)`, `size`                                                                                                                                                           |
| `dkim_keys`                      | `domain_id`, `selector`, `algorithm (rsa2048/ed25519)`, `private_key (encrypted)`, `public_dns_record`, `active`, `rotated_at`                                                                                                 |
| `nntp_watermarks`                | `list_id`, `watermark`                                                                                                                                                                                                         |

**Archive**

| Table                              | Key columns                                                                                                                                                                                                                                       |
|------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `archive_lists`                    | `list_id`, `archive_policy cache`, `stats cache JSONB`                                                                                                                                                                                            |
| `archive_senders`                  | `id`, `email`, `display_name`, `user_id nullable`                                                                                                                                                                                                 |
| `archive_threads`                  | `id`, `list_id`, `thread_id (hash of first msg)`, `subject`, `date_active`, `starting_message_id`, `category_id nullable`, `reply_count`, `participants_count`                                                                                    |
| `archive_messages`                 | `id`, `list_id`, `message_id_hash UQ(list,hash)`, `thread_id FK`, `parent_id nullable`, `sender_id`, `subject`, `body_text`, `body_html_sanitized nullable`, `date`, `timezone`, `in_reply_to`, `references text[]`, `store_key`, `archived_date` |
| `archive_attachments`              | `message_id FK`, `counter`, `name`, `content_type`, `encoding`, `size`, `store_key`                                                                                                                                                               |
| `archive_votes`                    | `message_id`, `user_id`, `value (+1/-1)`                                                                                                                                                                                                          |
| `archive_tags`, `archive_taggings` | tag name; `(thread_id, tag_id, user_id)`                                                                                                                                                                                                          |
| `archive_categories`               | `id`, `name`, `color`                                                                                                                                                                                                                             |
| `archive_favorites`                | `(thread_id, user_id)`                                                                                                                                                                                                                            |
| `archive_last_views`               | `(thread_id, user_id)`, `view_date`                                                                                                                                                                                                               |
| tantivy index                      | on disk at `data/index/`, rebuildable from the DB at any time                                                                                                                                                                                     |

**Ops**

| Table                | Key columns                                                                                                          |
|----------------------|----------------------------------------------------------------------------------------------------------------------|
| `audit_log`          | `id`, `at`, `actor_user_id`, `actor_token_id`, `ip`, `action`, `target_type`, `target_id`, `diff JSONB`; append-only |
| `webhooks`           | `id`, `url`, `secret_hash`, `events text[]`, `list_id nullable`, `enabled`                                           |
| `webhook_deliveries` | retry state                                                                                                          |
| `_sqlx_migrations`   | sqlx                                                                                                                 |

### 3.5 Queue & message store

- Queue = the `queue_jobs` table. Claim:

```sql
UPDATE queue_jobs SET locked_by = $worker, locked_at = now()
WHERE id = (
  SELECT id FROM queue_jobs
  WHERE queue = $queue AND run_after <= now() AND locked_by IS NULL
  ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED
) RETURNING *;
```

- Lock timeout: a job whose `locked_at` is older than `runner.lock_timeout` → treated as abandoned, another worker picks it up again.
- Poison job (attempts ≥ max) → moved to `queue = 'shunt'` with `last_error`. CLI `listmngr unshunt`, `listmngr qfile <id>`.
- Queue names are kept as in Mailman: `in`, `pipeline`, `out`, `retry`, `bounces`, `command`, `virgin`, `archive`, `digest`, `nntp`, `shunt`, `bad`.
- Message store trait:

```rust
trait MessageStore {
    async fn put(&self, raw: &[u8]) -> Result<StoreKey>;
    async fn get(&self, key: &StoreKey) -> Result<Bytes>;
    async fn delete(&self, key: &StoreKey) -> Result<()>;
}
```

  Backends: `fs` (default, `data/messages/ab/cd/<hash>.eml`), `db` (`message_blobs`), `s3` (phase 6). Raw bytes are kept as-is, not normalized (needed for DKIM verify & mbox export).
- SQLite: no `SKIP LOCKED` → claim inside `BEGIN IMMEDIATE`; single-node only.

### 3.6 Config

File `listmngr.toml` + env override `LISTMNGR__<SECTION>__<KEY>` (figment). Secrets support `*_file` (docker secrets).

```toml
[site]
name = "Example Lists"
site_owner = "postmaster@example.com"
default_language = "en"
base_url = "https://lists.example.com"

[database]
url = "postgres://listmngr:***@localhost/listmngr"   # or sqlite://data/listmngr.db
max_connections = 20

[message_store]
backend = "db"          # db | fs | s3 (P6-MESSAGE-STORE: db is the de facto default)
path = "data/messages"

[mta]
incoming = "postfix"    # postfix | exim | none  (generates transport/alias maps)
lmtp_listen = "127.0.0.1:8024"
smtp_relay = "127.0.0.1:25"
smtp_tls = "opportunistic"
max_recipients = 500
max_recipients_per_transaction = 500 # Mailman max_recipients: recipients per outgoing SMTP transaction
authenticity_checks = false          # SPF/DKIM/DMARC via the system resolver; Authentication-Results; conditional DMARC mitigation
retry_initial_secs = 10              # transient delivery failures back off 10s, 20s, 40s ... with jitter
retry_max_secs = 3600
max_sessions_per_connection = 0
map_directory = "data/mta"           # generation-* directories + `current` symlink, at startup and after list changes
lmtp_map_target = "127.0.0.1:8024"   # host:port the MTA uses for LMTP; defaults to lmtp_listen
transport_file_type = "regex"        # Postfix regex (read directly) | hash (postmap_command)
postmap_command = "/usr/sbin/postmap"
map_permissions = "group"            # owner | group | world
map_generations_kept = 5
verp_delimiter = "+"
verp_format = "{bounces}+{local}={domain}"
verp_personalized_deliveries = false # personalized copies get per-recipient VERP envelopes
verp_delivery_interval = 0           # every Nth post is delivered per recipient with VERP; 0 never

[web]
listen = "127.0.0.1:8000"
trusted_proxies = ["127.0.0.1/32"]
session_idle = "12h"
session_absolute = "7d"
signup = true                 # /web/signup; disable so that only administrators create accounts

[api]
listen = "127.0.0.1:8001"             # may be the same as web
compat_basic_auth = false             # enable for mailmanclient
compat_basic_auth_allow = ["127.0.0.1/32"]

[security]
argon2 = { memory_kib = 65536, iterations = 3, parallelism = 1 }
password_min_score = 3
require_2fa_for = ["server_owner"]
pending_request_life = "3d"
rate_limit = { login = "5/min", subscribe = "10/hour", api = "600/min", api_pre_auth = "1200/min" }

[mailman]                              # keeps the Mailman config group names for easy mapping
default_member_action = "defer"
default_nonmember_action = "hold"
noreply_address = "noreply"
site_owner_notify = true
filtered_messages_are_preservable = false # filter_action = preserve keeps a copy in the shunt store
bounce_probes = true                     # probe at the bounce threshold; false disables at once
bounce_probe_lifetime_secs = 604800
run_tasks_every_secs = 3600              # task runner: expire tokens/probes, collect finished jobs, stale bounce reset
finished_job_retention_secs = 604800     # how long finished queue jobs and their messages stay

[archive]
enabled = true
index_path = "data/index"
default_policy = "public"

[bounces]
register_bounces_every = "15m"

[digests]
send_every = "5m"

[runners]
workers = { in = 2, out = 4, pipeline = 2, archive = 1, bounces = 1, command = 1, digest = 1, retry = 1, virgin = 1, nntp = 0, task = 1 }
lock_timeout = "10m"

[observability]
log = "info"
metrics = true
```

### 3.7 MTA integration

| MTA                                   | Inbound                                                                  | Auto-generated maps                                                                                                                                                                |
|---------------------------------------|--------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Postfix (recommended)                 | `transport_maps`, `relay_recipient_maps`, `relay_domains` → LMTP `:8024` | `data/mta/current/{domains,recipients,transport}.regexp` (or `hash:` `postfix_domains`/`postfix_lmtp` + `postmap`), run at start, on list create/delete & `listmngr aliases regen` |
| Exim 4                                | router `manualroute` + `lsearch` as in Mailman 3                         | `data/mta/current/exim_{domains,recipients}` + snippet in `deploy/exim/`                                                                                                           |
| Built-in SMTP (phase 6, experimental) | listmngr receives on `:25` directly                                      | no map needed                                                                                                                                                                      |

Outbound: SMTP relay (Postfix on localhost or an external one with AUTH/STARTTLS/implicit TLS), chunked by `max_recipients`, `MAIL FROM = list-bounces+VERP@host`, DKIM sign per domain, `List-*` headers.

## 4. Feature parity matrix

Status: `P0`..`P7` = planned phase. The "Change" column = how it differs from Mailman.

### 4.1 Core — List settings (Postorius' 9 groups)

| Group               | Setting (Mailman names kept as-is)                                                                                                                                                                                                                                                                                                                                                                                            | Phase                         |
|---------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------|
| List Identity       | `display_name`, `description`, `info`, `subject_prefix`, `advertised`, `preferred_language`, `mail_host`, `list_name`, `fqdn_listname`, `list_id`, `created_at`, `last_post_at`, `post_id`, `volume`                                                                                                                                                                                                                          | P1                            |
| Automatic Responses | `autorespond_owner`, `autoresponse_owner_text`, `autorespond_postings`, `autoresponse_postings_text`, `autorespond_requests`, `autoresponse_request_text`, `autoresponse_grace_period`, `respond_to_post_requests`, `send_welcome_message`, `send_goodbye_message`, `admin_immed_notify`, `admin_notify_mchanges`                                                                                                             | P3                            |
| Alter Messages      | `filter_content`, `filter_types`, `pass_types`, `filter_extensions`, `pass_extensions`, `collapse_alternatives`, `convert_html_to_plaintext`, `filter_action`, `anonymous_list`, `include_rfc2369_headers`, `allow_list_posts`, `reply_goes_to_list` (no_munging/point_to_list/explicit_header), `reply_to_address`, `first_strip_reply_to`, `personalize` (none/individual/full), `include_sender_header` (Sender: override) | P2                            |
| DMARC Mitigations   | `dmarc_mitigate_action` (no_mitigation/munge_from/wrap_message/reject/discard), `dmarc_mitigate_unconditionally`, `dmarc_addresses`, `dmarc_moderation_notice`, `dmarc_wrapped_message_text`                                                                                                                                                                                                                                  | P2 (munge_from) / P6 (full)   |
| Digest              | `digests_enabled`, `digest_size_threshold`, `digest_send_periodic`, `digest_volume_frequency` (yearly/monthly/quarterly/weekly/daily), `next_digest_number`, `digest_last_sent_at`                                                                                                                                                                                                                                            | P3                            |
| Message Acceptance  | `default_member_action`, `default_nonmember_action` (defer/accept/hold/reject/discard), `accept_these_nonmembers`, `hold_these_nonmembers`, `reject_these_nonmembers`, `discard_these_nonmembers`, `require_explicit_destination`, `acceptable_aliases`, `administrivia`, `max_message_size`, `max_num_recipients`, `emergency`, `posting_pipeline`, `moderator "Approved:" posting key`                                      | P2                            |
| Archiving           | `archive_policy` (public/private/never), `archive_rendering_mode` (text/markdown), `archivers` (prototype/local=listmngr/mail-archive/mhonarc/hyperkitty-remote)                                                                                                                                                                                                                                                              | P1 (field) / P5 (enforcement) |
| Member Policy       | `subscription_policy` (open/confirm/moderate/confirm_then_moderate), `unsubscription_policy`, `member_roster_visibility` (public/members/moderators)                                                                                                                                                                                                                                                                          | P3                            |
| Bounce Processing   | `process_bounces`, `bounce_score_threshold`, `bounce_info_stale_after`, `bounce_you_are_disabled_warnings`, `bounce_you_are_disabled_warnings_interval`, `bounce_notify_owner_on_disable`, `bounce_notify_owner_on_removal`, `bounce_notify_owner_on_bounce_increment`, `forward_unrecognized_bounces_to` (discard/site_owner/administrators)                                                                                 | P3                            |
| Usenet              | `gateway_to_mail`, `gateway_to_news`, `linked_newsgroup`, `nntp_prefix_subject_too`, `newsgroup_moderation` (none/open_moderated/moderated), `usenet_watermark`                                                                                                                                                                                                                                                               | P6                            |
| Addresses           | `posting_address`, `bounces_address`, `join_address`, `leave_address`, `owner_address`, `request_address`, `no_reply_address` (derived)                                                                                                                                                                                                                                                                                       | P1                            |
| Styles              | `legacy-default`, `legacy-announce`, `private-default` + custom style (JSON)                                                                                                                                                                                                                                                                                                                                                  | P1                            |

### 4.2 Core — Users, addresses, membership

| Feature                                                                                                                                                                                            | Phase         | Change               |
|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------|----------------------|
| User: `user_id` uuid, `display_name`, password, `is_server_owner`, preferences, multiple addresses, `preferred_address`                                                                            | P1            | adds locale/timezone |
| Address: verify/unverify, link/unlink user, `display_name`, case-preserve `original_email`                                                                                                         | P1            |                      |
| Member roles: owner, moderator, member, nonmember                                                                                                                                                  | P1            |                      |
| `subscription_mode`: as_address / as_user                                                                                                                                                          | P1            |                      |
| Preferences layered: member → address → user → system (`delivery_mode`, `delivery_status`, `acknowledge_posts`, `hide_address`, `preferred_language`, `receive_list_copy`, `receive_own_postings`) | P1            |                      |
| `delivery_mode`: regular, plaintext_digests, mime_digests, summary_digests                                                                                                                         | P1/P3         |                      |
| `delivery_status`: enabled, by_user, by_bounces, by_moderator, unknown                                                                                                                             | P1            |                      |
| Per-member `moderation_action` override                                                                                                                                                            | P2            |                      |
| Subscription workflow: verify → confirm → moderate (state machine, resumable), `pre_verified`, `pre_confirmed`, `pre_approved`, `invitation`, `send_welcome_message` override                      | P3            | token hashed         |
| Unsubscription workflow (confirm / moderate)                                                                                                                                                       | P3            |                      |
| Mass subscribe / mass unsubscribe / sync members (file)                                                                                                                                            | P1 CLI, P4 UI |                      |
| Bans: global + per-list, regex `^`                                                                                                                                                                 | P3            |                      |
| Server owners                                                                                                                                                                                      | P1            |                      |
| Find member (`/members/find`, `findmember`)                                                                                                                                                        | P1            |                      |

### 4.3 Core — Chains & rules (moderation)

Chains: `default-posting-chain`, `default-owner-chain`, `accept`, `hold`, `reject`, `discard`, `moderation`, `header-match`, `dmarc-mitigation`. Link actions: `jump`, `defer`, `stop`, `run`, `detour`.

| Rule                                                                                                                                               | Description                                                                                      | Phase           |
|----------------------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------|-----------------|
| `approved`                                                                                                                                         | `Approved:`/`Approve:` header or first body line matching the posting key → accept; strip header | P2              |
| `emergency`                                                                                                                                        | list `emergency=true` → hold                                                                     | P2              |
| `loop`                                                                                                                                             | `List-Post` matches the list → discard                                                           | P2              |
| `banned-address`                                                                                                                                   | sender is banned (global/list) → reject                                                          | P2              |
| `member-moderation`                                                                                                                                | member has `moderation_action`                                                                   | P2              |
| `nonmember-moderation`                                                                                                                             | `*_these_nonmembers` + `default_nonmember_action`                                                | P2              |
| `administrivia`                                                                                                                                    | body looks like a command (subscribe/unsubscribe…)                                               | P2              |
| `implicit-dest`                                                                                                                                    | `require_explicit_destination` + `acceptable_aliases`                                            | P2              |
| `max-recipients`                                                                                                                                   |                                                                                                  | P2              |
| `max-size`                                                                                                                                         |                                                                                                  | P2              |
| `no-subject`                                                                                                                                       |                                                                                                  | P2              |
| `suspicious-header`                                                                                                                                | header matches (legacy)                                                                          | P2              |
| `no-senders`                                                                                                                                       | no From/Sender                                                                                   | P2              |
| `news-moderation`                                                                                                                                  |                                                                                                  | P6              |
| `dmarc-mitigation`                                                                                                                                 | DNS `_dmarc` p=reject/quarantine (+org domain via PSL)                                           | P2              |
| `digests`                                                                                                                                          | rule for digest messages                                                                         | P3              |
| `any`, `truth`                                                                                                                                     | glue                                                                                             | P2              |
| Dynamic header matches (per-list, action/tag/chain)                                                                                                |                                                                                                  | P2              |
| Hold reasons & moderator notice (`list:admin:action:post`), user notice (`list:user:notice:hold`)                                                  |                                                                                                  | P2              |
| Moderator actions: accept / reject (reason) / discard / defer, forward, "moderate sender" → set member action, "add to ban", "add to header match" |                                                                                                  | P2 REST / P4 UI |

### 4.4 Core — Pipeline handlers

| Pipeline                                      | Handlers (in order)                                                                                                                                                                                                                                                                              | Phase                                 |
|-----------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------|
| `default-posting-pipeline`                    | `validate-authenticity` → `mime-delete` → `tagger` → `member-recipients` → `avoid-duplicates` → `cleanse` → `cleanse-dkim` → `cook-headers` → `subject-prefix` → `rfc-2369` → `to-archive` → `to-digest` → `to-usenet` → `after-delivery` → `acknowledge` → `dmarc` → `arc-sign` → `to-outgoing` | P2 (except to-usenet P6, arc-sign P6) |
| `virgin`                                      | `cook-headers` → `to-outgoing`                                                                                                                                                                                                                                                                   | P2                                    |
| `default-owner-pipeline`                      | `owner-recipients` → `cleanse` → `cook-headers` → `to-outgoing`                                                                                                                                                                                                                                  | P2                                    |
| Other handlers                                | `decorate` (header/footer template, placeholders `$display_name`, `$listinfo_uri`, `$user_address`, `$user_delivered_to`, `$user_name`, `$user_options_uri`…), `file-recipients`, `replybot` (autoresponder)                                                                                     | P2 / P3                               |
| `mime-delete` details                         | filter/pass types & extensions, collapse alternatives, html→text, `filter_action` (discard/reject/forward_to_list_owner/preserve), keep `Content-Disposition`                                                                                                                                    | P2                                    |
| `cook-headers` details                        | `Sender`, `Reply-To` policy, `X-Mailman-Version` → `X-Listmngr-Version`, `Precedence: list`, `X-Mailman-Rule-Hits/Misses`, `List-Id`, `X-Message-ID-Hash`                                                                                                                                        | P2                                    |
| `rfc-2369`                                    | `List-Help`, `List-Post`, `List-Subscribe`, `List-Unsubscribe`, `List-Archive`, `Archived-At`, **+ `List-Unsubscribe-Post: List-Unsubscribe=One-Click` (RFC 8058)**                                                                                                                              | P2                                    |
| Personalization                               | `personalize=full` → 1 msg/recipient, VERP always on when personalizing                                                                                                                                                                                                                          | P2                                    |
| Custom pipeline per list (`posting_pipeline`) |                                                                                                                                                                                                                                                                                                  | P2                                    |

### 4.5 Core — Runners & queues

| Runner         | Phase | Notes                            |
|----------------|-------|----------------------------------|
| `lmtp`         | P2    | server built-in                  |
| `in`           | P2    |                                  |
| `pipeline`     | P2    |                                  |
| `out`          | P2    | chunking, VERP, DKIM             |
| `retry`        | P2    | backoff, `delivery_retry_period` |
| `virgin`       | P2    |                                  |
| `bounces`      | P3    |                                  |
| `command`      | P3    |                                  |
| `digest`       | P3    |                                  |
| `archive`      | P5    |                                  |
| `nntp`         | P6    |                                  |
| `task`         | P3    | expire pendings/workflows, cache |
| `shunt`, `bad` | P2    | CLI `unshunt`                    |
| `rest`         | –     | merged into the HTTP server      |

### 4.6 Core — Email commands & sub-addresses

| Sub-address                                              | Handling                                                                                                                     | Phase |
|----------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------|-------|
| `list@`                                                  | posting                                                                                                                      | P2    |
| `list-owner@`                                            | owner pipeline → owners + moderators                                                                                         | P2    |
| `list-bounces@`, `list-bounces+VERP@`                    | bounce runner                                                                                                                | P3    |
| `list-request@`                                          | command bot: `confirm <tok>`, `join`/`subscribe [digest=…] [address=…]`, `leave`/`unsubscribe`, `help`, `echo`, `end`/`stop` | P3    |
| `list-join@`, `list-subscribe@`                          | = `join`                                                                                                                     | P3    |
| `list-leave@`, `list-unsubscribe@`                       | = `leave`                                                                                                                    | P3    |
| `list-confirm+token@`                                    | = `confirm` (Subject/To token)                                                                                               | P3    |
| Autoresponder for owner/postings/requests + grace period |                                                                                                                              | P3    |
| `List-Unsubscribe-Post` one-click HTTP POST (RFC 8058)   | HTTP endpoint                                                                                                                | P2    |

### 4.7 Core — Bounce processing

| Feature                                                                                                                                                                                                                                                                                             | Phase |
|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|
| VERP encode/decode (`bounces+local=domain@host`), `verp_confirmations`, `verp_delivery_interval`                                                                                                                                                                                                    | P2/P3 |
| Detectors (ported from `flufl.bounce`): DSN RFC 3464 (multipart/report), `aol`, `caiwireless`, `exchange`, `exim`, `groupwise`, `llnl`, `microsoft`, `netscape`, `postfix`, `qmail`, `sendmail`, `simplematch` (regex catalog), `simplewarning`, `sina`, `smtp32`, `yahoo`, `yale` + fixture corpus | P3    |
| `bounce_events` (context normal/relay/probe), `processed`                                                                                                                                                                                                                                           | P3    |
| Score increments by 1/day/address, `bounce_info_stale_after` reset, threshold → `delivery_status=by_bounces`                                                                                                                                                                                        | P3    |
| Warnings (`bounce_you_are_disabled_warnings` × interval) → remove                                                                                                                                                                                                                                   | P3    |
| Probe message (`-bounces+<token>`), probe bounce → disable immediately                                                                                                                                                                                                                              | P3    |
| Owner notices: increment/disable/removal/unrecognized; `forward_unrecognized_bounces_to`                                                                                                                                                                                                            | P3    |
| Member re-enable (web/email confirm)                                                                                                                                                                                                                                                                | P4    |

### 4.8 Core — Digests

| Feature                                                                                                                      | Phase |
|------------------------------------------------------------------------------------------------------------------------------|-------|
| Append to the per-list digest mbox; `digest_size_threshold` (KB) trigger; `digest_send_periodic` + `digest_volume_frequency` | P3    |
| MIME digest (multipart/digest, `multipart/mixed` masthead + TOC + messages + footer)                                         | P3    |
| Plaintext digest RFC 1153                                                                                                    | P3    |
| Summary digest (Mailman: = MIME with a flag)                                                                                 | P3    |
| Volume/number bump, `listmngr digests --send/--bump/--periodic`                                                              | P3    |
| Templates: `list:member:digest:masthead/header/footer`                                                                       | P3    |
| Member `receive_list_copy`/`digest` delivery mode routing                                                                    | P3    |

### 4.9 Core — Templates (URIs)

Scope: site → domain → list; language fallback; loaders `mailman:///` (built-in), `file:///`, `https://` (basic auth), DB body. Names kept as in Mailman:

| Group        | Templates                                                                                                                                                                                                                                                                                                                                         | Phase                        |
|--------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------------------------|
| Admin        | `list:admin:action:post`, `list:admin:action:subscribe`, `list:admin:action:unsubscribe`, `list:admin:notice:disable`, `list:admin:notice:increment`, `list:admin:notice:removal`, `list:admin:notice:subscribe`, `list:admin:notice:unrecognized`, `list:admin:notice:unsubscribe`, `list:admin:notice:pending`                                  | P2–P3                        |
| Member       | `list:member:digest:footer`, `list:member:digest:header`, `list:member:digest:masthead`, `list:member:generic:footer`, `list:member:regular:footer`, `list:member:regular:header`                                                                                                                                                                 | P2–P3                        |
| User         | `list:user:action:invite`, `list:user:action:subscribe`, `list:user:action:unsubscribe`, `list:user:notice:goodbye`, `list:user:notice:hold`, `list:user:notice:no-more-today`, `list:user:notice:post`, `list:user:notice:probe`, `list:user:notice:refuse`, `list:user:notice:rejected`, `list:user:notice:warning`, `list:user:notice:welcome` | P3                           |
| Domain       | `domain:admin:notice:new-list`                                                                                                                                                                                                                                                                                                                    | P1                           |
| Placeholders | `$listname`, `$list_id`, `$display_name`, `$fqdn_listname`, `$list_domain`, `$description`, `$info`, `$request_email`, `$owner_email`, `$listinfo_uri`, `$list_requests`, `$user_email`, `$user_name`, `$user_delivered_to`, `$user_options_uri`, `$subject`, `$reasons`, `$confirm_email`, `$token`, `$sender_email`, `$moderator_notice`…       | –                            |
| i18n         | Templates + UI strings: `en`, `vi` initially; fluent or gettext `.po` imported from Mailman to reuse 40+ languages                                                                                                                                                                                                                                | P3 (framework) / P6 (import) |

### 4.10 Core — REST API

Two prefixes: `/3.1/` (compat, JSON shape identical to Mailman 3.3 so `mailmanclient` works) and `/api/v1/` (new, OpenAPI, typed, cursor pagination, ETag). Same handlers, different serializers.

| Resource       | Endpoints                                                                                                                                                                                                                                                                                                 | Phase            |
|----------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------------|
| system         | `GET /system/versions`, `/system/configuration[/section]`, `/system/preferences`, `/system/pipelines`, `/system/chains`                                                                                                                                                                                   | P1               |
| domains        | `GET/POST /domains`, `GET/DELETE /domains/{host}`, `/domains/{host}/lists`, `/domains/{host}/owners`, `/domains/{host}/uris`                                                                                                                                                                              | P1               |
| lists          | `GET/POST /lists`, `GET/DELETE /lists/{id}`, `/lists/{id}/config[/{attr}]` (GET/PUT/PATCH), `/lists/styles`, `?advertised=true`, `/lists/{id}/archivers`, `/lists/{id}/digest` (GET/POST send/bump), `/lists/{id}/uris`, `/lists/{id}/templates`                                                          | P1 (config) / P3 |
| roster/members | `/lists/{id}/roster/{owner,moderator,member,nonmember}`, `/lists/{id}/member/{email}`, `POST /members` (subscribe, `pre_*`, `invitation`, `send_welcome_message`, `role`), `GET/PATCH/DELETE /members/{id}`, `/members/{id}/preferences`, `/members/{id}/all/preferences`, `POST /members/find`, mass ops | P1               |
| held           | `GET /lists/{id}/held[?count&page]`, `GET /lists/{id}/held/{id}`, `POST /lists/{id}/held/{id}` (`action=accept\|reject\|discard\|defer`)                                                                                                                                                                  | P2               |
| requests       | `GET /lists/{id}/requests[?token_owner&request_type]`, `GET/POST /lists/{id}/requests/{token}` (`action=accept\|reject\|discard\|defer`)                                                                                                                                                                  | P3               |
| header-matches | `GET/POST /lists/{id}/header-matches`, `GET/PATCH/PUT/DELETE /lists/{id}/header-matches/{n}`, `DELETE` all                                                                                                                                                                                                | P2               |
| bans           | `/bans`, `/lists/{id}/bans`, `DELETE …/{email}`                                                                                                                                                                                                                                                           | P3               |
| users          | `GET/POST /users`, `GET/PATCH/DELETE /users/{id}`, `/users/{id}/addresses` (GET/POST), `/users/{id}/preferences`, `POST /users/{id}/login` (verify pwd — compat), `/users/{id}/all/preferences`                                                                                                           | P1               |
| addresses      | `GET /addresses/{email}`, `POST …/verify`, `…/unverify`, `GET/POST /addresses/{email}/user`, `DELETE` unlink, `/addresses/{email}/memberships`, `/addresses/{email}/preferences`                                                                                                                          | P1               |
| queues         | `GET /queues`, `GET /queues/{name}`, `POST /queues/{name}` (inject)                                                                                                                                                                                                                                       | P2               |
| templates/uris | `/templates/{id}`, `/uris`, list/domain uris                                                                                                                                                                                                                                                              | P3               |
| owners         | `GET /owners` (server owners)                                                                                                                                                                                                                                                                             | P1               |
| plugins        | `GET /plugins` (returns empty / lists Rust plugins)                                                                                                                                                                                                                                                       | P6               |
| archive (new)  | `/api/v1/archive/...` (search, threads, mbox export)                                                                                                                                                                                                                                                      | P5               |
| webhooks (new) | `/api/v1/webhooks`                                                                                                                                                                                                                                                                                        | P6               |
| Auth           | `/3.1/`: Basic (when `compat_basic_auth`, CIDR allowlist) or Bearer; `/api/v1/`: Bearer scoped token                                                                                                                                                                                                      | P1               |
| OpenAPI        | `/openapi.json` + SSR reference page at `/api/docs` (no embedded Swagger UI: no third-party assets loaded, no scripts — P1-API-DOCS-ORIGIN)                                                                                                                                                               | P1               |

### 4.11 Core — CLI (`mailman` → `listmngr`)

| Mailman                                                            | listmngr                                                                                                                                                                                                                                      | Phase |
|--------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|
| `start/stop/restart/status`                                        | `listmngr serve` (+ systemd), `listmngr status`                                                                                                                                                                                               | P0    |
| `conf`                                                             | `listmngr conf [--key]`                                                                                                                                                                                                                       | P0    |
| `version`                                                          | `listmngr version`                                                                                                                                                                                                                            | P0    |
| `info`                                                             | `listmngr info`                                                                                                                                                                                                                               | P0    |
| `create`, `remove`, `lists`                                        | `listmngr lists create/remove/ls`                                                                                                                                                                                                             | P1    |
| `addmembers`, `delmembers`, `members`, `syncmembers`, `findmember` | `listmngr members add/del/ls/sync/find`                                                                                                                                                                                                       | P1    |
| `digests`                                                          | `listmngr digests --send/--bump/--periodic`                                                                                                                                                                                                   | P3    |
| `inject`, `qfile`, `unshunt`                                       | `listmngr queue inject/show/unshunt`                                                                                                                                                                                                          | P2    |
| `aliases`                                                          | `listmngr aliases regen`                                                                                                                                                                                                                      | P2    |
| `notify`                                                           | `listmngr notify` (pending moderation reminders)                                                                                                                                                                                              | P3    |
| `gatenews`                                                         | `listmngr nntp gate`                                                                                                                                                                                                                          | P6    |
| `import21`                                                         | `listmngr import21 <list> config.pck` + `archive import mbox`                                                                                                                                                                                 | P6    |
| `withlist`, `shell`                                                | `listmngr shell` (small REPL with repo ops) or `listmngr eval --list …` — reduced scope                                                                                                                                                       | P6    |
| `reopen`                                                           | log rotation → not needed (tracing appender)                                                                                                                                                                                                  | –     |
| new                                                                | `listmngr migrate`, `listmngr user create/passwd/2fa`, `listmngr token create/revoke`, `listmngr dkim gen/rotate/dns`, `listmngr archive import/export/reindex`, `listmngr import3`, `listmngr doctor` (checks DNS/MTA/DB), `listmngr backup` | P0–P6 |

### 4.12 Postorius — Web UI (admin + member)

| Screen                  | Content                                                                                                                                                                                                                          | Phase |
|-------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|
| Accounts                | signup + verify email, login, logout, password reset, change password, TOTP setup/recovery codes, passkeys (WebAuthn), OIDC (Google/GitHub/generic OIDC), sessions list/revoke, delete account                                   | P4    |
| Profile                 | addresses (add/verify/primary/remove), display name, locale, timezone, per-scope preferences (global / per address / per subscription), subscriptions list (change delivery, unsubscribe), API tokens                            | P4    |
| List index              | filter advertised, by domain, search, role badges                                                                                                                                                                                | P4    |
| List summary            | description/info, subscribe form (anon → confirm), unsubscribe, link archive, member count, owners                                                                                                                               | P4    |
| List create             | choose domain, style, owner, advertised, description                                                                                                                                                                             | P4    |
| List settings           | the 9 groups of §4.1, one form per group, inline validation, diff preview + audit                                                                                                                                                | P4    |
| Members                 | rosters (members/nonmembers/owners/moderators), search/paginate, per-member options (moderation_action, delivery, preferences), mass subscribe (textarea/file, pre_confirm/pre_approve/invite/welcome), mass removal, export CSV | P4    |
| Held messages           | list + preview (rendered + raw), reason, bulk accept/reject/discard, reject reason, forward, "moderate sender", "ban sender", "add header match"                                                                                 | P4    |
| Subscription requests   | pending confirm / pending approval, accept/reject/discard/defer + reason                                                                                                                                                         | P4    |
| Unsubscription requests |                                                                                                                                                                                                                                  | P4    |
| Header filters          | CRUD, order, regex test                                                                                                                                                                                                          | P4    |
| Templates               | per list/domain/site, editor + preview + placeholders help, language                                                                                                                                                             | P4    |
| Bans                    | list + global                                                                                                                                                                                                                    | P4    |
| Delete list             | confirm + archive policy option                                                                                                                                                                                                  | P4    |
| Domains                 | CRUD, owners, templates, DKIM keys + DNS record display                                                                                                                                                                          | P4    |
| Users admin             | list/search users, edit roles, force verify, impersonate?, subscriptions                                                                                                                                                         | P4    |
| System                  | versions, config view (masked), runner/queue status, MTA maps status, audit log viewer                                                                                                                                           | P4    |
| Bounce                  | member bounce info, re-enable, bounce log                                                                                                                                                                                        | P4    |

### 4.13 HyperKitty — Archive

| Feature                                                                                                                                                                            | Phase | Change     |
|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|------------|
| Built-in archiver handler (`to-archive` → queue → index), no HTTP plugin needed                                                                                                    | P5    |            |
| Remote HyperKitty archiver (POST `/api/mailman/archive` key) for those who still want HyperKitty                                                                                   | P6    |            |
| Threading: `In-Reply-To` / `References` → parent; none → new thread; manual reattach                                                                                               | P5    |            |
| Message-ID-Hash URL `/archives/list/{list_id}/message/{hash}/`, `Archived-At` header                                                                                               | P5    | compat URL |
| List overview: recent activity, active/popular threads, top posters, thread count, participants                                                                                    | P5    |            |
| Thread list: latest, by year/month, unread indicator (last view)                                                                                                                   | P5    |            |
| Thread page: messages, quote fold, attachments, votes ±1, tags, category, favorite, permalink, reply/new thread (web post → injected through the pipeline with a verified address) | P5    |            |
| Sender pages, user profile (posts, votes, favorites, subscriptions)                                                                                                                | P5    |            |
| Search: list + site-wide, facets (list, sender, date), highlight                                                                                                                   | P5    | tantivy    |
| Export mbox (thread/month/list, gzip)                                                                                                                                              | P5    |            |
| Import mbox (`listmngr archive import`) + reindex                                                                                                                                  | P5    |            |
| Private archive: auth + membership check; `never` = not stored                                                                                                                     | P5    |            |
| Admin: delete message/thread, hide, reattach, category CRUD                                                                                                                        | P5    |            |
| Rendering: `text`/`markdown` mode, link auto, emoji, gravatar (opt-in, proxied), email obfuscate                                                                                   | P5    |            |
| RSS/Atom feed per list/thread                                                                                                                                                      | P5    | new        |
| Attachments served safely (nosniff, download disposition, separate path)                                                                                                           | P5    |            |

### 4.14 django-mailman3 / mailman-web

| Feature                                                                                                     | Phase |
|-------------------------------------------------------------------------------------------------------------|-------|
| Account = Mailman user (no two-way sync needed)                                                             | P4    |
| Account email addresses = addresses (shared verification)                                                   | P4    |
| Timezone/locale profile                                                                                     | P4    |
| Social login (allauth) → OIDC/OAuth2 providers                                                              | P4    |
| `mailman-web` cli (migrate, collectstatic, qcluster) → `listmngr migrate`, no collectstatic/qcluster needed | P0    |

### 4.15 Improvements (not in Mailman)

- DB-backed queue, crash-safe, multi-node; no mail lost on a mid-flight crash (Mailman qfiles rename dance).
- Scoped API tokens, audit log, webhook (events: message.held, member.subscribed, list.created…).
- In-app DKIM key mgmt (gen/rotate/DNS record), ARC seal, `Authentication-Results` written into the archive.
- RFC 8058 one-click unsubscribe.
- TOTP/WebAuthn/OIDC built-in, session mgmt.
- Prometheus metrics (queue depth, delivery latency, bounce rate), `listmngr doctor`.
- SQLite mode: 1 file, no Postgres, for small lists/dev.
- Dark mode, responsive, a11y, htmx partial updates, keyboard shortcuts for the moderation queue.
- GDPR: export/erase user data.
- OpenAPI + typed SDKs auto-generated (TS/Python) from the spec.

## 5. Security design

### 5.1 Auth & session

| Item                    | Design                                                                                                                                                                                                                                |
|-------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Password                | Argon2id (m=64MiB, t=3, p=1), optional pepper (env), `zxcvbn` score ≥ 3, no character restrictions other than length ≤ 1024                                                                                                           |
| Lockout                 | progressive delay keyed by (account, IP); does not reveal whether a user exists                                                                                                                                                       |
| 2FA                     | TOTP (RFC 6238, drift ±1), 10 recovery codes hashed; WebAuthn passkeys (resident/non-resident); mandatory for server_owner (config)                                                                                                   |
| OIDC                    | Authorization Code + PKCE, `state`/`nonce`, email trusted only when `email_verified=true`, no auto-linking of unverified emails                                                                                                       |
| Session                 | `tower-sessions` DB store, cookie `__Host-lm_session`, `Secure; HttpOnly; SameSite=Lax`, ID rotated on login/privilege change, idle 12h/absolute 7d, revoke all                                                                       |
| CSRF                    | double-submit token bound to the session + `Sec-Fetch-Site`/`Origin` check; htmx sends `X-CSRF-Token` via global `hx-headers`                                                                                                         |
| API token               | `lm_<id>_<secret>`; DB stores SHA-256(secret); scopes: `system:read`, `lists:read`, `lists:write`, `members:read`, `members:write`, `moderation`, `users:write`, `archive:write`, `admin`; list/domain restriction; expiry; last_used |
| Basic compat            | only when `compat_basic_auth=true` + IP in allowlist; user/pass = token id/secret                                                                                                                                                     |
| Email confirm tokens    | 32 byte CSPRNG, base32 lower, stored as SHA-256, TTL `pending_request_life`, single-use, compared with `subtle::ConstantTimeEq`                                                                                                       |
| `Approved:` posting key | Argon2 hash per list; strip header + body line; no 2.1-style list password                                                                                                                                                            |

### 5.2 Email

| Item             | Design                                                                                                                                   |
|------------------|------------------------------------------------------------------------------------------------------------------------------------------|
| Inbound verify   | SPF/DKIM/DMARC via `mail-auth` → `Authentication-Results` (trusted authserv-id), results stored in msgdata; rule `validate-authenticity` |
| DMARC mitigation | look up `_dmarc.<from-domain>` and the org domain (PSL), `p=reject/quarantine` (+ `sp=`, `pct=`) → munge_from / wrap / reject / discard  |
| Outbound DKIM    | sign `From/To/Subject/Date/List-*` + body; key per domain, RSA-2048 + Ed25519 (dual), rotate, DNS record UI                              |
| ARC              | seal when the list modifies content (subject prefix/footer/munge) → preserves deliverability                                             |
| VERP             | always a dedicated sender; HMAC bounce token to prevent forged bounces                                                                   |
| Loop/abuse       | `loop` rule, `X-Loop`, max hops, rate-limit per sender & per list (posts/hour, config)                                                   |
| Size/DoS         | reject at LMTP if > `max_message_size` site-wide; MIME depth ≤ 20, parts ≤ 1000, header count ≤ 500; streaming write to store            |
| Attachments      | content-type sniff, block executable extensions per list config, serve `Content-Disposition: attachment` + `nosniff`                     |
| Address parse    | strict RFC 5322 + IDNA, reject control chars, normalize case for matching but keep `original_email`                                      |

### 5.3 Web

| Item          | Design                                                                                                                                                                    |
|---------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| CSP           | `default-src 'self'; script-src 'self' 'nonce-…'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'; form-action 'self'`; htmx `allowEval=false`, no `hx-on` |
| Headers       | HSTS (when TLS), `X-Content-Type-Options`, `Referrer-Policy: strict-origin-when-cross-origin`, `Permissions-Policy`                                                       |
| HTML sanitize | `ammonia` allowlist for archive/markdown; no remote images (proxy opt-in); links `rel="noopener nofollow ugc"`                                                            |
| Rate limit    | `governor` keyed IP/account: login, signup, reset, subscribe, search, API                                                                                                 |
| Proxy         | `trusted_proxies` CIDR required before `X-Forwarded-For` is trusted                                                                                                       |
| Privacy       | `hide_address`, roster visibility, archive obfuscate `user at domain`, `anonymous_list`, robots noindex for private                                                       |
| Upload        | mass subscribe file ≤ 5MB, streaming parse                                                                                                                                |
| Errors        | no stack exposed; error id correlated with logs                                                                                                                           |

### 5.4 Data & ops

| Item         | Design                                                                                                                                                      |
|--------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Secrets      | `SecretString` + `zeroize`; no Debug/log; `*_file` config; DKIM private key + TOTP secret encrypted at rest with `security.master_key` (XChaCha20-Poly1305) |
| Audit        | every change to config/roles/moderation/user data → `audit_log` (actor, ip, diff); UI viewer; export                                                        |
| Backup       | `listmngr backup` (pg_dump + message store) + restore doc                                                                                                   |
| Supply chain | `Cargo.lock` commit, `cargo deny` (advisories, licenses, dup), `cargo audit` CI, SBOM (CycloneDX), cosign sign image, Dependabot                            |
| Build        | `unsafe_code = forbid`, clippy `pedantic` + `nursery` (warn), `-D warnings` CI, `overflow-checks` release                                                   |
| Container    | static musl or distroless, non-root uid 1000, read-only FS, `cap_drop ALL`, healthcheck                                                                     |
| systemd      | `ProtectSystem=strict`, `PrivateTmp`, `NoNewPrivileges`, `ProtectHome`, `SystemCallFilter`                                                                  |
| TLS          | reverse proxy preferred; option `web.tls` rustls + ACME (phase 6)                                                                                           |
| Threat model | `docs/SECURITY.md`: assets, actors (anon, member, moderator, owner, server owner, MTA, attacker via mail), STRIDE table, mitigations map                    |
| Disclosure   | `SECURITY.md` + `security.txt`                                                                                                                              |

## 6. UI/UX

### 6.1 Principles

- SSR (askama) + htmx for partial updates (moderation queue, member table, search). No SPA, no Node build step for the runtime.
- Plain CSS with design tokens (color, spacing, type scale), dark mode via `prefers-color-scheme` + toggle, responsive mobile-first.
- A11y: semantic HTML, focus ring, ARIA for htmx swaps (`aria-live`), contrast AA, keyboard (moderation: `a` accept, `r` reject, `d` discard, `j/k`).
- i18n UI strings (fluent), language from user pref → Accept-Language.
- Consistent layout: sidebar (list nav) + content; breadcrumbs; command palette (`/`) to find lists/users.

### 6.2 Information architecture

```
/                               list index (advertised), search
/lists/{list_id}/               summary + subscribe
/lists/{list_id}/settings/{section}
/lists/{list_id}/members/{role}
/lists/{list_id}/held
/lists/{list_id}/requests/{subscriptions|unsubscriptions}
/lists/{list_id}/header-matches
/lists/{list_id}/templates
/lists/{list_id}/bans
/lists/{list_id}/delete
/domains, /domains/{host}/...
/users, /users/{id}
/account/{profile|addresses|security|sessions|tokens|subscriptions|preferences}
/system/{status|config|audit|queues}
/archives/                      list overview
/archives/list/{list_id}/       overview
/archives/list/{list_id}/latest, /{year}/{month}/, /thread/{hash}/, /message/{hash}/, /export/…
/archives/search?q=
/archives/users/{id}
/moderation                     held + requests of every list I manage, combined
```

## 7. Roadmap

Relative effort: S < M < L < XL. A phase tag is created only after its acceptance gates pass and CHANGELOG/FEATURE_PARITY carry the evidence; a package version alone does not imply phase completion. ADR-0003 established `0.1.0` as the unreleased development baseline through Phase 7, so the earlier mechanical `v0.<phase>.0` rule does not apply retroactively to Phase 0; `1.0.0` (annotated tag `v1.0.0`, 2026-10-04, `P7-1.0`) is the first release.

### 7.0 Execution conventions

- **One acceptance ID = one branch** `feat/<id-slug>` cut from `main`; pass
  every gate in CLAUDE.md (including PostgreSQL and `scripts/test-mailmanclient.py`
  when REST is touched) → merge `--no-ff` → delete the branch. Do not stack
  several features on one branch.
- Each work package below has an ID `P<phase>-<NAME>`; the same-ID ledger row records
  the exact command and result. Roadmap checkboxes are ticked only when the ledger row exists.
- **Stage ↔ Phase**: some ledger rows refer to work packages by letter (A–F,
  e.g. "B6", "C4", "D4", "F3") from an earlier execution plan kept outside the
  repo: A–C = Phase 2–3 (done), D = Phase 4, E = Phase 5, F = Phase 6. From now
  on only `P<phase>-*` IDs are used.
- **Web UI**: server-rendered per ADR-0002 — askama + vendored htmx 2.x + plain CSS,
  no Node runtime, no SPA. The "Stage D SPA" wording in the ledger was drift and has
  been corrected; ADR-0004 records the reconciliation decision and the path for moving
  the existing `format!` pages to askama, one Phase 4 work package at a time.

### Phase 0 — Bootstrap (S)

Goal: workspace compiles, running `serve` answers healthz, CI green. Checkbox indicates artifact presence only; formal completion follows the ID-based acceptance ledger in `FEATURE_PARITY.md`.

- [x] Workspace with 9 crates (lib + bin), `[workspace.dependencies]`, lints, exact Rust toolchain, rustfmt/clippy/deny config
- [x] `listmngr-core`: `Config` (figment), `Error`, ids (`ListId`, `UserId`…), basic enums
- [x] `listmngr-db`: pool init (pg/sqlite), `migrate!()` infra, migration `0000_init`
- [x] `listmngr` CLI: `version`, `conf`, `info`, `migrate`, `serve` (axum `/healthz`, `/readyz`, Prometheus `/metrics`), tracing init
- [x] **P0-08** CI GitHub Actions: blocking fmt, locked build, clippy `-D warnings`, tests, mandatory `TEST_POSTGRES_URL` migration/connectivity gate, deny, audit; cache
- [x] **P0-09** Multi-stage static-musl/scratch non-root Dockerfile, PostgreSQL Compose with runtime credentials, `.env.example`, `.dockerignore`, tool-free healthcheck
- [x] **P0-10** Hardened systemd unit with managed state/working directory, syscall filter, and kernel/device/filesystem protections
- [x] **P0-11** Actionable CLAUDE, README, ARCHITECTURE, FEATURE_PARITY, SECURITY, disclosure, deploy/MTA-boundary docs, and ADRs
- [x] **SEC-10** Supply-chain/deployment baseline: lockfile, pinned CI/action/tool/image inputs, deny/audit policy, full AGPLv3 text, least-privilege container/systemd artifacts
- Acceptance: `scripts/check-phase0-artifacts.sh`; `cargo build --locked --workspace`; `cargo test --locked --workspace --all-targets`; `TEST_POSTGRES_URL=… scripts/test-postgres.sh`; `docker compose --env-file .env -f deploy/docker-compose.yml up --build --wait` → host `/healthz` and `/readyz` 200. Artifact checkboxes do not assert these live gates passed; evidence/status is recorded in `docs/FEATURE_PARITY.md`.

### Phase 1 — Core domain, DB, basic REST (M)

Goal: create domain/list/user/member via REST + CLI; `mailmanclient` subset runs.

- [x] Migrations: domains, users, credentials, addresses, mailing_lists (+settings columns), members, preferences, api_tokens, audit_log, header_matches, bans, templates, list_styles
- [x] Repos (sqlx): Domain, User, Address, List, Member, Preferences (layered resolve), Token, Audit
- [x] Styles: 3 built-in + applied on create
- [x] REST `/3.1/`: system, domains, lists (+config GET/PUT/PATCH for all attrs), styles, users, addresses, members (roster, subscribe pre_* only, preferences), owners, find
- [x] Auth: Bearer token + scopes; Basic compat (allowlist); rate-limit
- [x] `/api/v1/` sharing the same handlers, OpenAPI (utoipa), SSR reference page `/api/docs` served from the same origin
- [x] CLI: `lists create/remove/ls`, `members add/del/ls/sync/find`, `user create/passwd`, `token create/revoke`, `domains add/rm/ls`
- [x] Audit recorded for every write
- [x] Acceptance: integration tests on PostgreSQL + SQLite cover the Phase 1 CRUD contract; Python `mailmanclient==3.3.5` script passes create domain → list → subscribe → set config → roster; exhaustive differential test covers nullable preference layering. Exact evidence is recorded in `docs/FEATURE_PARITY.md`.

### Phase 2 — Mail path (L)

Goal: send a message to the list → members receive it; hold/accept via REST.

Checkboxes are ticked per the ledger rows named alongside; each row's deviations remain in effect.

- [x] Message store (fs, db) + `messages` index + `Message-ID-Hash` — P2-STORE, P2-ARCHIVE-AUTHORITY (fs store is a standalone library; CLI/LMTP intake uses the DB; GC via P3-TASK-RUNNER)
- [x] Queue (`queue_jobs`) + claim/backoff/shunt + runner supervisor + graceful shutdown — P2-QUEUE, P2-RUNTIME, P2-LEASE-HEARTBEAT, P2-DELIVERY-POLICY
- [x] LMTP server (RFC 2033: LHLO, MAIL, RCPT, DATA, RSET, NOOP, QUIT, PIPELINING, SIZE, 8BITMIME, per-recipient status), sub-address routing, early reject — P2-TRANSPORT, P2-LMTP-PARAMETERS
- [x] `in` runner + chains + 15 rules (§4.3 except news/digests) + header-match chain + DMARC lookup + `munge_from` — P2-CHAIN-ENGINE, P2-CHAIN-RULES, P2-DMARC-MUNGE, P2-VALIDATE-AUTHENTICITY, P2-HEADER-MATCHES-REST
- [x] Held messages: DB, notices (owner/user), REST held endpoints + actions — held REST, P2-TEMPLATES (hold notices), P3-MODERATOR-REJECTION-NOTICE, P2-HELD-FORWARD
- [x] Pipeline runner + handlers §4.4 (except to-usenet, arc-sign): full mime-delete, decorate + placeholders, personalize, VERP — P2-PIPELINE-HANDLERS, P2-MIME-DELETE, P2-HANDLERS-DECORATE, P2-COOK-HEADERS, P2-PERSONALIZE-VERP
- [x] Templates engine (built-in `mailman:///` bodies ported from Mailman en) + loader DB/file/http — P2-TEMPLATES (`https://` accepted but not fetched inside the transaction; see deviation)
- [x] `out` runner: `mail-send`, chunk, TLS, DKIM sign (dkim_keys + CLI `dkim gen/dns`), `retry`, `virgin`, `bad` — P2-RECIPIENT-LIMIT, P2-STARTTLS, P2-SMTP-AUTH, P3-DKIM, P3-DKIM-BODY, P2-DELIVERY-POLICY
- [x] RFC 2369/8058 headers + HTTP one-click unsubscribe endpoint (token HMAC) — P2-COOK-HEADERS, P2-ONE-CLICK-UNSUBSCRIBE
- [x] Postfix/Exim map generation + `aliases regen`; docker-compose adds postfix — P2-MTA-INTEGRATION, P2-MTA-MAPS
- [x] CLI `queue inject/show/unshunt`, `status` — P2-CLI, P2-RUNTIME (+ `queue recipients/resolve`)
- [x] Metrics: queue depth, deliveries, latency — P2-METRICS
- [x] REST `/queues` — P2-QUEUES-REST
- [x] **P2-HELD-FORWARD** (S): `forward=True&forward_to=…` on `POST /lists/{id}/held/{id}` as in Mailman/Postorius — wraps the original as `message/rfc822`, sent from `-bounces`, combinable with every action including `defer`; `moderation_log.forward_to` + audit.
- [x] **P2-E2E-ACCEPTANCE** (M): the acceptance matrix below in `crates/cli/tests/mailpath_e2e.rs` (real binary + SMTP sink): member delivery, nonmember hold, accept-once, headers (List-*, subject prefix, footer) + DKIM `dkim=pass` with a test key, ban → rejection notice, max-size → hold, `personalize=full` + `verp_personalized_deliveries` → N VERP msgs + one-click, inject while the server is down → delivered exactly once, SIGKILL mid-out (relay holding DATA) → nothing lost, no double delivery. A kill in the middle of the `in` stage cannot be scheduled in the harness; lease expiry there is covered by the repository contract.
- Acceptance: e2e test harness = pg + smtp sink (Rust mock or `mailhog`) → send via LMTP → assert N members receive, headers correct (List-*, subject prefix, footer, DKIM verify passes with the test key), held → accept → delivered; nonmember → hold; ban → reject DSN; max-size → hold; `personalize=full` → N separate msgs with correct VERP; crash mid-pipeline → job not lost (kill -9 test).

### Phase 3 — Subscription, commands, bounces, digests (L)

Goal: complete Core parity (except NNTP/DMARC wrap).

- [x] Subscription/unsubscription workflow state machine + pendings + tokens + policies + invitations + moderation requests + REST `requests` — P3-SUBSCRIPTION-POLICY, P3-SUBSCRIPTION-REQUESTS-REST, P3-ADMIN-SUBSCRIBE, P3-HTTP-CONFIRM-RECEIPT, P3-EMAIL-CONFIRM-RECEIPT
- [x] Email command runner: `confirm`, `join/subscribe`, `leave/unsubscribe`, `help`, `echo`, `end/stop`; `-request/-join/-leave/-confirm` routing; autoresponder + grace period; administrivia — P3-EMAIL-COMMANDS, P3-AUTORESPONDER, P3-HELP-REPLY
- [x] Welcome/goodbye/invite/hold/refuse/rejected/warning/probe notices; template scopes + language fallback; REST templates/uris; ban REST — P2-TEMPLATES, P3-WELCOME, P3-GOODBYE, P3-ADMIN-SUBSCRIBE (invite), P3-MODERATOR-REJECTION-NOTICE, P3-BOUNCE-INCREMENT-NOTICE, P3-BOUNCE-DISABLE-NOTICE, P3-BOUNCE-PROBES, P3-LIST-POSTING-BANS, P3-BAN-RESOURCE-GET, P3-SITE-BANS-REST
- [x] Bounce runner: VERP decode, DSN + ENVID, events/scoring/stale/disable/owner notices, forward unrecognized, detectors + fixture corpus, warnings + removal, probes — P3-BOUNCE-RUNNER, P3-BOUNCE-DETECTORS, P3-BOUNCE-PROBES, P3-BOUNCE-MAINTENANCE, P3-BOUNCE-SCHEDULER, P3-BOUNCE-INBOX, P3-BOUNCE-ACK, P3-DIRECT-BOUNCE-SCORE, P3-SMTP-BOUNCE-EVENT
- [x] Digest runner: durable collection (DB, not mbox), thresholds/periodic, MIME + RFC1153 builders, volume/number rollover, masthead/header/footer, CLI `digests`, REST `/lists/{id}/digest` — P3-DIGEST-SETTINGS, P3-DIGEST-REST
- [x] `task` runner: expire pendings/workflows, cleanup orphan messages (refcount), stale bounce reset — P3-TASK-RUNNER
- [x] `notify` (pending reminders), `admin_notify_mchanges`, `admin_immed_notify` (held posts) — P3-TASK-RUNNER, P3-ADMIN-NOTIFY-MCHANGES, P2-TEMPLATES
- [x] i18n framework (fluent) + `en`, `vi` — P3-I18N; import `.po` Mailman templates (tooling) → P6
- [x] **P3-DUAL-BACKEND-CI** (M): CI runs the SQLite suite (every non-ignored test), the selective PG gate (`scripts/test-postgres.sh`, 38) and **every** `#[ignore]`d PostgreSQL test (`scripts/test-postgres-all.sh`, 53) on the same disposable server; each test creates/drops its own schema via `listmngr_db::test_support::IsolatedSchema`, no separate empty DB needed, no silent fallback.
- [x] ~~**P3-CLIENT-SUITE**~~ (done): `tests/compat/mailmanclient_suite.py` — the `using.rst` doctest of mailmanclient 3.3.5 section by section (18 sections, 232 checks, 7 documented deviations) run via `scripts/test-mailmanclient.py` after the Phase 1 flow and the held flow; additional REST compat so the real client runs correctly (styles, `GET /members`, mass unsubscribe, users/addresses/preferred_address, `lists/find`, role routes, archivers dict, `when`, `respond_and_continue`, Mailman's hold reason…); plus two general fixes: 500s logged with a correlation id, per-connection SQLite pragma + `BEGIN IMMEDIATE` for every write.
- [x] ~~**P3-DIGEST-SNAPSHOT**~~ (done) (S): RFC 1153/MIME snapshot compared against Mailman 3.3 output from real fixtures — `tests/compat/generate_mailman_digest.py` runs Mailman 3.3.10's own `RFC1153Digester`/`MIMEDigester` into `crates/mail/tests/fixtures/digests/`; `crates/mail/tests/digest_snapshot.rs` compares line by line (plain) and part by part (MIME) instead of insta; renderer changed to follow Mailman (masthead wrapped at 70 columns, header before TOC, TOC drops the subject prefix and takes the display name, kept headers in Mailman's order, scrubber note, `Message: n` in MIME posts, no more closing part); three Mailman quirks are named and not copied.
- [x] ~~**P3-SUBSCRIPTION-E2E**~~ (done) (S): confirm-token round-trip over email in the e2e harness (`-join` → challenge → `-confirm` → member; `-leave` likewise) — `join_and_leave_by_mail_round_trip_their_confirmation_tokens` in `crates/cli/tests/mailpath_e2e.rs` (real binary, LMTP in, SMTP sink out: challenge, receipt, welcome, post delivered, new challenge for leave, goodbye, replaying a used token does nothing); the per-address cooldown fixed to count only pending requests (`consumed=0`), because previously a leave right after a join was silently swallowed.
- Acceptance: full `mailmanclient` doctest-equivalent suite pass; bounce corpus detection ≥ flufl.bounce; digest snapshot tests (insta) compared with Mailman output; subscription flow e2e over email (confirm token round-trip).

### Phase 4 — Web UI, Postorius parity (L)

**Architecture (ADR-0002, reaffirmed by ADR-0004):** server-rendered. askama
(compile-time, auto-escape) + htmx 2.x vendored (progressive enhancement; every
form works without JS) + plain CSS with design tokens + strict CSP with nonce,
no Node runtime, no SPA. The existing pages (`crates/api/src/webui.rs`,
HTML built with `format!` via `crates/web`) are a temporary slice: from P4-SHELL
onwards every screen renders via askama; no two rendering paths are kept in
parallel after P4-SHELL merges.

Status: login/password session (CSRF, exact Origin), list index with
filters/badges + list create + list summary (P4-LIST-CREATE-INDEX),
subscribe/confirm, scoped held review, members/policy, a small settings slice,
a small `/moderation` slice, own postings, bounce recovery (rows `P4-WEB-*`,
P4-LIST-COPY, P4-BOUNCE-WEB-RECOVERY) are in place — and from P4-SHELL, everything
renders via askama with the en/vi i18n shell, design tokens + dark mode, a11y
baseline. Not yet: full accounts, 2FA/OIDC, full admin.

Work packages, in order; one ID per branch:

| ID                              | Effort | Scope                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | Own acceptance                                                                                                                                                                                |
|---------------------------------|--------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| ~~P4-SHELL~~ (done)             | M      | askama layout + design tokens + dark mode; security headers, rate limit, `trusted_proxies` (already present); htmx vendored (hash pinned, no CDN, no page loads it yet → no CSP nonce needed yet); UI i18n with Fluent (`en`, `vi`, same `listmngr_i18n` catalog); a11y baseline (landmarks, focus, skip link, `aria-current`); the 20 existing routes + one-click + archive compat moved to templates                                                                                        | PASS: axe no critical/serious on 4 pages; no CSP violation; `scripts/test-webui-browser.py` PASS with behaviour unchanged                                                                     |
| ~~P4-ACCOUNTS~~                 | —      | **Split** into the IDs below: one vertical = one branch. Reason: signup/reset/addresses need a mail path outside list scope (the outbound runner currently requires `context["list_id"]`), so P4-SITE-NOTICES must come first; the remaining parts do not touch mail and can go ahead first.                                                                                                                                                                                                  | (see each ID)                                                                                                                                                                                 |
| ~~P4-ACCOUNT-SESSIONS~~ (done)  | S      | List one's own browser sessions (opaque id, created/expiry timestamps, current session marked), end one session or all other sessions; each write one audit event in the same transaction                                                                                                                                                                                                                                                                                                     | PASS: `crates/api/tests/webui/sessions.rs` + PostgreSQL contract in `scripts/test-postgres.sh`                                                                                                |
| ~~P4-ACCOUNT-PROFILE~~ (done)   | S      | display name, locale, timezone; the user's locale wins over `Accept-Language` in the shell; one validator shared by the form and the REST user patch                                                                                                                                                                                                                                                                                                                                          | PASS: `crates/api/tests/webui/profile.rs` + PostgreSQL contract; Chromium switches to `vi` and back                                                                                           |
| ~~P4-SITE-NOTICES~~ (done)      | M      | Site-level mail (belonging to no list): envelope from `site.site_owner`, outbound handles jobs without `list_id`, site-scope templates (`site:user:action:verify`/`reset`, en+vi). Prerequisite for signup/reset/addresses                                                                                                                                                                                                                                                                    | PASS: `crates/runners/src/site_notice_tests.rs` — sink receives `MAIL FROM:<>`, From = site owner, DKIM by site domain, site template override, no list binding                               |
| ~~P4-ACCOUNT-SIGNUP~~ (done)    | M      | signup + verify email (SHA-256-hashed token, single use, 24h, 1 message/address/hour), responses do not reveal account existence, rate limit; `web.signup` switch                                                                                                                                                                                                                                                                                                                             | PASS: `crates/api/tests/webui/signup.rs` signup → verify → login + PostgreSQL contract; Chromium signup                                                                                       |
| ~~P4-ACCOUNT-RESET~~ (done)     | S      | password reset via email token (change password already exists); every session of the account ends                                                                                                                                                                                                                                                                                                                                                                                            | PASS: `crates/api/tests/webui/reset.rs` request → confirm → new login + PostgreSQL contract; Chromium                                                                                         |
| ~~P4-ACCOUNT-ADDRESSES~~ (done) | M      | addresses add/verify/primary/remove; remove = unlink + forget verified, keep membership; another account's address → silent                                                                                                                                                                                                                                                                                                                                                                   | PASS: `crates/api/tests/webui/addresses.rs` add → verify → primary → remove + PostgreSQL contract; Chromium                                                                                   |
| ~~P4-ACCOUNT-TOKENS~~ (done)    | S      | User API tokens (create/list/revoke) in the UI; scopes by authority: a list owner only list scopes bound to their own lists, the server owner every scope unbound                                                                                                                                                                                                                                                                                                                             | PASS: `crates/api/tests/webui/tokens.rs` + PostgreSQL contract; token shown only once; revocation immediate; Chromium                                                                         |
| ~~P4-ACCOUNT-DELETE~~ (done)    | S      | account deletion (the erase root for P4-GDPR): password re-confirmation; membership/address/token/credential/session/domain-owner deleted in the same transaction; the last server owner is refused                                                                                                                                                                                                                                                                                           | PASS: `crates/api/tests/webui/delete_account.rs` + PostgreSQL contract; one `member.delete` audit per membership                                                                              |
| ~~P4-TOTP~~ (done)              | S      | TOTP (RFC 6238, drift ±1, each code once) + 10 hashed recovery codes; server-side QR SVG; `require_2fa_for = ["server_owner"]` blocks admin/moderation/token-mint pages until enrolled                                                                                                                                                                                                                                                                                                        | PASS: `crates/api/tests/webui/totp.rs` + PostgreSQL contract; Chromium enrol → 2-step login → recovery code                                                                                   |
| ~~P4-WEBAUTHN~~ (done)          | M      | passkeys (`webauthn_rp` pure Rust — no OpenSSL, keeps the musl build), register/delete keys, passwordless login; passkey = second factor; the only first-party script `passkeys.js` under `script-src 'self'`                                                                                                                                                                                                                                                                                 | PASS: `crates/api/tests/webui/passkeys.rs` with an ES256 soft authenticator + PostgreSQL contract; Chromium CDP virtual authenticator                                                         |
| ~~P4-OIDC~~ (done)              | M      | Generic OIDC (Google works via standard discovery; GitHub is OAuth2 only → not supported); Authorization Code + PKCE S256, `state`/`nonce`, ID token verified RS256/ES256 via JWKS; link/unlink under account (unlink requires password, refuses removing the last way in); JIT account only when `email_verified`, random password `usable=0`; login via provider still goes through the TOTP step if enabled                                                                                | PASS: `crates/api/tests/webui/oidc.rs` with a mock provider (`mock_oidc.rs`) + PostgreSQL contract                                                                                            |
| ~~P4-LIST-SETTINGS~~ (done)     | L      | the 9 groups of §4.1, one form per group on the same patch engine/validator as the REST config; inline per-field refusal (400 keeps the entered values); before/after diff preview without writing; header rules CRUD/move up-down/test a value; bans list + site (`/web/admin/bans`); templates catalogue + editor + placeholder preview + language; digest send/bump; archivers (recorded, effective with P5-REMOTE-ARCHIVERS); delete list by retyping the id + stating the archive policy | PASS: `crates/api/tests/webui/list_settings_groups.rs` + PostgreSQL contract; Chromium: preview/refusal/save, rule add-test-remove, ban, template preview, delete refused                     |
| ~~P4-MEMBERS~~ (done)           | L      | rosters for 4 roles (`?role=`), search/paginate: htmx swaps `#roster` on `HX-Request`, full page otherwise; per-member options (moderation_action, display name, role, delivery mode/status, ack/hide/list copy/own posts/language with Inherit) + effective values; bounce score + reset/re-enable; mass subscribe via textarea/file (multipart) through the registrar workflow with `pre_*`/invitation, per-address outcome; mass removal by select/paste; CSV export (10k)                 | PASS: `crates/api/tests/webui/members_admin.rs` (fragment vs full page) + PostgreSQL contract; Chromium: htmx swap keeps the document, mass subscribe, options, bounce reset, export, removal |
| ~~P4-HELD-QUEUE~~ (done)        | M      | held list + preview (From/To/Date decode, text body, raw 64 KiB), per-post decision + bulk (1 tx, skips already-decided posts), reject reason → notice, forward_to (refuses the list's own address inline), moderate sender (member override / nonmember row), ban sender, header rule prefill link, `moderation.js` keyboard shortcuts j/k/a/r/d/h/s/?; requests queue accept/reject/discard/defer; index counts held/requests                                                               | PASS: `crates/api/tests/webui/held_queue.rs` + PostgreSQL contract; Chromium: sender policy, bulk discard, accept by key, request accept                                                      |
| ~~P4-LIST-CREATE-INDEX~~ (done) | S      | list create (domain, style, owner, advertised, description) for server owner/domain owner, one transaction comprising list + owner + audit; list index filters (search/domain/`show=all` for one's own non-advertised lists, role badges); list summary (address, policies, roles, settings link) + subscribe (anon → confirm)                                                                                                                                                                | PASS: `crates/api/tests/webui/list_create_index.rs` create → anon join request + PostgreSQL contract; Chromium: create list, duplicate id refused inline, filtered directory, summary         |
| ~~P4-DOMAINS-USERS~~ (done)     | M      | domains index/add/delete (retype host, only when empty) + owners add/remove + domain-scope templates (same editor) + DKIM DNS record from the configured key (no key generation); users admin: search, display name + server owner (blocks demoting the last owner), force verify/unverify address, memberships                                                                                                                                                                               | PASS: `crates/api/tests/webui/domains_users.rs` + PostgreSQL contract; Chromium: add domain, owner add/remove, delete refused then deleted, find and edit an account                          |
| ~~P4-SYSTEM~~ (done)            | S      | versions + DB backend; masked config (`redacted_json`, dotted keys); queue depth by state + oldest ready + runners holding leases; MTA maps status (kind/directory/target/generation `current`); audit log viewer filtered by action prefix + target, paginated                                                                                                                                                                                                                               | PASS: `crates/api/tests/webui/system.rs` + PostgreSQL contract; Chromium: system page, audit filter                                                                                           |
| ~~P4-MODERATION-CROSS~~ (done)  | S      | `/moderation` cross-list aggregate: every held post + request of the lists one moderates (macro shared with the per-list queue, cap 50/kind), decisions return to the aggregate page (`back=moderation`); no cross-list bulk                                                                                                                                                                                                                                                                  | PASS: `crates/api/tests/webui/moderation_cross.rs` + PostgreSQL contract; Chromium: aggregate page with 3 held + 1 request                                                                    |
| ~~P4-GDPR~~ (done)              | S      | JSON export (account, preferences, addresses, memberships, token metadata, domains owned, sessions count, audit events) for oneself and for admins; erase via the admin page (retype the address, blocks the last owner) and CLI `user export/erase`; no archive anonymisation                                                                                                                                                                                                                | PASS: `crates/api/tests/webui/gdpr.rs` + PostgreSQL contract + `crates/cli/tests/gdpr.rs`; Chromium: JSON export, erasing the last owner refused                                              |
| ~~P4-ACCEPTANCE~~ (done)        | M      | Playwright journey at viewport 390×844 (signup → verify → login → create list → subscribe+confirm → post held → accept → settings → logout) with a token/owner/post bridge from the Rust harness; axe at every stop; Lighthouse a11y ≥ 95 on 4 pages; CSP (old harness); CI job `browser` runs both harnesses with tool pins                                                                                                                                                                  | PASS: `chromium_acceptance_journey` + `scripts/test-webui-journey.py`; `.github/workflows/ci.yml` job `browser`                                                                               |

- Acceptance: Playwright e2e (signup → create list → subscribe → post → moderate → settings), axe a11y with no critical errors, no CSP violation, Lighthouse a11y ≥ 95, mobile viewport pass — **passed** via P4-ACCEPTANCE (ledger); "post" is a held message inserted by the harness, the mail path has its own e2e.

### Phase 5 — Archive, HyperKitty parity (L)

Status: `crates/archive` has parsing, threading root ids, HyperKitty-compatible
Message-ID-Hash, `Archived-At`, basic SSR archive reading
(`/web/lists/{id}/archive`, mbox export ≤ 20 messages) — P2-ARCHIVE-AUTHORITY,
P2-DSN-INSPECTION; P5-RENDER added sender/date/parent, the thread tree, separately
stored attachments, safe text/markdown rendering, obfuscation, reattach, gravatar proxy.
P5-SEARCH added the tantivy index, P5-UI the overview/threads/sender/feed pages, P5-INTERACTIONS votes/tags/categories/favourites, P5-WEB-POST posting from the web, P5-MBOX mbox import/export. P5-ADMIN added the archive admin page and P5-REMOTE-ARCHIVERS three remote archivers. P5-ACCEPTANCE checked hash and threading against a real HyperKitty; Phase 5 is complete per the ledger. UI follows the same Phase 4 architecture (askama + htmx).

| ID                             | Effort | Scope                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | Own acceptance                                                                                                                                                                                                                                        |
|--------------------------------|--------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| ~~P5-RENDER~~ (done)           | M      | sender/date/parent indexed; thread tree (`threading::order`, 500 posts); attachments stored in `archive_attachments`, served on a separate path + nosniff + attachment disposition + type deny-list; text (quote folding, linkify) and a safe markdown subset (`pulldown-cmark` events → own writer); obfuscation for guests; owner reattach (audited); gravatar opt-in proxied + cached                                                                                                                         | PASS: `crates/archive/tests/render_threading.rs` (mbox snapshot per the HyperKitty algorithm, not a real HyperKitty import) + `crates/api/tests/webui/archive_render.rs` + PostgreSQL contract                                                        |
| ~~P5-SEARCH~~ (done)           | M      | tantivy 0.26 index (list, hash, thread, subject, body, sender, date), thread/date filtering via `search::Query` (no facets on the page yet), commit batching (100 changes/2 s, always on shutdown) in the archive runner, `listmngr archive reindex`; archive pages use the index when present, LIKE fallback                                                                                                                                                                                                    | PASS: `crates/archive/tests/search.rs` (+ 100k bench ignored, figures in the ledger), `crates/api/tests/webui/archive_search.rs` + PostgreSQL contract, `crates/cli/tests/archive_reindex.rs`, runner test                                            |
| ~~P5-UI~~ (done)               | L      | `/archive/overview` (totals, months, 10 newest threads, 10 most active threads and 10 top posters over 30 days), `/archive/threads[/{y}/{m}]` (20/page, `new` badge per the reader's `archive_thread_views`), `/archive/thread/{hash}` (records last-view), `/archive/senders/{sha256(email)}`, search page with result count + `<mark>` (on text nodes only), `/archive/feed.atom` and `/archive/feed.rss`                                                                                                      | PASS: `crates/api/tests/webui/archive_ui.rs` + PostgreSQL contract; axe/CSP via the Chromium harness (overview, threads, thread, sender, search)                                                                                                      |
| ~~P5-INTERACTIONS~~ (done)     | S      | votes ±1 (`archive_votes`, one vote/user/post, retractable), tags (`archive_tags`, normalised, removed by the tagger or the owner, page `/archive/tags/{tag}`), categories (`archive_categories` + `archive_thread_categories`, set by the owner, page `/archive/categories/{name}`), favorites (`archive_favorites`, page `/archive/favorites`); last-view already present since P5-UI; vote/tag/category audit in the same transaction                                                                         | PASS: `crates/api/tests/webui/archive_interactions.rs` + PostgreSQL contract; Chromium harness (vote, tag, favourite)                                                                                                                                 |
| ~~P5-WEB-POST~~ (done)         | M      | `GET/POST /archive/post[?reply=hash]`: a member with a verified address composes a new thread/reply; the server composes (`listmngr_mail::web_post::compose`, mail-builder) and injects into the `in` queue with context `web_post{user_id,address,reply}`; the `in` runner's `approve_web_post` = approved-like when the sender matches the address, is not banned, and the member is not moderated (defer/accept)                                                                                              | PASS: `crates/api/tests/webui/archive_post.rs` + PostgreSQL contract; `crates/runners/src/web_post_tests.rs`; e2e `web_post_is_delivered_and_archived` (real binary: web login → post → SMTP sink receives → archive page shows it); Chromium harness |
| ~~P5-MBOX~~ (done)             | M      | `listmngr archive import` (mboxrd, `.gz`, batch/transaction, skip by hash, generates `Message-ID` when missing; 100k msg < 10 minutes on a dev machine) + mbox export (whole store/thread/month, gzip) in the CLI and streaming `…/archive/export.mbox[.gz]`; `archive_policy=never` blocks both import and export; private archive auth + membership check; export goes through `publication` so anonymous lists still hide authors                                                                             | PASS: `crates/archive/tests/mbox.rs` + `crates/cli/tests/archive_import.rs` (CLI import/export + 100k benchmark `#[ignore]`, figures in the ledger) + `crates/api/tests/webui/archive_export.rs` + PostgreSQL contract                                |
| ~~P5-ADMIN~~ (done)            | S      | owner hides/unhides and permanently deletes a post or a whole thread (migration `0051` adds `archive_messages.hidden_at`; every read path filters `hidden_at IS NULL`; deleting a post links replies to the parent and re-roots the thread when the root is lost, carrying tag/category/favourite/last-view along); reattach already present since P5-RENDER; category CRUD on `…/archive/admin` (paying off the debt from P4-LIST-SETTINGS and P5-INTERACTIONS)                                                 | PASS: `crates/api/tests/webui/archive_admin.rs` + PostgreSQL contract; Chromium harness (add/delete category, hide then unhide a post)                                                                                                                |
| ~~P5-REMOTE-ARCHIVERS~~ (done) | S      | `[archive] archivers` (`mail_archive_address`, `mhonarc_command` as argv, `prototype_path`) + the `list_archivers` toggle is what enables an archiver; `mail-archive` enqueues a copy into `Queue::Out` within the `ArchiveRepo::complete` transaction (only `public` lists), `mhonarc` pipes via `tokio::process::Command` with `$listname`/`$hostname`/`$hash`, `prototype` writes a maildir `<root>/<list>/new/<hash>` (writes `tmp/` then renames); hidden posts or `archive_policy=never` are not forwarded | PASS: `crates/runners/src/remote_archivers_tests.rs` + PostgreSQL contract                                                                                                                                                                            |

| ~~P5-ACCEPTANCE~~ (done) | S   | gathers the four Phase 5 acceptance criteria: 100k import benchmark and search p95 (figures in the ledger), `crates/archive/tests/hyperkitty_parity.rs` checks hash and threading against a real HyperKitty (`mailman-users@mailman3.org`, 2025-03, 138 messages, fixture keeps only headers/hash/relations); fixes the divergence found — a reply to an absent parent becomes its own root like HyperKitty (`resolve_thread` + `adopt_orphans` shared by runner and importer) | PASS: `hyperkitty_parity.rs` (138/138 hash; 133/138 parent, 132/138 thread — the remainder are February roots/parents outside the fixture) + `archive.rs::verify_orphans_adopted_by_a_late_parent` + PostgreSQL contract |

- Acceptance: import a 100k msg mbox (public Mailman list) < 10 minutes on a dev machine, search p95 < 100ms, URL hash matches HyperKitty for the same Message-ID, threading snapshot compared with a HyperKitty import of the same mbox — **passed** via P5-ACCEPTANCE (ledger); hash and threading checked against a real HyperKitty via the REST API, not a local HyperKitty import.

### Phase 6 — Advanced & migration (M)

- Full DMARC — reject/discard, unconditional, `dmarc_addresses` and org domain (`mail-auth`'s RFC 9989 tree walk, replacing the PSL) closed in P2-DMARC-MUNGE / P2-VALIDATE-AUTHENTICITY; remaining:
  - [x] ~~**P6-DMARC-WRAP**~~ (done) (S): `wrap_message` + `dmarc_wrapped_message_text` — `listmngr_mail::munge::wrap` per `mailman/handlers/dmarc.py` (KEEPERS, `message/rfc822` inline, text part when present), migration 0052 widens the CHECK, REST/OpenAPI/settings page accept the value.
  - [x] ~~**P6-ARC-SEAL**~~ (done) (M): `[mta.arc]` — `listmngr_mail::arc::Sealer` (mail-auth `arc` feature) seals every delivery after the list's DKIM signature with `ARC-Seal`/`ARC-Message-Signature`/`ARC-Authentication-Results` carrying the intake results; `Verifier::verifying_arc` checks the inbound chain in the `in` runner, `cleanse-dkim` keeps `ARC-*` when the site seals; the key's DNS record is shown on the domain page.
- NNTP gateway (all three IDs closed), split into:
  - [x] ~~**P6-NNTP-SETTINGS**~~ (done) (S): Usenet settings on the list (`gateway_to_mail`, `gateway_to_news`, `linked_newsgroup`, `nntp_prefix_subject_too`, `newsgroup_moderation`, `usenet_watermark` read-only; migration 0053), REST on both prefixes, `news-moderation` rule in the chain.
  - [x] ~~**P6-NNTP-GATEWAY**~~ (done) (M): handler `to-usenet` → queue `nntp`; `nntp` runner: `prepare_message` (Approved:, subject prefix, Newsgroups:, Message-ID, Lines:, remove/rewrite headers) and POST via the NNTP client (`[nntp] host/port/user/password`); duplicate Message-ID (441) → munge once and requeue; server down → backoff.
  - [x] ~~**P6-NNTP-GATENEWS**~~ (done) (M): `listmngr nntp gate` + a periodic cycle in the `nntp` runner (`[nntp] gatenews_every_secs`): GROUP/ARTICLE via `Reader`, `nntp::inbound` (`X-Originally-To`, `To` = list, drops articles carrying the list's `List-Id`), injects into `in` with `fromusenet`, per-article watermark with audit; the first poll only catches up.
- Import from Mailman, split into:
  - [x] ~~**P6-IMPORT21**~~ (done) (M): crate `listmngr-import` — `pickle::read` (protocol 0–2, `OBJ`/`BUILD` instances), `Config21`, `plan()` per `mailman/utilities/importer.py` (settings/topics/aliases/bans/header_filter_rules/templates/rosters), `apply()` audited and re-runnable, `listmngr import21 <list_id> <config.pck> [--dry-run]`; mapping table in `docs/MIGRATION.md`.
  - [x] ~~**P6-IMPORT3-REST**~~ (done) (M): `listmngr import3 --rest` — `rest3::Rest` (Basic auth, GET only, password from file), `import3::fetch` (domains, site bans, lists + config, 4 rosters + preferences, bans, header-matches, uris; pagination via `count`/`page`), `plan()` (table of 75 settings, `7d` → days, aliases per line, the rest become warnings), `apply()` audited and re-runnable.
  - [x] ~~**P6-IMPORT3-USERS**~~ (done) (M): user + address + preferred address + preferences via `UserRepo::create_imported_with_context` (credential `usable=0`, adopts bare addresses, keeps verification, membership `as_user`); Mailman passwords (passlib) are **not** imported — the account owner resets via recovery.
  - [x] ~~**P6-IMPORT3-HELD**~~ (done) (M): Mailman 3 held messages + pending requests → `ModerationRepo::hold_imported_with_context` (keeps bytes + `hold_date` intact, audit `moderation.import`, sends no notice) and `WorkflowRepo::import_request_with_context` (`pending_moderation`, audit `subscription.import`); a subscriber confirmation still outstanding is warned about, not imported.
  - [x] ~~**P6-IMPORT3-DB**~~ (done) (L): `import3 --db` reads the Mailman 3 DB directly (SQLite/PostgreSQL via sqlx Any) + `var/messages` (pickle `email.message.Message` → `render_message`) into the same `Site` as REST; int enums, intervals, pickled `MutableList`, `pended`/`pendedkeyvalue`, `_request`; REST ↔ DB cross-checked on the same core.
  - [x] ~~**P6-IMPORT3-HYPERKITTY**~~ (done) (L): `import3 --hyperkitty` reads the HyperKitty DB (SQLite/PostgreSQL) → votes/tags/category/favorites placed on the posts the mbox import brought over (same Message-ID-Hash, thread = hash of the first post) and on already-imported accounts (matched by `auth_user.email`); `ArchiveRepo::import_interactions` one transaction + audit `archive.import_interactions`, missing post/thread/account → `skipped`, re-running does not duplicate, sends no mail.
- [x] ~~**P6-PO-TEMPLATES**~~ (done) (M): `tests/compat/import_mailman_templates.py` generates the catalog from Mailman 3.3.10 — in practice 30 languages have usable translations (29 with subject, 21 with body) (not 40+: that figure is Mailman 2.1's; Mailman 3 has 33 directories, mostly empty files): subject from `.po` when the msgid (placeholders renamed) exactly matches the Fluent English, body for the 17 templates whose English here is still Mailman's; rewritten templates (welcome, confirmation, bounce, rejected…) keep listmngr's English/Vietnamese. `SUPPORTED` (UI) remains en/vi; `notice_languages` for notices, list/member picker, `import3` (`pt_BR` → `pt-BR`).
- Webhooks + plugins, split into (design approved 2026-09-25; event = `audit_log.action`, outbox fan-out in the same transaction as the write, HKDF secret from `[webhooks] signing_key_file`, web UI deferred):
  - [x] ~~**P6-WEBHOOKS-STORE**~~ (done) (M): migration `webhooks` (`id`, `url`, `events`, `list_id nullable`, `enabled`, `description`, `secret_hash`, `secret_salt`) + `webhook_deliveries` (`id`, `webhook_id`, `event`, `payload`, `list_id`, `attempts`, `next_attempt_at`, `state`, `last_status`, `last_error`); `WebhookRepo` (create/get/list/update/delete/rotate/deliveries; secret = HKDF(signing_key, id‖salt), shown once); fan-out in `record_tx_with_context`: an `enabled` hook matching the event (`*`, prefix `moderation.*`) and scope (site or the target's `list_id`) → one delivery per hook, same transaction; payload `{id, event, at, list_id?, target, actor, data: redacted diff}`; `[webhooks] enabled/signing_key_file/allow_http/allow_private_targets/max_attempts`; sweep deletes `delivered` older than `finished_job_retention`. No HTTP yet.
  - [x] ~~**P6-WEBHOOKS-DELIVER**~~ (done) (M): `webhook` runner in `listmngr serve` when `[webhooks] enabled` (independent of the mail role): claims a lease on due deliveries, POSTs JSON with `X-Listmngr-Event/-Delivery/-Timestamp/-Signature: sha256=HMAC(secret, ts.body)`; 2xx → `delivered`, otherwise → retry per `delivery_policy::Backoff` up to `max_attempts` → `failed` keeping `last_status/last_error`; reqwest rustls, no redirect following, 10s timeout, `https://` only unless `allow_http`, resolves first and refuses loopback/link-local/private/ULA unless `allow_private_targets`, pins the resolved IP (against DNS rebinding); `ping`; metrics. Tested with a loopback HTTP server: 2xx, 500→retry, redirect refused, private IP refused, signature verifiable.
  - [x] ~~**P6-WEBHOOKS-API**~~ (done) (M): `/api/v1/webhooks` (`GET/POST`, `GET/PATCH/DELETE /{id}`, `POST /{id}/rotate`, `POST /{id}/ping`, `GET /{id}/deliveries`), new scope `webhooks` (server owner sees all, a list-scope token only its own list's hooks), OpenAPI, CLI `listmngr webhooks ls|add|rm|rotate|ping|deliveries`, audit `webhook.*`, docs.
  - [x] ~~**P6-WEBHOOKS-WEB**~~ (done) (S): list settings + admin pages for webhooks and deliveries.
  - [x] ~~**P6-PLUGINS**~~ (done) (M): trait `Plugin` (`name`, `version`, `rules()`, `handlers()`, `archivers()`) compile-time in `crates/plugins`; `Registry::builtin()` registers plugin rules/handlers after the built-ins, a list's chain/pipeline may name them; `archivers::run` calls plugin archivers; `GET /plugins` + `listmngr plugins` (no plugins → `[]`); sample plugin `keyword-hold` behind the cargo feature `plugin-example` only to test the wiring.
- The rest of Phase 6, split into (plan of 2026-09-25; done in this order):
  - Exim snippets: **closed since Phase 2** — `[mta] incoming = "exim"` generates `lsearch` maps, `deploy/exim/listmngr.conf` is the router/transport, `scripts/check-mta-configs.sh` runs the `exim -bt` matrix in a container; no new ID opened.
  - [x] ~~**P6-ARCHIVER-HYPERKITTY**~~ (done) (M): `hyperkitty` archiver alongside `mail-archive`/`mhonarc`/`prototype` — `[archive] archivers.hyperkitty_url` + `hyperkitty_api_key`/`_file`; after storing a post, the runner POSTs the published copy to `<url>/api/mailman/archive` like `mailman-hyperkitty` 1.2: `Authorization: Token <key>`, multipart `mlist=<fqdn>` + file `message`; 2xx → logs the returned URL, error → warn, does not block the post; enabled/disabled per list via `list_archivers` (REST + settings page). Tested with a loopback HTTP target checking headers/fields/bytes.
  - [x] ~~**P6-INBOUND-SMTP**~~ (done) (M, experimental): `[mta] inbound_smtp_listen` (off by default) — a second SMTP listener using the same `InboundHandler` as LMTP: accepts `EHLO/HELO`, one reply for `DATA`, RCPT only to list addresses (no relay), size/RCPT-count limits as LMTP, no AUTH/STARTTLS (put it behind the MTA or loopback only); marked "experimental" explicitly in the docs.
  - [x] ~~**P6-WEB-TLS**~~ (done) (M): `[web] tls.cert_file`/`tls.key_file` — `serve` accepts TLS with the rustls already in the lock (hyper-util accept loop, `ConnectInfo` unchanged), HTTP/1.1 + h2 via ALPN; files validated at config load; tested with a self-signed cert generated by `openssl` like the STARTTLS test.
  - [x] ~~**P6-WEB-ACME**~~ (done) (M): `[web.tls] acme_domains`/`acme_contact`/`acme_directory_url`/`acme_cache_dir`/`acme_ca_file` (flat beside `cert_file`/`key_file`, mutually exclusive) — automatic certificates via ACME TLS-ALPN-01 on the TLS listener itself using `rustls-acme` 0.15.4 (without the `tokio` feature), cache `0700`/`0600`, deploys the cached copy at startup, background renewal at 2/3 of the lifetime; acceptance runs for real with Pebble v2.10.1 + `pebble-challtestsrv` as DNS on loopback (`#[ignore]` test `web_acme`, CI job `acme` with pinned digest).
  - [x] ~~**P6-MESSAGE-STORE**~~ (done) (L): `[message_store] backend` made real: `db` (default, as today), `fs` (`path/<aa>/<bb>/<store_key>`, files 0600), `s3` (`s3_bucket`/`s3_region`/`s3_endpoint`/`s3_prefix`/`s3_access_key_id`/`s3_secret_access_key[_file]`, hand-written SigV4 on reqwest: PUT/GET/HEAD/DELETE/ListObjectsV2) — `listmngr_db::blobs::BlobStore` for every read/write point of `message_blobs.raw` (queue, held, digest, notices, import, usenet), content-addressed SHA-256, object written before the row; orphan sweep after 1h in the task runner (`collected_blobs`); `listmngr message-store migrate|check`; loopback fake-S3 test verifies signatures + 4 standard AWS SigV4 vectors + PostgreSQL contract.
  - [x] ~~**P6-BACKUP**~~ (done) (L): `listmngr backup <dir>` (manifest: format, version, migration ledger + checksum, DB and message store backends, columns/row count per table; one `.jsonl` per table via dynamic `sqlx::Any` decoding by value type, blobs base64; `fs`/`s3` bytes are read through the store and live in the `.jsonl` instead of copying objects separately — backend-independent backup) and `listmngr restore <dir>` into an empty DB migrated to the same ledger, any backend, FKs deferred to commit (SQLite `defer_foreign_keys`; PG `ALTER CONSTRAINT DEFERRABLE` inside the transaction then reverted) because `users` ↔ `addresses` form a cycle; acceptance: test site SQLite → PostgreSQL (own schema) → SQLite, comparing row counts of every table + spot-check list/member/message/held/audit.
  - [x] ~~**P6-ACCEPTANCE**~~ (done) (M): `crates/cli/tests/phase6_acceptance.rs` runs the real binary with the in-repo fixture: `import3 --db` (`mailman.db` + `var/`) → `archive import` mbox by HyperKitty hash → `import3 --hyperkitty` → `import21 config.pck` onto the same list → `serve`; HyperKitty permalinks `/archives/list/<address>/message/<hash>/` (and `/hyperkitty/…`, thread, month, index, `/postorius/lists/<id>/`) redirect 308 to the corresponding page (`crates/api/src/compat.rs`, PLAN §9 "URL compat"), target page 200 with the right subject; PostgreSQL contract with the same scenario; live evidence (SQLite + PG core, HyperKitty PG) in the `P6-IMPORT3-*` rows.
  - MySQL: **not for 1.0** — sqlx `Any` enables only SQLite + PostgreSQL, migration/snapshot tests run on just the two backends; recorded in §8.
- [x] ~~**P6-DOCTOR**~~ (done) (S): `listmngr doctor` — JSON v1 `ok`/`checks` (`ok`/`fail`/`skip`), exit 12 `CLI-DOCTOR`; migration ledger checked against the embedded migrator itself (SQLite read-only, PG `READ ONLY`), the relay's SMTP greeting when the mail role is on (sends no bytes), MX/implicit-MX of each mail domain (`--dns-server`); no migrate, no mail sent, no MTA reload; TLS/AUTH/delivery/SPF-DKIM-DMARC-ARC-PTR are explicit `skip`.
- [x] ~~`backup/restore` + independent restore acceptance.~~ (P6-BACKUP)
- [x] ~~**P6-NNTP-GATENEWS-FIX**~~ (done) (S): two bugs found when reproducing with the real CLI while closing P6-DOCTOR — (1) a disconnected/truncated ARTICLE still advanced the watermark without queueing → lost article; (2) two concurrent pollers enqueued the same article twice. Fix: `Reader::article` distinguishes a missing article (`None`, skip) from a broken session (`Err`, stop, keep the watermark); `UsenetRepo::advance_watermark` CAS on the watermark + enqueue + audit in one transaction, a poller that loses the CAS stops; an unreadable header block is skipped instead of getting stuck.
- [x] ~~Acceptance: migrate 1 real Mailman 2.1 site + 1 real Mailman 3 site → lists/members/settings/archive URLs unchanged.~~ (P6-ACCEPTANCE on a fixture generated from a real core/HyperKitty + live evidence of `P6-IMPORT3-REST`/`-DB`/`-HYPERKITTY`/`P6-IMPORT21`)

### Phase 7 — Hardening & 1.0 (M)

Original goals (kept for reference):

- Security review per the §5 checklist + external review if available; fuzz (LMTP parser, bounce detectors, VERP, template placeholders, mime-delete) `cargo-fuzz` 24h/target
- Load test: 10k members list, 1k posts/h; measure queue latency; tune `max_recipients`, pool
- Chaos: kill runner mid-job, DB restart, MTA down → no lost mail, no double-deliver (idempotency key per job)
- Docs site (mdBook): install, migrate, admin guide, API, ops runbook
- Release: cross-compile (linux amd64/arm64, macOS), deb/rpm, Docker multi-arch, Helm chart, SBOM + cosign, upgrade path & DB migration policy (backward-compatible N-1)
- 1.0 tag

Split into work packages (plan of 2026-10-01, one ID one branch, done in this order; each ID has a ledger row with real commands and figures, and states explicitly the "operational target" part that cannot run in full within one session):

- [x] ~~**P7-FUZZ**~~ (done) (M): `fuzz/` directory (cargo-fuzz, nightly pinned in `fuzz/rust-toolchain.toml`) with targets on pure, I/O-free APIs: `lmtp_session` (LMTP/SMTP session over `tokio::io::duplex`, fake handler), `bounce_detect` (bounce detectors on message bytes), `verp_decode`, `template_render` (placeholders + catalog), `mime_filter` (content filtering / mime-delete), `pickle_read` (the `import21` pickle reader), `mbox_read`; corpus seeded from in-repo fixtures; `scripts/fuzz.sh <seconds/target>`; CI job `fuzz` runs each target with `-max_total_time=60`; locally each target runs ≥ 10 minutes, recording exec/s and crashes; any crash found → fix + regression test in the same ID. Deviation recorded up front: 24h/target is the operational schedule (`scripts/fuzz.sh 86400`), not run within the session.
- [x] ~~**P7-SECURITY-REVIEW**~~ (done) (M): review every item of §5.1–§5.4 → `docs/SECURITY_REVIEW.md`: table of item / status / evidence (test, ledger ID) / vulnerabilities found; every finding fixed in the same ID with a test; additionally reviewed: SSRF (webhooks, HyperKitty archiver, ACME directory), path traversal and symlinks in the `fs` store/backup dir, dynamic SQL (backup/blobs `IN (...)`, table names from the catalog), size limits (LMTP, mbox import, backup line), timing (token/secret comparison), web security headers (CSP/HSTS/`Referrer-Policy`), open redirects (compat/login `next`), rate limit, logs not leaking secrets. External review: recorded as not yet done.
- [x] ~~**P7-CHAOS**~~ (done) (M): `crates/cli/tests/chaos.rs` on the real binary + SMTP sink: (1) kill the runner mid-job (after claim, before ack) → lease expires, a new runner re-claims, the sink receives exactly one copy per recipient (idempotency per job/recipient); (2) DB restart midway (SQLite: file locked/unlocked; PG contract: `pg_terminate_backend` or pool restart) → no job lost, none duplicated; (3) MTA down → retry/backoff, delivered when the MTA comes up; cross-checked against the existing `queue_recovery.rs` and only the missing parts added.
- [x] ~~**P7-LOAD**~~ (done) (M): `crates/cli/tests/load.rs` (`#[ignore]` benchmark): 10k-member list, posts pumped via LMTP → SMTP sink; measures throughput and queue latency p50/p95 (`created_at` → sink receipt); runs locally bounded (10k members, 100 posts) and records real figures; `docs/OPERATIONS.md` tuning section (`max_recipients`, `database.max_connections`, runner concurrency, `finished_job_retention`); the 1k posts/hour goal is recorded as a target, with whether it was measurable stated explicitly.
- [x] ~~**P7-DOCS-SITE**~~ (done) (M): mdBook at `docs/book/` (`book.toml`, `SUMMARY.md`; chapters: Introduction, Installation (binary/Docker/systemd/Helm), Configuration (every config section), Migrating from Mailman (from `MIGRATION.md`), Operations (backup/restore, doctor, tasks, message store, TLS/ACME), Administration (web UI by role), API (`/api/v1`, `/3.1`, webhooks), Security (from `SECURITY.md`/review), Architecture (summary), Changelog); `mdbook` pinned, `mdbook build` + link check in CI (`docs` job); content taken from the existing README/ARCHITECTURE/MIGRATION, README keeps its ledger-prose role.
- [x] ~~**P7-RELEASE**~~ (done) (L): `[profile.release]` (lto = "thin", codegen-units = 1, strip, debug = "line-tables-only"?) measuring size/time; `.github/workflows/release.yml` on tag `v*`: build `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (cross/zigbuild), `aarch64-apple-darwin`, `x86_64-apple-darwin`; `cargo-deb` + `cargo-generate-rpm` (systemd unit, user `listmngr`, `/etc/listmngr`, `/var/lib/listmngr`); Docker multi-arch buildx from `deploy/Dockerfile` (tag + digest); Helm chart `deploy/helm/listmngr` (Deployment, Service, Secret/ConfigMap, PVC, probes `/healthz` `/readyz`, PodSecurity read-only/non-root) + `helm lint` + `helm template` test; SBOM CycloneDX (`cargo cyclonedx`) + `cosign sign-blob` keyless + `SHA256SUMS`; `docs/UPGRADE.md`: N-1 migration policy (schema additive, no deletions/semantic changes within 1 minor; `migrate` before moving to the new binary; rollback = `backup` first, `restore` into the old version), `doctor` checks before/after. Local verification bounded: `cargo build --release` on the host, `cargo zigbuild --target x86_64-unknown-linux-musl` if possible, `helm lint`, `docker build` if the daemon is running; the parts needing a GitHub runner (image push, cosign keyless OIDC) recorded as running in CI.
- [x] ~~**P7-1.0**~~ (done) (S): bump `1.0.0` across the workspace (`Cargo.toml`, lock), `CHANGELOG.md` `## [1.0.0] - <date>` gathers Unreleased, README/`CLAUDE.md`/§10 update the "0.1.0 = unreleased" sentence, the phase table in the ledger closes Phase 7, annotated tag `v1.0.0` created locally (push is the user's decision).

### Phase 8 — 1.1: closing the items "open for 1.x" (M)

Goal: close the four technical items in `docs/SECURITY_REVIEW.md` "What remains
open for 1.x" (encrypting the TOTP secret at rest, DKIM Ed25519 + selector
rotation, posting-rate and hop-count limit rules, MIME structure ceilings at
the inbound gate) and clean up the ledger/doc passages that still contradict
the 1.0.0 status. External review, MySQL/MariaDB, WASM plugin, the SDK
generated from OpenAPI and the ZAP baseline are backlog (end of this section),
not scheduled.

Split into work packages (plan of 2026-10-05, one ID one branch, done in this
order; each ID has a ledger row with the real commands and figures; the §7.0
conventions stay unchanged; the version changes only in `P8-1.1`):

- [x] ~~**P8-LEDGER-RECONCILE**~~ (done) (S): documentation only. `docs/FEATURE_PARITY.md`:
  the phase table (rows 0–4 still say "open: `P2-E2E-ACCEPTANCE`, `P2-HELD-FORWARD`",
  "Partial / interim rendering", "PostgreSQL acceptance predates 0004") and the
  "Residual P1s" table (rows "Full Phase 2 …", "Later features", "Recovery and
  operations") corrected to match the closed rows, keeping every sentence that
  is still true (hosted CI has no run yet because the repo has no remote; the
  push is the user's decision); the passage "opt-in plaintext trusted-relay …
  not production-ready" replaced with a 1.0.0 status sentence; add row 8
  "In progress". `docs/PLAN.md` section "Implementation status" gains the
  2026-10-04 milestone (Phase 7 closed, 1.0.0). `docs/HANDOFF.md` (the
  2026-09-19 snapshot says P5-MBOX is not committed) and
  `docs/MAILMAN_REPLACEMENT.md` ("the complete open scope") get a
  "historical — superseded" banner at the top of the file, not deleted. Gate:
  all of CLAUDE.md + `python3 -m unittest discover -s scripts/tests` +
  `mdbook build docs/book` + `scripts/check-book-links.py`.
- [x] ~~**P8-MIME-LIMITS**~~ (done) (M): `[mta] max_header_count = 500`,
  `max_mime_parts = 1000`, `max_mime_depth = 20` (integers ≥ 1, checked at
  load). `listmngr_mail::structure::{Limits, Measure, measure, check}`: header =
  the number of fields in the outermost header block (lines not starting with
  WSP before the first blank line; folded lines do not count); part = the total
  number of MIME parts including nested `message/rfc822`; depth = the deepest
  nesting level (outermost body = 1). Applied at the inbound gate before
  anything is written: `InboundHandler::deliver` (shared by LMTP and inbound
  SMTP) returns `554 5.6.0 message structure exceeds the site's limits` for
  every recipient, no enqueue; `nntp` gatenews skips posts over the ceiling
  (log, the watermark still advances, no enqueue) like posts with an unreadable
  header. Fuzz: `measure` runs in the `mime_filter` target. Test: unit at the
  exact boundaries (20/21, 1000/1001, 500/501, nested `message/rfc822` counts
  toward depth); runner on a real LMTP session (21 levels → 554 for both
  recipients, `queue_jobs` empty; 20 levels → 250); gatenews with a fake
  server; validate config. Docs: README, ARCHITECTURE, `SECURITY_REVIEW.md` row
  "Size and DoS" → done, `configuration-reference.md` regenerated, CHANGELOG.
- [x] ~~**P8-ABUSE-RULES**~~ (done) (M): two new rules in `default-posting-chain`.
  `max-hops`: `[mta] max_received_hops = 30` (0 = off, ≤ 1000) counts the
  post's `Received:` headers; exceeded → link `jump discard` right after
  `loop`, reason "Too many Received: headers (N > max)". `posting-rate`:
  `[security] rate_limit.post` (`COUNT/WINDOW` like the other keys in the same
  group, unset by default = off) per (list, sender address) pair counts the
  posts accepted into the pipeline within the window; exceeded → link `defer`
  at the end of the moderation group (after `suspicious-header`, before `any`)
  → hold with reason "Posting rate exceeded:
  N posts in the last WINDOW". Storage: migration `0055` table
  `posting_rate(list_id, email, posted_at)` + index, written in the very
  acceptance transaction of the `in` runner (`processor.rs`,
  `Disposition::Accept` branch); new sweep step `posting_rate` deletes rows
  older than 1 day (the longest window). Facts: `gather_context` counts the
  rows in the window → `ctx.sender.recent_posts`, `ctx.site_posting_rate`,
  `ctx.site_max_received_hops`. Update `pipeline/tests/chain.rs`, `rest.rs`
  (`/system/chains`), the schema snapshot (`schema_contract.rs` +
  `repositories.rs`), `scripts/test-postgres.sh`. Test: rules at the exact
  boundaries and off by default; runner: `rate_limit.post = "3/hour"` → the
  fourth post from the same sender is held with the right reason, other senders
  unaffected, rows older than the window do not count; `max-hops`: 31
  `Received:` → discard, 30 → accept. Docs: README, ARCHITECTURE,
  `SECURITY_REVIEW.md` row "Loop and abuse" → done, config reference,
  CHANGELOG.
- [x] ~~**P8-DKIM-ED25519**~~ (done) (M): the key file decides the algorithm — PKCS#8 with OID
  `1.2.840.113549.1.1.1` → RSA (`rsa-sha256`, as before), OID `1.3.101.112` →
  Ed25519 (`ed25519-sha256`, RFC 8463), PKCS#1 → RSA; any other kind → error at
  load. Several `[[mta.dkim_signing]]` with the same `domain` and different
  `selector` are allowed, and each entry signs one `DKIM-Signature` in
  configuration order (dual signing).
  `SigningKeys` → `BTreeMap<domain, Vec<DkimSigner<DkimKey, Done>>>`;
  `dns_record` returns `k=ed25519; p=<base64 32 byte>`; the domain page lists
  every selector. New CLI `listmngr dkim gen --domain D --selector S --algorithm
  rsa|ed25519 [--bits 2048] --out FILE` (PKCS#8 PEM, mode 0600, no overwrite,
  prints the TXT record, does not print the key), `listmngr dkim records` (a
  record for every entry in the config), `listmngr dkim dns` (looks up the TXT
  via the `doctor` resolver and compares it with the expected `p=`; exit 12 on
  mismatch). Selector rotation runbook in `docs/OPERATIONS.md`: gen a new
  selector → add the entry → `dkim dns` confirms → drop the old entry after the
  longest message lifetime. Test: sign with Ed25519 → verify `dkim=pass` with
  the `mail_auth` Verifier on a seeded `TxtCache` (as in `mailpath_e2e.rs`);
  dual → both signatures pass; EC P-256 key → load error; CLI on the real binary
  (both algorithms, mode, record format, refuses to overwrite, no key on
  stdout). Docs: README, ARCHITECTURE, OPERATIONS, `SECURITY_REVIEW.md` row
  "Outbound DKIM" → done, CHANGELOG.
- [x] ~~**P8-MASTER-KEY**~~ (done) (L): `[security] master_key_file` (optional; 64 hex
  characters = 32 bytes, read via `read_secret_file`). `listmngr_db::keyring`:
  `MasterKey` (`Zeroizing<[u8; 32]>`), HKDF-SHA256 (`ring`) with info
  `listmngr/totp/v1`, AEAD `CHACHA20_POLY1305` with a random 12-byte nonce, AAD =
  `user_id`; `user_totp.secret` keeps the TEXT type, ciphertext `v1:<base64(nonce ||
  ct)>`, old base32 rows remain readable (the upgrade needs no migration);
  `Database::with_master_key`. TOTP enrol/confirm/verify go through the keyring;
  the decrypted copy lives in `Zeroizing<Vec<u8>>`. CLI `listmngr secrets status`
  (counts plain/encrypted rows), `listmngr secrets encrypt` (encrypts every plain
  row in one transaction, one audit `security.encrypt_secrets`), `listmngr secrets
  rewrap --previous-key-file FILE` (key rotation). `doctor`: check `master_key` —
  `warn` when no key is set or plain rows remain under a key; `fail` when
  encrypted rows exist without a key. Backup/restore copy the ciphertext as is;
  restoring to a site with a different key → `doctor` fail; documented in
  UPGRADE/OPERATIONS. `SmtpAuthSecret` zeroizes on drop. Out of scope, recorded
  as a deviation: wrapping the DKIM/ARC keys (owner-only files), the webhook
  signing key and the S3 secret (read from files). Test: keyring (round trip,
  wrong AAD/key/tampered ciphertext → error); db (enrol+confirm+verify under a
  key → `v1:` row; old rows still verify; `encrypt` converts N rows + audit;
  `rewrap`); PG `#[ignore]` for `encrypt`; CLI and doctor on the real binary.
  Docs: README, ARCHITECTURE, OPERATIONS, UPGRADE, `SECURITY_REVIEW.md` row
  "Secrets" → done/deviates, config reference, CHANGELOG.
- [x] ~~**P8-1.1**~~ (done) (S): bump `1.1.0` like `P7-1.0` (workspace, path deps,
  `Chart.yaml`, fixture `system-versions.json`, `fuzz/Cargo.lock`),
  `CHANGELOG.md` `## [1.1.0] - <date>`, `SECURITY_REVIEW.md` "What remains open"
  rewritten (external review remains), ledger phase table closes Phase 8,
  annotated tag `v1.1.0` locally (the push is the user's decision).

Backlog after 1.1 (not scheduled): external security review; MySQL/MariaDB
(§1.3); WASM plugin; TS/Python SDK generated from `/openapi.json` (§4.15); ZAP
baseline in CI (§8); CSP nonce; `__Host-` cookie (needs `Path=/`).

### Phase 9 — Maintenance after 1.1 (S)

Fixes for bugs found after `1.1.0` was tagged; the version does not change until
there is a release (`docs/UPGRADE.md`). The §7.0 conventions stay unchanged: one
ID one branch, a ledger row with the same ID records the real commands and
results.

- [x] ~~**P9-CLI-USER-VERIFIED**~~ (done) (S): `listmngr user create` left the address
  unverified, so the first account per "First run" (book, Install) was
  rejected by `/web/login` (401) and not counted by the "last server owner"
  guard. Fix: `UserRepo::create_verified_with_context` — the address is
  verified in the same transaction that creates the user (an existing
  subscriber address is adopted and verified), audit `user.create` records
  `verified`; only the CLI calls it, REST `POST /users` stays as in Mailman.
  Test: db (SQLite + PG contract, login works immediately, REST path still
  unverified), CLI on the real binary (`user export` shows `verified_on` and
  the audit). Docs: README, ARCHITECTURE, book Install (second step:
  `require_2fa_for` default → enrol TOTP at `/web/account/totp` before
  `/web/admin` opens), book Admin, CHANGELOG.

### Phase 10 — Deployment with Helm (M)

Current state (checked 2026-10-07): the chart `deploy/helm/listmngr` has existed
since `P7-RELEASE` (single-replica Deployment, Service, ConfigMap, two Secrets,
PVC, Ingress, NOTES; hardening complete) but has only been through `helm lint` +
`helm template` (CI job `chart`, `scripts/tests/test_release.py`), **never
`helm install`ed onto a real cluster**; no PostgreSQL in the chart (the default
value of `secrets.LISTMNGR__DATABASE__URL` points to a `postgresql` host that
does not exist); no Postfix front MTA as in Compose; no `helm test`,
`values.schema.json`, ServiceAccount, NetworkPolicy; the chart is only packaged
as a `.tgz` on the GitHub release, not yet published to OCI; `deploy/README.md`
has no Helm section. Phase 10 goal: one `helm install` command yields a working
site (web + DB + mail), with evidence on local kind and in CI, installable from
a registry.

Phase-specific principle: no third-party dependency charts (Bitnami changed its
image model in 2025; operators need CRDs) — PostgreSQL and Postfix use exactly
the image/digest Compose already uses, with the same hardening; the Compose
architecture (one listmngr process, Postfix alongside, maps via a shared volume,
LMTP not public) maps 1:1 onto one Pod. Chart and app versions change only at
`P10-1.2` (`docs/UPGRADE.md` policy); until then `Chart.yaml` stays at `1.1.0`
and CHANGELOG entries go under `[Unreleased]`.

Split into work packages (plan of 2026-10-07, one ID per branch, done in this
order — harness first so every later ID has a real kind loop; each ID has a
ledger row with real commands and figures, clearly marking the parts that exist
only in CI/release):

- [x] ~~**P10-HELM-TEST**~~ (done) (M): real-install harness + chart foundation. `scripts/test-helm.sh
  [--mta]`: requires `kind`, `kubectl`, `helm`, `docker`; `kind create cluster
  --name listmngr-helm --image kindest/node:<tag>@sha256:<digest>` (pinned like
  the other images); `docker build -f deploy/Dockerfile -t listmngr:helm-test .`
  then `kind load docker-image`; `helm install t deploy/helm/listmngr --wait
  --timeout 5m` with `image.repository=listmngr`, `image.tag=helm-test`,
  `image.pullPolicy=Never` and (until `P10-HELM-DB`)
  `secrets.LISTMNGR__DATABASE__URL=sqlite:///var/lib/listmngr/listmngr.db`
  on the PVC; `helm test t`; `kubectl exec deploy/t-listmngr -- listmngr --config
  /etc/listmngr/listmngr.toml user create … --server-owner --password-stdin`;
  `kubectl port-forward` + `curl --fail /healthz /readyz` and `/web/login` 200;
  `helm upgrade` changing `config` → `checksum/config` annotation changes → new
  pod ready; `helm uninstall`; `trap` deletes the cluster. Chart: `templates/tests/
  test-ready.yaml` (`test` hook, pod from the already-pinned busybox
  `secretFilesImage`, `wget --spider http://<svc>:<port>/readyz`, same
  securityContext); `values.schema.json` (type/enum/required for every key, `additionalProperties:
  false` at the root) → `helm lint --strict`; dedicated ServiceAccount
  (`serviceAccount.create`, `automountServiceAccountToken: false`);
  `podSecurityContext`/`securityContext` unchanged; `Chart.yaml` gains
  `keywords`, `maintainers` (lint INFO `icon` recorded as deliberately skipped,
  no asset). CI: new job `helm` using `helm/kind-action@1544676c07bb3570fcb70166beaa07c4528fd4aa # v1.15.1`
  (SHA looked up with `git ls-remote --tags` 2026-10-08, `install_only` + kind `v0.33.0`) runs `scripts/test-helm.sh`;
  the old `chart` job is folded in (lint `--strict` + template matrix: default,
  `secretFiles`+`ingress`, `existingSecret`, `persistence.enabled=false`).
  Test: `scripts/tests/test_helm.py` (chart has the test hook, schema, SA; the
  script is called by CI; `helm lint --strict` and the template matrix run
  locally when `helm` is available), `test_acceptance_wiring.py` adds
  `scripts/test-helm.sh`. Docs: `deploy/README.md` gains a "Helm" section
  (harness commands + limits), ACCEPTANCE_NOTES, ARCHITECTURE (deploy section),
  CHANGELOG. Figures recorded: `--wait` time, pod/PVC count, `helm test` output.
- [x] ~~**P10-HELM-DB**~~ (done) (M): PostgreSQL in the chart. `postgresql.enabled = true`
  (default): single-replica StatefulSet `<fullname>-postgresql` from
  `postgres:17-alpine@sha256:18cfe3ef…` (the exact Compose and CI digest),
  headless Service, `volumeClaimTemplates` (`postgresql.persistence.size`
  default `10Gi`, `storageClass`), probes `pg_isready -U <user> -d <db>`
  (readiness 5s/liveness 10s), `runAsUser/runAsGroup 70` (Alpine's `postgres`
  user), `readOnlyRootFilesystem: true` with emptyDir for
  `/var/run/postgresql` and `/tmp`, `drop: [ALL]`, `RuntimeDefault`, its own
  `resources`. Password: `postgresql.auth.password` **required** when enabled
  (`required` in the template → `helm install` fails early, like Compose's
  `POSTGRES_PASSWORD:?`; not generated randomly because `randAlphaNum` changes
  on every `upgrade`) or `postgresql.auth.existingSecret` (key `password`);
  `postgresql.auth.username/database` default to `listmngr`. When enabled, the
  chart itself assembles `LISTMNGR__DATABASE__URL=postgres://<user>:<pw
  urlquery>@<fullname>-postgresql:5432/<db>` into the app's Secret (the
  `secrets.LISTMNGR__DATABASE__URL` value is ignored and `helm lint` warns via
  `fail` if both are set); when disabled, current behavior is kept
  (`secrets`/`existingSecret`, external DB or SQLite on the PVC). The `migrate`
  init container gains a `wait-db` init (busybox `nc -z` to the DB for up to
  120s) so restarts are not counted. `networkPolicy.enabled` (default false):
  PostgreSQL only accepts from the app pod; the app only accepts 8000 from the
  configured namespace/label (`networkPolicy.webFrom`), egress DNS + DB (+ 25/587
  with MTA). The harness `scripts/test-helm.sh` switches its default to PG
  (`--sqlite` keeps the old path); adds a `helm upgrade --set postgresql.auth.password=…`
  step keeping the PVC → data survives (the user created in the previous step
  can still log in). Test: `test_helm.py` (StatefulSet present/absent per
  `postgresql.enabled`, URL assembled correctly and escaped, template fails
  when the password is missing, NetworkPolicy has the right selectors). Docs:
  book Install "Helm" (two paths: PG in the chart / external DB),
  `docs/UPGRADE.md` (the chart does not bump the PostgreSQL major: the `17`
  digest changes only via review + `pg_dump`/restore), `docs/OPERATIONS.md`
  (backup on k8s: `kubectl exec
  … listmngr backup` to the PVC + `pg_dump` from the StatefulSet),
  ACCEPTANCE_NOTES, CHANGELOG.
- [x] ~~**P10-HELM-MTA**~~ (done) (M): Postfix front MTA as in Compose, as a sidecar.
  `mta.enabled = false` by default. When enabled: a `postfix` container in the
  same Pod from `mta.image` (`ghcr.io/p10node/listmngr-postfix`, default tag
  `appVersion`, optional `digest`; built from `deploy/postfix/Dockerfile` — this
  image is additionally built/pushed/cosigned by the `release.yml` job `image`,
  the same way as the main image; local harness `kind load`), mounts volume
  `state` `subPath: mta` at `/var/lib/listmngr/mta` read-only (maps
  `group`-readable, `postfix` already belongs to gid 1000 in the image,
  `fsGroup: 1000`), emptyDir `/var/spool/postfix` (`mta.spool.size`, optional
  PVC), the container's own securityContext: `runAsNonRoot: false`,
  `runAsUser: 0` (master needs root and then drops privileges itself),
  `capabilities.add` exactly the eight caps of Compose,
  `readOnlyRootFilesystem: false` (`postconf -e`, spool) — a **documented
  hardening exception**, pod-level unchanged; env
  `POSTFIX_MYHOSTNAME=<mta.hostname>`, `POSTFIX_MYNETWORKS=127.0.0.0/8` (only
  listmngr in the same pod may relay), `POSTFIX_RELAYHOST=<mta.relayhost>`;
  probe `postfix status`. The listmngr container receives extra env when
  `mta.enabled` (env overrides TOML, no splicing into the `config` string):
  `LISTMNGR__MTA__ENABLED=true`, `INCOMING=postfix`,
  `MAP_DIRECTORY=/var/lib/listmngr/mta`, `LMTP_LISTEN=127.0.0.1:8024`,
  `LMTP_MAP_TARGET=127.0.0.1:8024`, `SMTP_RELAY=127.0.0.1:25`,
  `SMTP_TLS=plaintext_trusted_relay` (loopback inside the pod — exactly the
  "isolated" condition of `deploy/README.md`), `LOCAL_HOSTNAME=<mta.hostname>`;
  `service.lmtp` remains only for an external MTA and `fail`s if enabled
  together with `mta.enabled`. Second Service `<fullname>-smtp`:
  `mta.service.type` (default `LoadBalancer`), port 25 → container 25,
  `externalTrafficPolicy: Local` (keeps the client IP for logs/`mynetworks`),
  `loadBalancerIP`/`annotations`. Harness `--mta`: build + load the Postfix
  image, install with `mta.enabled=true
  mta.hostname=lists.example.invalid`; `list create` via the CLI → maps appear
  (`kubectl exec -c postfix -- ls /var/lib/listmngr/mta/current`); SMTP session
  via `port-forward` 25: `RCPT TO:` a list address → 250, an unknown address →
  550, outbound relay from a client outside the pod → 554 (the
  `scripts/check-mta-configs.sh` matrix in live form); send one message to the
  list → the runner picks it up (log `kubectl logs -c listmngr`), outbound
  delivery has no real destination so it stops in the retry queue — recorded
  as bounded. Test: `test_helm.py` (sidecar present/absent, MTA env correct,
  smtp Service, cap list exactly eight, `fail` when `service.lmtp` +
  `mta.enabled`), `test_release.py` adds the Postfix image in the workflow.
  Docs: book Install + Connect the mail system (Kubernetes section:
  DNS/MX/SPF/DKIM pointing at the `LoadBalancer`, `secretFiles` for the DKIM
  key), `deploy/README.md`, ARCHITECTURE, SECURITY (the sidecar's hardening
  exception), CHANGELOG.
- [x] ~~**P10-HELM-PUBLISH**~~ (done) (S): install from the registry. `release.yml` job `chart`:
  `helm registry login ghcr.io` with `GITHUB_TOKEN` (`packages: write`),
  `helm push dist/listmngr-<v>.tgz oci://ghcr.io/p10node/charts` →
  `cosign sign --yes ghcr.io/p10node/charts/listmngr@<digest>` (digest from the
  `helm push` output), the `.tgz` still goes to the release + `SHA256SUMS`;
  `provenance`/`Chart.yaml` `annotations` (`org.opencontainers.image.source`).
  Docs are the main part: book Install "Helm" rewritten as a runbook (install
  `oci://ghcr.io/p10node/charts/listmngr --version <v>`, `helm show values`,
  minimal values for DB/MTA/ingress + cert-manager annotation example,
  first-run `user create` + TOTP as in the current Install, `helm upgrade`,
  `helm rollback` ≠ schema rollback → point to UPGRADE); `docs/UPGRADE.md`
  section "Containers and Helm" expanded (order: backup → `helm upgrade` →
  `doctor`; chart minor ↔ app minor share the same number); README bullet
  "Helm chart" gains the OCI address; `deploy/README.md` Helm section
  completed; ledger. Test: `test_release.py` needles
  `helm push`/`oci://`/`cosign sign` for the chart; `test_book.py` links. Limit
  stated explicitly: the OCI push and chart cosign run only when a tag is
  pushed — the same status as the rest of `release.yml`.
- [x] ~~**P10-IMAGE-NATIVE**~~ (done) (S): after the real `v1.2.0` release (2026-10-08) the
  `image` job's arm64 build via QEMU took 138 minutes. `release.yml`: the
  `image` job becomes a matrix of `linux/amd64` on `ubuntu-24.04` +
  `linux/arm64` on `ubuntu-24.04-arm` (each runner builds both images natively,
  `push-by-digest`, uploads the digest as an artifact), job `image-manifest`
  downloads the digests, `docker buildx imagetools create` joins the manifest
  list under `<version>`, `<major>.<minor>`, `latest` for both `listmngr` and
  `listmngr-postfix`, reads the list digest with `imagetools inspect
  --format`, cosign signs by digest, `publish` takes the digest from this job;
  `setup-qemu-action` dropped. Test: `test_release.py` (matrix, arm runner, no
  QEMU, join + sign step, `publish` needs). Locally only the join step can be
  rehearsed (a single-use `registry:3` registry, arm64 image already
  available); the `push-by-digest` step needs the `docker-container` driver
  like `setup-buildx-action` on CI → the real evidence is the next tag push.
  Docs: book Releases, ARCHITECTURE, CHANGELOG.
- [x] ~~**P10-1.2.1**~~ (done) (S): patch release after `v1.2.0` — the release run for tag 1.2.0
  never completed (`publish` failed twice: chart cosign without login, then
  `download-artifact` tripped over two `.dockerbuild` artifacts), arm64 image
  via QEMU 3 h 21 min; `30767bb` also contains a code fix (`mime_delete`) not
  in the 1.2.0 changelog → do not move the tag again, cut `1.2.1`: bump as in
  `P10-1.2` (44 manifest lines, lock, `Chart.yaml`, fixture, fuzz lock,
  `test_version.py`), CHANGELOG `## [1.2.1] - 2026-10-09` collecting
  `[Unreleased]` (IMAGE-NATIVE, 4 fixes), SECURITY/README/book, ledger; tag
  `v1.2.1` locally, push it alone (first run of the new pipeline); `v1.2.0`
  stays pointing at `25dc7bb` (has image/chart on GHCR, no release page) and
  is recorded explicitly in SECURITY/ledger.
- [x] ~~**P10-1.2**~~ (done) (S, user's decision): bump `1.2.0` as in `P8-1.1`
  (workspace, path deps, `Chart.yaml` `version`/`appVersion`, fixture
  `system-versions.json`, `fuzz/Cargo.lock`, `scripts/tests/test_version.py`),
  `CHANGELOG.md` `## [1.2.0] - <date>`, phase ledger table closes Phase 10,
  annotated tag `v1.2.0` locally (pushing is the user's decision). Without it,
  the `1.1.0` chart in the repo differs from the `1.1.0` chart already
  packaged — recorded in the ledger.

Decisions to settle before starting (defaults in bold, used if no opinion is
given):

1. PostgreSQL in the chart: **hand-written StatefulSet from the Compose image** /
   external DB only / operator (CloudNativePG, needs CRDs).
2. Postfix: **sidecar in the same Pod** (RWO volume shared naturally, loopback
   LMTP) / separate Deployment (needs an RWX PVC or the same node).
3. Where to publish the chart: **`oci://ghcr.io/p10node/charts/listmngr`** / GitHub
   Pages `index.yaml` / only the `.tgz` on the release as today.
4. Do `P10-1.2` right after `P10-HELM-PUBLISH`, or bundle it with other backlog.
5. CI budget: the `helm` job builds the image in kind (~ the same as the current
   `container` job); accept that, or run only when `deploy/**` changes.

## 8. Testing strategy

| Type        | Tool                                                                                                     | Scope                                                                                                                                                  |
|-------------|----------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| Unit        | `cargo test`                                                                                             | rules, handlers (message in → out), VERP, hash, templates, preferences layering, bounce detectors (fixture corpus), digest builders (snapshot `insta`) |
| Property    | `proptest`                                                                                               | VERP encode/decode, address normalize, token, chunking                                                                                                 |
| Integration | `testcontainers` (pg), sqlite in-memory                                                                  | repos, REST (axum `oneshot`), migrations up/down, queue claim concurrency (N workers do not double-claim)                                              |
| E2E mail    | harness: pg + LMTP client + SMTP sink (Rust)                                                             | flow §7 P2/P3                                                                                                                                          |
| E2E web     | Playwright (Node dev-only, CI)                                                                           | flows §7 P4/P5                                                                                                                                         |
| Compat      | Python `mailmanclient` test script in CI (docker)                                                        | REST `/3.1/` shape                                                                                                                                     |
| Fuzz        | `cargo-fuzz`                                                                                             | parsers                                                                                                                                                |
| Security    | `cargo audit/deny`, ZAP baseline scan CI (P7), CSP report endpoint                                       |                                                                                                                                                        |
| Perf        | criterion (rules/pipeline), k6 (HTTP), custom (LMTP→SMTP throughput)                                     | P7                                                                                                                                                     |
| Test data   | public mbox corpus (Mailman/Python lists), flufl.bounce fixtures, DMARC DNS mock (hickory test resolver) |                                                                                                                                                        |

## 9. Migration from Mailman

| Source            | Method                                                                                                                                                                                                                         | Notes                                                                                                                                                                                 |
|-------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Mailman 2.1       | `listmngr import21 <list_id> /path/config.pck` + `archive import --mbox`                                                                                                                                                       | map 2.1 → 3 settings like Mailman's `import21` (mapping table in `docs/MIGRATION.md`); 2.1 passwords (sha/plain) → force reset                                                        |
| Mailman 3 Core    | `listmngr import3 --db postgres://…` (or `--rest URL`)                                                                                                                                                                         | copy domains/lists/settings/members/preferences/templates/bans/header_matches/held/pendings; user passwords (passlib pbkdf2/sha512_crypt) → verify legacy on login then rehash Argon2 |
| HyperKitty        | `listmngr import3 --hyperkitty-db …`                                                                                                                                                                                           | threads/votes/tags/categories/favorites; or import mbox + rebuild (loses votes/tags)                                                                                                  |
| Postorius/allauth | user emails/social accounts → `users`/`addresses`/`user_oidc`                                                                                                                                                                  |                                                                                                                                                                                       |
| URL compat        | **Done (P6-ACCEPTANCE)**: `/archives/list/{address}/message/{hash}/`, `/hyperkitty/…`, `/postorius/lists/{id}/` → 308 redirect to `/web/lists/{id}/archive?message=…` and the corresponding pages (`crates/api/src/compat.rs`) |                                                                                                                                                                                       |
| MTA               | change the LMTP transport port; regenerate alias/transport                                                                                                                                                                     |                                                                                                                                                                                       |
| Rollback          | in parallel: listmngr reads its own DB; keep Mailman on standby; cutover switches the transport map                                                                                                                            |                                                                                                                                                                                       |

## 10. Decisions to settle

| #   | Question                                 | Recommendation                                                                                                                                                                                             |
|-----|------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 1   | License                                  | **Decided: AGPL-3.0-or-later**; see ADR-0003 and `LICENSE`.                                                                                                                                                |
| 2   | DB                                       | Postgres-first, SQLite fully supported but single-node. Queries written portably, using runtime `sqlx::query` + testing both backends (no compile-time macros, to avoid 2 sets of queries).                |
| 3   | Frontend                                 | SSR askama + htmx (recommended) vs Leptos/Dioxus. SSR: simple, strict CSP, no WASM bundle, easy i18n.                                                                                                      |
| 4   | Name                                     | crate prefix `listmngr-*`, binary `listmngr`, env `LISTMNGR__*`, header `X-Listmngr-*`. Is an `X-Mailman-*` alias needed for compat? (recommendation: emit both in P2, config to turn off).                |
| 5   | REST compat depth                        | `/3.1/` is enough for `mailmanclient` to run; no emulation of `/3.0/`.                                                                                                                                     |
| 6   | Deploy target                            | Docker + systemd binary. Helm in P7; real Helm install (PG, Postfix sidecar, kind, OCI) in Phase 10.                                                                                                       |
| 7   | Templates i18n                           | Decided (P6-PO-TEMPLATES): vendor the Mailman translations (GPL-3.0-or-later) into the binary — compatible with AGPL-3.0-or-later per GPLv3 §13; source and terms in `crates/mail/catalog/mailman/SOURCE`. |
| 8   | Built-in inbound SMTP                    | experimental P6, not a primary goal.                                                                                                                                                                       |
| 9   | Python `mailmanclient` compat test in CI | yes (docker python), low CI cost, guarantees wire-compat.                                                                                                                                                  |

## 11. Risks

| Risk                                     | Mitigation                                                                                                             |
|------------------------------------------|------------------------------------------------------------------------------------------------------------------------|
| Parity list too long, easy to miss items | `FEATURE_PARITY.md` is a living checklist, each PR ticks it; cross-check by running the `mailmanclient` test suite.    |
| Bounce detection heuristics hard to port | use the flufl.bounce fixture corpus as oracle; prioritize standard DSN, heuristics later.                              |
| Deliverability (wrong DKIM/ARC/DMARC)    | test with real keys + `mail-tester`, `dkimvalidator`; e2e verify with `mail-auth`.                                     |
| SQLite + Postgres divergence             | test matrix for both; limit dialect-specific SQL, wrap in the repo layer.                                              |
| `sqlx 0.9` / `askama 0.16` new APIs      | pin versions, ADR records the reason; upgrade per phase.                                                               |
| Large UI scope                           | use reusable partial components; prioritize moderation & settings first, "nice-to-have" (command palette) later.       |
| Import 2.1 pickle                        | dedicated pickle reader (`crates/import/src/pickle.rs`) + real Mailman 3 `config.pck` fixture (`MAILMAN_TESTING_DIR`). |
