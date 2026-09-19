# listmngr — Kế hoạch chi tiết

> Mailman 3 alternative (Core + Postorius + HyperKitty + mailman-web + django-mailman3 + mailmanclient) viết bằng Rust.
> Single binary, feature parity, UI hiện đại, security-first.

## Trạng thái triển khai (2026-09-14) — thay cho các checkpoint 2026-09-06/07

Tài liệu này là **hợp đồng sản phẩm chuẩn và roadmap**, không phải bảng kê tính
năng đã giao. Bằng chứng theo từng acceptance ID nằm ở
[FEATURE_PARITY.md](FEATURE_PARITY.md); bề mặt đã triển khai mô tả ở
README/ARCHITECTURE; `Cargo.lock` là nguồn phiên bản phụ thuộc (ADR-0003). Các
checkpoint 2026-09-06/07 (mail path "plaintext trusted relay", "no current
PostgreSQL PASS", R1/O1 OPEN) đã bị vượt qua; nội dung cũ giữ trong git history.

Đã có bằng chứng (bounded local acceptance, ledger ID trong ngoặc):

- Phase 0/1: đầy đủ (`P0-*`, `P1-*`).
- Phase 2 mail path: LMTP (P2-TRANSPORT, P2-LMTP-PARAMETERS); store/queue/runtime
  (P2-STORE, P2-QUEUE, P2-RUNTIME, P2-LEASE-HEARTBEAT); chain/rule engine với
  15 rules, header-match và DMARC (P2-CHAIN-ENGINE, P2-CHAIN-RULES,
  P2-DMARC-MUNGE, P2-VALIDATE-AUTHENTICITY, P2-HEADER-MATCHES-REST); handler pipeline trừ
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
  -ACK, các notice); digests (P3-DIGEST-SETTINGS, P3-DIGEST-REST); task runner +
  `notify` (P3-TASK-RUNNER); `admin_notify_mchanges` (P3-ADMIN-NOTIFY-MCHANGES);
  i18n en/vi (P3-I18N); bans list + site (P3-LIST-POSTING-BANS,
  P3-SITE-BANS-REST).
- Phase 4: các lát SSR rời (P4-WEB-*: login/session/CSRF, held review có scope,
  members/policy, settings lát nhỏ, own postings, bounce recovery) — tất cả đã
  chuyển sang askama trong P4-SHELL: một đường render duy nhất, shell i18n
  (`Accept-Language` → `site.default_language` → `en`), design tokens + dark
  mode, a11y baseline (axe không critical/serious), htmx 2.0.10 vendored phục vụ
  từ chính origin nhưng chưa trang nào nạp. Accounts (P4-ACCOUNT-*), TOTP,
  passkeys (`passkeys.js` là script first-party duy nhất, `script-src 'self'`)
  và OIDC đã đóng; chưa CSP nonce (chưa cần). Xem §7.0 và Phase 4.
- Phase 5: threading + Message-ID-Hash tương thích HyperKitty + archive đọc cơ
  bản (P2-ARCHIVE-AUTHORITY). Chưa có search/UI đầy đủ.

R1 (partial LMTP batch commit) và O1 (lease clock sau lock wait) trong bảng
"Residual P1s" của ledger đã được các tăng trưởng sau đóng:
`crates/runners/tests/inbound_safety.rs`
(`second_recipient_storage_failure_rolls_back_entire_batch`,
`data_database_wait_timeout_replies_for_all_without_late_intake`) và các contract
PostgreSQL `mail_queue_lock_clock`, `sibling_lease_clock`, `email_command_clock`
trong `scripts/test-postgres.sh`. Ngày 2026-09-14 tất cả PASS trong
`cargo test --locked --workspace --all-targets --no-fail-fast` (145 binaries,
936 passed, 0 failed, 56 ignored) và `TEST_POSTGRES_URL=… scripts/test-postgres.sh`
(38 tests, PostgreSQL 14.24 dùng một lần rồi drop). Ledger đã cập nhật.

Chưa có: bộ test tương đương doctest mailmanclient (P3-CLIENT-SUITE), toàn bộ
Phase 4–7 trừ các lát nêu trên (P4-SHELL, toàn bộ P4-ACCOUNT-*, P4-TOTP,
P4-WEBAUTHN, P4-OIDC, P4-LIST-SETTINGS, P4-MEMBERS, P4-HELD-QUEUE, P4-LIST-CREATE-INDEX, P4-DOMAINS-USERS, P4-SYSTEM, P4-MODERATION-CROSS, P4-GDPR, P4-ACCEPTANCE đã đóng — Phase 4 hoàn tất theo ledger; Phase 5: P5-RENDER, P5-SEARCH, P5-UI, P5-INTERACTIONS, P5-WEB-POST, P5-MBOX, P5-ADMIN, P5-REMOTE-ARCHIVERS, P5-ACCEPTANCE đã đóng — Phase 5 hoàn tất theo ledger). Chi tiết và thứ tự ở §7.

## 0. Tóm tắt 1 phút

- **Thay thế**: `mailman-core` + `postorius` + `hyperkitty` + `mailman-web` + `django-mailman3` + `mailmanclient` + uwsgi + qcluster/cron → 1 binary `listmngr`.
- **Giữ nguyên**: tên khái niệm (list, member, role, chain, rule, pipeline, handler, runner, template URI), REST `/3.1/` wire-compat, sub-address (`-request`, `-join`, `-leave`, `-owner`, `-bounces`, `-confirm`), `Message-ID-Hash`, `Archived-At`, URL archive.
- **Nâng cấp**: DB-backed queue (không pickle/qfiles), Argon2id + TOTP + WebAuthn + OIDC, scoped API token, DKIM/ARC/DMARC native, CSRF/CSP/rate-limit, audit log, tantivy search, htmx SSR UI, Prometheus, SQLite dev mode.
- **Stack**: Rust 2024, tokio, axum, sqlx (Postgres + SQLite), askama + htmx, mail-parser/mail-send/mail-auth, tantivy.
- **Roadmap**: 8 phase (0→7). Phase 2 kết thúc = gửi/nhận thư qua list hoạt động. Phase 5 kết thúc = parity đầy đủ. Phase 7 = 1.0.

## 1. Mục tiêu & phạm vi

### 1.1 Tuyên bố

Người đang tìm/đang dùng Mailman có thể chọn listmngr mà **không mất tính năng nào**, deploy đơn giản hơn, UI tốt hơn, bảo mật cao hơn.

### 1.2 Nguyên tắc thiết kế

| Nguyên tắc                 | Ý nghĩa cụ thể                                                                                                                                        |
|----------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------|
| Parity trước, mở rộng sau  | Mọi tính năng Mailman 3.3.x có tương đương. Tên khái niệm giữ nguyên → đọc doc Mailman vẫn hiểu listmngr.                                             |
| Wire-compat                | REST `/3.1/` đủ để `mailmanclient` (Python) chạy; sub-address & VERP format giống; archive URL dùng `message_id_hash` (base32-sha1) giống HyperKitty. |
| Single binary, zero-Python | Không Django/Celery/uwsgi/cron. Static assets & templates embed vào binary. 1 config file.                                                            |
| Security by default        | Argon2id, 2FA, scoped tokens, DKIM/ARC, CSRF, CSP, rate-limit, audit log, HTML sanitize. Mặc định an toàn, không cần bật.                             |
| Crash-safe, multi-node     | Queue + message store trong DB/object store. `FOR UPDATE SKIP LOCKED`. Chạy nhiều node cùng DB.                                                       |
| Observability              | `tracing` structured logs, `/metrics` Prometheus, `/healthz`, `/readyz`.                                                                              |
| Không unsafe               | `unsafe_code = "forbid"` toàn workspace.                                                                                                              |

### 1.3 Non-goals (v1)

- Không tương thích URL/UI Mailman 2.1 (chỉ import dữ liệu 2.1).
- Không Python plugin API. Thay bằng webhook + Rust trait plugin (compile-time). WASM plugin → sau 1.0.
- Không MySQL/MariaDB ở v1 (Postgres + SQLite). → 1.x.
- Không thay MTA hoàn toàn: vẫn cần Postfix/Exim nhận thư từ internet. listmngr = LMTP in + SMTP out. Built-in inbound SMTP = experimental (phase 6).
- Không NNTP ở phase đầu (phase 6).

## 2. Tech stack

| Thành phần        | Crate / tool                                                                | Version                   | Lý do                                      |
|-------------------|-----------------------------------------------------------------------------|---------------------------|--------------------------------------------|
| Runtime           | `tokio`                                                                     | 1.53                      | chuẩn async                                |
| HTTP              | `axum` + `tower-http` + `axum-extra`                                        | 0.8 / 0.7 / 0.12          | ergonomic, middleware tower                |
| DB                | `sqlx` (postgres, sqlite, runtime-tokio, tls-rustls, chrono, uuid, migrate) | 0.9                       | async, migrations embed                    |
| Templates         | `askama` + `askama_web` (axum-0.8)                                          | 0.16                      | compile-time, type-safe                    |
| Frontend          | htmx 2.x (vendored), CSS thuần (design tokens), không Node runtime          | –                         | SSR, ít JS, CSP strict                     |
| Mail parse/build  | `mail-parser`, `mail-builder`                                               | 0.11 / 0.5                | robust MIME                                |
| SMTP out          | `mail-send`                                                                 | 0.6                       | SMTP client + DKIM sign                    |
| Email auth        | `mail-auth` (DKIM/ARC/SPF/DMARC, hickory DNS)                               | 0.12                      | verify in + sign out + DMARC policy lookup |
| Search            | `tantivy`                                                                   | 0.26                      | thay Whoosh/Elasticsearch                  |
| CLI               | `clap` (derive)                                                             | 4.6                       |                                            |
| Config            | `figment` (toml + env)                                                      | 0.10                      | layered                                    |
| OpenAPI           | `utoipa`                                                                    | 5.5                       | spec từ code                               |
| Password          | `argon2` + `zxcvbn`                                                         | 0.6 / 3.1                 | Argon2id                                   |
| 2FA               | `totp-rs`, `webauthn-rs`                                                    | 6.0 / bản stable gần nhất |                                            |
| Session           | `tower-sessions` + `tower-sessions-sqlx-store`                              | 0.15                      | DB session                                 |
| Rate limit        | `governor`                                                                  | 0.10                      |                                            |
| Secrets           | `secrecy`, `zeroize`, `subtle`                                              | 0.10 / 1.9 / 2.6          | không log, const-time                      |
| HTML sanitize     | `ammonia`                                                                   | 4.1                       | archive render                             |
| Markdown          | `pulldown-cmark`                                                            | 0.13                      | `archive_rendering_mode=markdown`          |
| HTML→text         | `html2text`                                                                 | 0.17                      | `convert_html_to_plaintext`                |
| Hash              | `sha1` + `data-encoding`                                                    | 0.11 / 2.11               | `Message-ID-Hash` base32(sha1)             |
| Validation        | `garde`                                                                     | 0.23                      |                                            |
| Embed assets      | `rust-embed`                                                                | 8.12                      |                                            |
| Errors/log        | `thiserror`, `anyhow`, `tracing`, `tracing-subscriber`                      | 2 / 1 / 0.1               |                                            |
| IDs/time          | `uuid` (v7), `chrono`                                                       | 1.26 / 0.4                |                                            |
| IDN               | `idna`                                                                      | 1.1                       |                                            |
| Pickle (import21) | `serde-pickle`                                                              | –                         | đọc `config.pck` Mailman 2.1               |
| Test              | `testcontainers`, `proptest`, `cargo-fuzz`, `insta`, Playwright (dev only)  | –                         |                                            |
| Supply chain      | `cargo-deny`, `cargo-audit`, `cargo-sbom`, `cosign`                         | –                         |                                            |

Ghi chú:

- `rustls` đang ở 0.24-dev → dùng bản stable qua transitive deps của `sqlx`/`mail-send`, không pin trực tiếp.
- Rust 2024 edition có `async fn` in trait; `async-trait` chỉ dùng cho `dyn` object (handlers/rules registry).

## 3. Kiến trúc

### 3.1 Workspace layout

```
listmngr/
├── Cargo.toml                  # workspace, [workspace.dependencies] pin version, lints
├── rust-toolchain.toml         # stable
├── deny.toml, rustfmt.toml, clippy.toml
├── CLAUDE.md, README.md, CHANGELOG.md
├── docs/
│   ├── PLAN.md                 # file này
│   ├── ARCHITECTURE.md
│   ├── FEATURE_PARITY.md       # checklist sống, cập nhật mỗi PR
│   ├── SECURITY.md             # threat model + disclosure
│   ├── MIGRATION.md            # từ Mailman 2.1 / 3.x
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

Dependency graph (chỉ 1 chiều):

```
cli ─┬─> api ─┐
     ├─> web ─┼─> archive ─> pipeline ─> mail ─┐
     └─> runners ┘                       db ───┼─> core
```

### 3.2 Process model

`listmngr serve` = 1 process, 1 tokio runtime, chạy đồng thời:

| Thành phần        | Mô tả                                                                                                                                       |
|-------------------|---------------------------------------------------------------------------------------------------------------------------------------------|
| HTTP server       | axum: `/` web UI, `/3.1/` REST compat, `/api/v1/` REST mới, `/archives/`, `/metrics`, `/healthz`, `/readyz`, `/openapi.json`                |
| LMTP listener     | `:8024` (RFC 2033). Nhận thư từ MTA → ghi message store → enqueue `in`. Reject sớm nếu > max size / list không tồn tại.                     |
| Runner supervisor | mỗi runner = tokio task (có thể N worker/runner). Poll queue table (`SKIP LOCKED`), exponential backoff khi idle. Restart runner nếu panic. |
| Scheduler         | digest periodic, `task` runner (expire pendings/workflow states, cache cleanup), bounce stale reset, transport map regen, index commit.     |

- `listmngr serve --roles web,mta,runners` → tách vai trò khi scale ngang (nhiều node cùng DB + shared message store).
- Graceful shutdown (SIGTERM): dừng nhận job mới, drain in-flight (timeout), đóng listener.
- Không cần cron/celery/qcluster.

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
  │                    reject → bounce về sender (virgin queue)
  │                    discard→ log
  │
  ├─ pipeline runner ─> handlers theo thứ tự → to-archive queue(archive)
  │                                          → to-digest (append digest mbox)
  │                                          → to-usenet queue(nntp)
  │                                          → to-outgoing queue(out)
  ├─ out runner ─────> SMTP relay, chunk theo max_recipients, VERP/personalize, DKIM sign
  │                    fail tạm → queue(retry) (backoff), fail vĩnh viễn → bounce event
  ├─ bounce runner ──> detect (DSN + heuristics) → bounce_events → score → warn/disable/remove
  ├─ command runner ─> email commands → reply (virgin)
  ├─ archive runner ─> parse, thread, index (tantivy), attachments
  ├─ digest runner ──> theo threshold/periodic → build MIME + RFC1153 → queue(out)
  ├─ virgin runner ──> thư do hệ thống sinh (notices, replies) → cook-headers → out
  ├─ retry runner ───> re-enqueue out
  ├─ task runner ────> expiry/cleanup định kỳ
  └─ shunt/bad ──────> job lỗi (poison) giữ lại, `listmngr unshunt`
```

### 3.4 Data model (Postgres; SQLite cùng schema)

Ký hiệu: `PK`, `FK→`, `UQ`, `JSONB` (SQLite = TEXT json).

**Identity**

| Bảng                         | Cột chính                                                                                                                                       |
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

| Bảng                      | Cột chính                                                                                                                                                                                                                                                                                                                                                             |
|---------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `mailing_lists`           | `id PK`, `list_id UQ (name.domain)`, `list_name`, `mail_host FK→domains`, `display_name`, `description`, `info`, `subject_prefix`, `advertised`, `anonymous_list`, `created_at`, `last_post_at`, `post_id`, `volume`, `next_digest_number`, `digest_last_sent_at`, `emergency`, `style_name`, + toàn bộ settings §4.1 dạng cột typed; setting ít dùng → `extra JSONB` |
| `list_acceptable_aliases` | `list_id`, `alias (pattern)`                                                                                                                                                                                                                                                                                                                                          |
| `list_nonmember_rules`    | `list_id`, `kind (accept/hold/reject/discard)`, `pattern`                                                                                                                                                                                                                                                                                                             |
| `members`                 | `id uuid PK`, `list_id FK`, `role (owner/moderator/member/nonmember)`, `address_id FK`, `user_id FK nullable`, `subscription_mode (as_address/as_user)`, `moderation_action nullable`, `display_name`, `preferences_id FK`, `bounce_score`, `last_bounce_received`, `last_warning_sent`, `total_warnings_sent`, `created_at`; `UQ(list_id, role, address_id)`         |
| `preferences`             | `id PK`, `acknowledge_posts`, `hide_address`, `preferred_language`, `receive_list_copy`, `receive_own_postings`, `delivery_mode`, `delivery_status` (tất cả nullable → layered: member → address → user → system)                                                                                                                                                     |
| `header_matches`          | `id`, `list_id`, `position`, `header`, `pattern`, `action nullable`, `tag`, `chain nullable`                                                                                                                                                                                                                                                                          |
| `bans`                    | `id`, `list_id nullable (null = global)`, `email_or_regex`                                                                                                                                                                                                                                                                                                            |
| `templates`               | `id`, `name (list:member:regular:footer …)`, `scope (site/domain/list)`, `scope_id`, `language`, `uri nullable`, `body nullable`, `username/password (cho http uri)`; `UQ(name,scope,scope_id,language)`                                                                                                                                                              |
| `list_archivers`          | `list_id`, `name`, `enabled`                                                                                                                                                                                                                                                                                                                                          |
| `list_styles`             | code-defined (legacy-default, legacy-announce, private-default) + `custom_styles JSONB`                                                                                                                                                                                                                                                                               |

**Workflow & moderation**

| Bảng              | Cột chính                                                                                                                                                |
|-------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------|
| `pendings`        | `id`, `token_hash UQ`, `kind (subscription/unsubscription/held/probe/invite/verify)`, `payload JSONB`, `expires_at`, `created_at`                        |
| `workflow_states` | `token`, `name (subscription/unsubscription)`, `step`, `data JSONB`                                                                                      |
| `held_messages`   | `id`, `list_id`, `message_key FK→messages`, `sender`, `subject`, `reason`, `hold_date`, `msgdata JSONB`, `moderator_id nullable`, `disposition nullable` |
| `moderation_log`  | `held_id`, `action`, `reason`, `moderator_id`, `forward_to`, `at`                                                                                        |

**Mail infrastructure**

| Bảng                             | Cột chính                                                                                                                                                                                                                      |
|----------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `messages` (message store index) | `id PK`, `message_id`, `message_id_hash UQ`, `list_id nullable`, `size`, `store_key` (fs/db/s3 ref), `headers JSONB (subset)`, `created_at`, `refcount`                                                                        |
| `message_blobs`                  | `store_key PK`, `raw bytea` (backend `db` only)                                                                                                                                                                                |
| `queue_jobs`                     | `id bigserial`, `queue (in/out/pipeline/…)`, `message_key`, `msgdata JSONB`, `run_after`, `attempts`, `max_attempts`, `locked_by`, `locked_at`, `last_error`, `created_at`; index `(queue, run_after) WHERE locked_by IS NULL` |
| `bounce_events`                  | `id`, `list_id`, `email`, `timestamp`, `message_id`, `context (normal/relay/probe)`, `processed bool`                                                                                                                          |
| `digest_mbox`                    | `list_id`, `volume`, `number`, `messages (append-only refs)`, `size`                                                                                                                                                           |
| `dkim_keys`                      | `domain_id`, `selector`, `algorithm (rsa2048/ed25519)`, `private_key (encrypted)`, `public_dns_record`, `active`, `rotated_at`                                                                                                 |
| `nntp_watermarks`                | `list_id`, `watermark`                                                                                                                                                                                                         |

**Archive**

| Bảng                               | Cột chính                                                                                                                                                                                                                                         |
|------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `archive_lists`                    | `list_id`, `archive_policy cache`, `stats cache JSONB`                                                                                                                                                                                            |
| `archive_senders`                  | `id`, `email`, `display_name`, `user_id nullable`                                                                                                                                                                                                 |
| `archive_threads`                  | `id`, `list_id`, `thread_id (hash msg đầu)`, `subject`, `date_active`, `starting_message_id`, `category_id nullable`, `reply_count`, `participants_count`                                                                                         |
| `archive_messages`                 | `id`, `list_id`, `message_id_hash UQ(list,hash)`, `thread_id FK`, `parent_id nullable`, `sender_id`, `subject`, `body_text`, `body_html_sanitized nullable`, `date`, `timezone`, `in_reply_to`, `references text[]`, `store_key`, `archived_date` |
| `archive_attachments`              | `message_id FK`, `counter`, `name`, `content_type`, `encoding`, `size`, `store_key`                                                                                                                                                               |
| `archive_votes`                    | `message_id`, `user_id`, `value (+1/-1)`                                                                                                                                                                                                          |
| `archive_tags`, `archive_taggings` | tag name; `(thread_id, tag_id, user_id)`                                                                                                                                                                                                          |
| `archive_categories`               | `id`, `name`, `color`                                                                                                                                                                                                                             |
| `archive_favorites`                | `(thread_id, user_id)`                                                                                                                                                                                                                            |
| `archive_last_views`               | `(thread_id, user_id)`, `view_date`                                                                                                                                                                                                               |
| tantivy index                      | trên disk `data/index/`, rebuild từ DB bất cứ lúc nào                                                                                                                                                                                             |

**Ops**

| Bảng                 | Cột chính                                                                                                            |
|----------------------|----------------------------------------------------------------------------------------------------------------------|
| `audit_log`          | `id`, `at`, `actor_user_id`, `actor_token_id`, `ip`, `action`, `target_type`, `target_id`, `diff JSONB`; append-only |
| `webhooks`           | `id`, `url`, `secret_hash`, `events text[]`, `list_id nullable`, `enabled`                                           |
| `webhook_deliveries` | retry state                                                                                                          |
| `_sqlx_migrations`   | sqlx                                                                                                                 |

### 3.5 Queue & message store

- Queue = bảng `queue_jobs`. Claim:

```sql
UPDATE queue_jobs SET locked_by = $worker, locked_at = now()
WHERE id = (
  SELECT id FROM queue_jobs
  WHERE queue = $queue AND run_after <= now() AND locked_by IS NULL
  ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED
) RETURNING *;
```

- Lock timeout: job `locked_at` quá `runner.lock_timeout` → coi như bỏ, worker khác nhận lại.
- Poison job (attempts ≥ max) → chuyển `queue = 'shunt'` kèm `last_error`. CLI `listmngr unshunt`, `listmngr qfile <id>`.
- Tên queue giữ nguyên Mailman: `in`, `pipeline`, `out`, `retry`, `bounces`, `command`, `virgin`, `archive`, `digest`, `nntp`, `shunt`, `bad`.
- Message store trait:

```rust
trait MessageStore {
    async fn put(&self, raw: &[u8]) -> Result<StoreKey>;
    async fn get(&self, key: &StoreKey) -> Result<Bytes>;
    async fn delete(&self, key: &StoreKey) -> Result<()>;
}
```

  Backends: `fs` (default, `data/messages/ab/cd/<hash>.eml`), `db` (`message_blobs`), `s3` (phase 6). Raw bytes giữ nguyên, không normalize (cần cho DKIM verify & mbox export).
- SQLite: không có `SKIP LOCKED` → claim trong `BEGIN IMMEDIATE`; chỉ hỗ trợ single-node.

### 3.6 Config

File `listmngr.toml` + env override `LISTMNGR__<SECTION>__<KEY>` (figment). Secret hỗ trợ `*_file` (docker secrets).

```toml
[site]
name = "Example Lists"
site_owner = "postmaster@example.com"
default_language = "en"
base_url = "https://lists.example.com"

[database]
url = "postgres://listmngr:***@localhost/listmngr"   # hoặc sqlite://data/listmngr.db
max_connections = 20

[message_store]
backend = "fs"          # fs | db | s3
path = "data/messages"

[mta]
incoming = "postfix"    # postfix | exim | none  (sinh transport/alias maps)
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
signup = true                 # /web/signup; tắt để chỉ quản trị viên tạo tài khoản

[api]
listen = "127.0.0.1:8001"             # có thể trùng web
compat_basic_auth = false             # bật cho mailmanclient
compat_basic_auth_allow = ["127.0.0.1/32"]

[security]
argon2 = { memory_kib = 65536, iterations = 3, parallelism = 1 }
password_min_score = 3
require_2fa_for = ["server_owner"]
pending_request_life = "3d"
rate_limit = { login = "5/min", subscribe = "10/hour", api = "600/min", api_pre_auth = "1200/min" }

[mailman]                              # giữ tên nhóm cấu hình Mailman để dễ map
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

| MTA                                   | Inbound                                                                  | Maps sinh tự động                                                                                                                                                             |
|---------------------------------------|--------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Postfix (khuyến nghị)                 | `transport_maps`, `relay_recipient_maps`, `relay_domains` → LMTP `:8024` | `data/mta/current/{domains,recipients,transport}.regexp` (hoặc `hash:` `postfix_domains`/`postfix_lmtp` + `postmap`), chạy khi start, tạo/xoá list & `listmngr aliases regen` |
| Exim 4                                | router `manualroute` + `lsearch` giống Mailman 3                         | `data/mta/current/exim_{domains,recipients}` + snippet trong `deploy/exim/`                                                                                                   |
| Built-in SMTP (phase 6, experimental) | listmngr nhận `:25` trực tiếp                                            | không cần map                                                                                                                                                                 |

Outbound: SMTP relay (Postfix localhost hoặc external có AUTH/STARTTLS/implicit TLS), chunk theo `max_recipients`, `MAIL FROM = list-bounces+VERP@host`, DKIM sign per domain, `List-*` headers.

## 4. Feature parity matrix

Trạng thái: `P0`..`P7` = phase dự kiến. Cột "Thay đổi" = khác Mailman thế nào.

### 4.1 Core — List settings (Postorius 9 nhóm)

| Nhóm                | Setting (tên Mailman giữ nguyên)                                                                                                                                                                                                                                                                                                                                                                                              | Phase                      |
|---------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|----------------------------|
| List Identity       | `display_name`, `description`, `info`, `subject_prefix`, `advertised`, `preferred_language`, `mail_host`, `list_name`, `fqdn_listname`, `list_id`, `created_at`, `last_post_at`, `post_id`, `volume`                                                                                                                                                                                                                          | P1                         |
| Automatic Responses | `autorespond_owner`, `autoresponse_owner_text`, `autorespond_postings`, `autoresponse_postings_text`, `autorespond_requests`, `autoresponse_request_text`, `autoresponse_grace_period`, `respond_to_post_requests`, `send_welcome_message`, `send_goodbye_message`, `admin_immed_notify`, `admin_notify_mchanges`                                                                                                             | P3                         |
| Alter Messages      | `filter_content`, `filter_types`, `pass_types`, `filter_extensions`, `pass_extensions`, `collapse_alternatives`, `convert_html_to_plaintext`, `filter_action`, `anonymous_list`, `include_rfc2369_headers`, `allow_list_posts`, `reply_goes_to_list` (no_munging/point_to_list/explicit_header), `reply_to_address`, `first_strip_reply_to`, `personalize` (none/individual/full), `include_sender_header` (Sender: override) | P2                         |
| DMARC Mitigations   | `dmarc_mitigate_action` (no_mitigation/munge_from/wrap_message/reject/discard), `dmarc_mitigate_unconditionally`, `dmarc_addresses`, `dmarc_moderation_notice`, `dmarc_wrapped_message_text`                                                                                                                                                                                                                                  | P2 (munge_from) / P6 (đủ)  |
| Digest              | `digests_enabled`, `digest_size_threshold`, `digest_send_periodic`, `digest_volume_frequency` (yearly/monthly/quarterly/weekly/daily), `next_digest_number`, `digest_last_sent_at`                                                                                                                                                                                                                                            | P3                         |
| Message Acceptance  | `default_member_action`, `default_nonmember_action` (defer/accept/hold/reject/discard), `accept_these_nonmembers`, `hold_these_nonmembers`, `reject_these_nonmembers`, `discard_these_nonmembers`, `require_explicit_destination`, `acceptable_aliases`, `administrivia`, `max_message_size`, `max_num_recipients`, `emergency`, `posting_pipeline`, `moderator "Approved:" posting key`                                      | P2                         |
| Archiving           | `archive_policy` (public/private/never), `archive_rendering_mode` (text/markdown), `archivers` (prototype/local=listmngr/mail-archive/mhonarc/hyperkitty-remote)                                                                                                                                                                                                                                                              | P1 (field) / P5 (thực thi) |
| Member Policy       | `subscription_policy` (open/confirm/moderate/confirm_then_moderate), `unsubscription_policy`, `member_roster_visibility` (public/members/moderators)                                                                                                                                                                                                                                                                          | P3                         |
| Bounce Processing   | `process_bounces`, `bounce_score_threshold`, `bounce_info_stale_after`, `bounce_you_are_disabled_warnings`, `bounce_you_are_disabled_warnings_interval`, `bounce_notify_owner_on_disable`, `bounce_notify_owner_on_removal`, `bounce_notify_owner_on_bounce_increment`, `forward_unrecognized_bounces_to` (discard/site_owner/administrators)                                                                                 | P3                         |
| Usenet              | `gateway_to_mail`, `gateway_to_news`, `linked_newsgroup`, `nntp_prefix_subject_too`, `newsgroup_moderation` (none/open_moderated/moderated), `usenet_watermark`                                                                                                                                                                                                                                                               | P6                         |
| Địa chỉ             | `posting_address`, `bounces_address`, `join_address`, `leave_address`, `owner_address`, `request_address`, `no_reply_address` (derived)                                                                                                                                                                                                                                                                                       | P1                         |
| Styles              | `legacy-default`, `legacy-announce`, `private-default` + custom style (JSON)                                                                                                                                                                                                                                                                                                                                                  | P1                         |

### 4.2 Core — Users, addresses, membership

| Tính năng                                                                                                                                                                                          | Phase         | Thay đổi             |
|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------|----------------------|
| User: `user_id` uuid, `display_name`, password, `is_server_owner`, preferences, nhiều addresses, `preferred_address`                                                                               | P1            | thêm locale/timezone |
| Address: verify/unverify, link/unlink user, `display_name`, case-preserve `original_email`                                                                                                         | P1            |                      |
| Member roles: owner, moderator, member, nonmember                                                                                                                                                  | P1            |                      |
| `subscription_mode`: as_address / as_user                                                                                                                                                          | P1            |                      |
| Preferences layered: member → address → user → system (`delivery_mode`, `delivery_status`, `acknowledge_posts`, `hide_address`, `preferred_language`, `receive_list_copy`, `receive_own_postings`) | P1            |                      |
| `delivery_mode`: regular, plaintext_digests, mime_digests, summary_digests                                                                                                                         | P1/P3         |                      |
| `delivery_status`: enabled, by_user, by_bounces, by_moderator, unknown                                                                                                                             | P1            |                      |
| Per-member `moderation_action` override                                                                                                                                                            | P2            |                      |
| Subscription workflow: verify → confirm → moderate (state machine, resume được), `pre_verified`, `pre_confirmed`, `pre_approved`, `invitation`, `send_welcome_message` override                    | P3            | token hashed         |
| Unsubscription workflow (confirm / moderate)                                                                                                                                                       | P3            |                      |
| Mass subscribe / mass unsubscribe / sync members (file)                                                                                                                                            | P1 CLI, P4 UI |                      |
| Bans: global + per-list, regex `^`                                                                                                                                                                 | P3            |                      |
| Server owners                                                                                                                                                                                      | P1            |                      |
| Find member (`/members/find`, `findmember`)                                                                                                                                                        | P1            |                      |

### 4.3 Core — Chains & rules (moderation)

Chains: `default-posting-chain`, `default-owner-chain`, `accept`, `hold`, `reject`, `discard`, `moderation`, `header-match`, `dmarc-mitigation`. Link actions: `jump`, `defer`, `stop`, `run`, `detour`.

| Rule                                                                                                                                               | Mô tả                                                                                    | Phase           |
|----------------------------------------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------|-----------------|
| `approved`                                                                                                                                         | `Approved:`/`Approve:` header hoặc dòng đầu body khớp posting key → accept; strip header | P2              |
| `emergency`                                                                                                                                        | list `emergency=true` → hold                                                             | P2              |
| `loop`                                                                                                                                             | `List-Post` trùng list → discard                                                         | P2              |
| `banned-address`                                                                                                                                   | sender bị ban (global/list) → reject                                                     | P2              |
| `member-moderation`                                                                                                                                | member có `moderation_action`                                                            | P2              |
| `nonmember-moderation`                                                                                                                             | `*_these_nonmembers` + `default_nonmember_action`                                        | P2              |
| `administrivia`                                                                                                                                    | body giống lệnh (subscribe/unsubscribe…)                                                 | P2              |
| `implicit-dest`                                                                                                                                    | `require_explicit_destination` + `acceptable_aliases`                                    | P2              |
| `max-recipients`                                                                                                                                   |                                                                                          | P2              |
| `max-size`                                                                                                                                         |                                                                                          | P2              |
| `no-subject`                                                                                                                                       |                                                                                          | P2              |
| `suspicious-header`                                                                                                                                | header matches (legacy)                                                                  | P2              |
| `no-senders`                                                                                                                                       | không có From/Sender                                                                     | P2              |
| `news-moderation`                                                                                                                                  |                                                                                          | P6              |
| `dmarc-mitigation`                                                                                                                                 | DNS `_dmarc` p=reject/quarantine (+org domain qua PSL)                                   | P2              |
| `digests`                                                                                                                                          | rule cho digest messages                                                                 | P3              |
| `any`, `truth`                                                                                                                                     | glue                                                                                     | P2              |
| Header matches động (per-list, action/tag/chain)                                                                                                   |                                                                                          | P2              |
| Hold reasons & moderator notice (`list:admin:action:post`), user notice (`list:user:notice:hold`)                                                  |                                                                                          | P2              |
| Moderator actions: accept / reject (reason) / discard / defer, forward, "moderate sender" → set member action, "add to ban", "add to header match" |                                                                                          | P2 REST / P4 UI |

### 4.4 Core — Pipeline handlers

| Pipeline                                      | Handlers (thứ tự)                                                                                                                                                                                                                                                                                | Phase                              |
|-----------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------------------------------|
| `default-posting-pipeline`                    | `validate-authenticity` → `mime-delete` → `tagger` → `member-recipients` → `avoid-duplicates` → `cleanse` → `cleanse-dkim` → `cook-headers` → `subject-prefix` → `rfc-2369` → `to-archive` → `to-digest` → `to-usenet` → `after-delivery` → `acknowledge` → `dmarc` → `arc-sign` → `to-outgoing` | P2 (trừ to-usenet P6, arc-sign P6) |
| `virgin`                                      | `cook-headers` → `to-outgoing`                                                                                                                                                                                                                                                                   | P2                                 |
| `default-owner-pipeline`                      | `owner-recipients` → `cleanse` → `cook-headers` → `to-outgoing`                                                                                                                                                                                                                                  | P2                                 |
| Handler khác                                  | `decorate` (header/footer template, placeholders `$display_name`, `$listinfo_uri`, `$user_address`, `$user_delivered_to`, `$user_name`, `$user_options_uri`…), `file-recipients`, `replybot` (autoresponder)                                                                                     | P2 / P3                            |
| `mime-delete` chi tiết                        | filter/pass types & extensions, collapse alternatives, html→text, `filter_action` (discard/reject/forward_to_list_owner/preserve), giữ `Content-Disposition`                                                                                                                                     | P2                                 |
| `cook-headers` chi tiết                       | `Sender`, `Reply-To` policy, `X-Mailman-Version` → `X-Listmngr-Version`, `Precedence: list`, `X-Mailman-Rule-Hits/Misses`, `List-Id`, `X-Message-ID-Hash`                                                                                                                                        | P2                                 |
| `rfc-2369`                                    | `List-Help`, `List-Post`, `List-Subscribe`, `List-Unsubscribe`, `List-Archive`, `Archived-At`, **+ `List-Unsubscribe-Post: List-Unsubscribe=One-Click` (RFC 8058)**                                                                                                                              | P2                                 |
| Personalization                               | `personalize=full` → 1 msg/recipient, VERP luôn bật khi personalize                                                                                                                                                                                                                              | P2                                 |
| Custom pipeline per list (`posting_pipeline`) |                                                                                                                                                                                                                                                                                                  | P2                                 |

### 4.5 Core — Runners & queues

| Runner         | Phase | Ghi chú                          |
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
| `rest`         | –     | gộp vào HTTP server              |

### 4.6 Core — Email commands & sub-addresses

| Sub-address                                              | Xử lý                                                                                                                        | Phase |
|----------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------|-------|
| `list@`                                                  | posting                                                                                                                      | P2    |
| `list-owner@`                                            | owner pipeline → owners + moderators                                                                                         | P2    |
| `list-bounces@`, `list-bounces+VERP@`                    | bounce runner                                                                                                                | P3    |
| `list-request@`                                          | command bot: `confirm <tok>`, `join`/`subscribe [digest=…] [address=…]`, `leave`/`unsubscribe`, `help`, `echo`, `end`/`stop` | P3    |
| `list-join@`, `list-subscribe@`                          | = `join`                                                                                                                     | P3    |
| `list-leave@`, `list-unsubscribe@`                       | = `leave`                                                                                                                    | P3    |
| `list-confirm+token@`                                    | = `confirm` (Subject/To token)                                                                                               | P3    |
| Autoresponder cho owner/postings/requests + grace period |                                                                                                                              | P3    |
| `List-Unsubscribe-Post` one-click HTTP POST (RFC 8058)   | HTTP endpoint                                                                                                                | P2    |

### 4.7 Core — Bounce processing

| Tính năng                                                                                                                                                                                                                                                                                       | Phase |
|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|
| VERP encode/decode (`bounces+local=domain@host`), `verp_confirmations`, `verp_delivery_interval`                                                                                                                                                                                                | P2/P3 |
| Detectors (port từ `flufl.bounce`): DSN RFC 3464 (multipart/report), `aol`, `caiwireless`, `exchange`, `exim`, `groupwise`, `llnl`, `microsoft`, `netscape`, `postfix`, `qmail`, `sendmail`, `simplematch` (regex catalog), `simplewarning`, `sina`, `smtp32`, `yahoo`, `yale` + fixture corpus | P3    |
| `bounce_events` (context normal/relay/probe), `processed`                                                                                                                                                                                                                                       | P3    |
| Score tăng 1/ngày/địa chỉ, `bounce_info_stale_after` reset, threshold → `delivery_status=by_bounces`                                                                                                                                                                                            | P3    |
| Warnings (`bounce_you_are_disabled_warnings` × interval) → remove                                                                                                                                                                                                                               | P3    |
| Probe message (`-bounces+<token>`), probe bounce → disable ngay                                                                                                                                                                                                                                 | P3    |
| Owner notices: increment/disable/removal/unrecognized; `forward_unrecognized_bounces_to`                                                                                                                                                                                                        | P3    |
| Member re-enable (web/email confirm)                                                                                                                                                                                                                                                            | P4    |

### 4.8 Core — Digests

| Tính năng                                                                                                                 | Phase |
|---------------------------------------------------------------------------------------------------------------------------|-------|
| Append vào digest mbox per list; `digest_size_threshold` (KB) trigger; `digest_send_periodic` + `digest_volume_frequency` | P3    |
| MIME digest (multipart/digest, `multipart/mixed` masthead + TOC + messages + footer)                                      | P3    |
| Plaintext digest RFC 1153                                                                                                 | P3    |
| Summary digest (Mailman: = MIME với flag)                                                                                 | P3    |
| Volume/number bump, `listmngr digests --send/--bump/--periodic`                                                           | P3    |
| Templates: `list:member:digest:masthead/header/footer`                                                                    | P3    |
| Member `receive_list_copy`/`digest` delivery mode routing                                                                 | P3    |

### 4.9 Core — Templates (URIs)

Scope: site → domain → list; language fallback; loader `mailman:///` (built-in), `file:///`, `https://` (basic auth), DB body. Tên giữ nguyên Mailman:

| Nhóm         | Templates                                                                                                                                                                                                                                                                                                                                         | Phase                        |
|--------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------------------------|
| Admin        | `list:admin:action:post`, `list:admin:action:subscribe`, `list:admin:action:unsubscribe`, `list:admin:notice:disable`, `list:admin:notice:increment`, `list:admin:notice:removal`, `list:admin:notice:subscribe`, `list:admin:notice:unrecognized`, `list:admin:notice:unsubscribe`, `list:admin:notice:pending`                                  | P2–P3                        |
| Member       | `list:member:digest:footer`, `list:member:digest:header`, `list:member:digest:masthead`, `list:member:generic:footer`, `list:member:regular:footer`, `list:member:regular:header`                                                                                                                                                                 | P2–P3                        |
| User         | `list:user:action:invite`, `list:user:action:subscribe`, `list:user:action:unsubscribe`, `list:user:notice:goodbye`, `list:user:notice:hold`, `list:user:notice:no-more-today`, `list:user:notice:post`, `list:user:notice:probe`, `list:user:notice:refuse`, `list:user:notice:rejected`, `list:user:notice:warning`, `list:user:notice:welcome` | P3                           |
| Domain       | `domain:admin:notice:new-list`                                                                                                                                                                                                                                                                                                                    | P1                           |
| Placeholders | `$listname`, `$list_id`, `$display_name`, `$fqdn_listname`, `$list_domain`, `$description`, `$info`, `$request_email`, `$owner_email`, `$listinfo_uri`, `$list_requests`, `$user_email`, `$user_name`, `$user_delivered_to`, `$user_options_uri`, `$subject`, `$reasons`, `$confirm_email`, `$token`, `$sender_email`, `$moderator_notice`…       | –                            |
| i18n         | Templates + UI strings: `en`, `vi` ban đầu; fluent hoặc gettext `.po` import từ Mailman để tận dụng 40+ ngôn ngữ                                                                                                                                                                                                                                  | P3 (framework) / P6 (import) |

### 4.10 Core — REST API

Hai prefix: `/3.1/` (compat, JSON shape giống Mailman 3.3 để `mailmanclient` chạy) và `/api/v1/` (mới, OpenAPI, typed, pagination cursor, ETag). Cùng handler, khác serializer.

| Resource       | Endpoints                                                                                                                                                                                                                                                                                                 | Phase            |
|----------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------------|
| system         | `GET /system/versions`, `/system/configuration[/section]`, `/system/preferences`, `/system/pipelines`, `/system/chains`                                                                                                                                                                                   | P1               |
| domains        | `GET/POST /domains`, `GET/DELETE /domains/{host}`, `/domains/{host}/lists`, `/domains/{host}/owners`, `/domains/{host}/uris`                                                                                                                                                                              | P1               |
| lists          | `GET/POST /lists`, `GET/DELETE /lists/{id}`, `/lists/{id}/config[/{attr}]` (GET/PUT/PATCH), `/lists/styles`, `?advertised=true`, `/lists/{id}/archivers`, `/lists/{id}/digest` (GET/POST send/bump), `/lists/{id}/uris`, `/lists/{id}/templates`                                                          | P1 (config) / P3 |
| roster/members | `/lists/{id}/roster/{owner,moderator,member,nonmember}`, `/lists/{id}/member/{email}`, `POST /members` (subscribe, `pre_*`, `invitation`, `send_welcome_message`, `role`), `GET/PATCH/DELETE /members/{id}`, `/members/{id}/preferences`, `/members/{id}/all/preferences`, `POST /members/find`, mass ops | P1               |
| held           | `GET /lists/{id}/held[?count&page]`, `GET /lists/{id}/held/{id}`, `POST /lists/{id}/held/{id}` (`action=accept                                                                                                                                                                                            | reject           |
| requests       | `GET /lists/{id}/requests[?token_owner&request_type]`, `GET/POST /lists/{id}/requests/{token}` (`action=accept                                                                                                                                                                                            | reject           |
| header-matches | `GET/POST /lists/{id}/header-matches`, `GET/PATCH/PUT/DELETE /lists/{id}/header-matches/{n}`, `DELETE` all                                                                                                                                                                                                | P2               |
| bans           | `/bans`, `/lists/{id}/bans`, `DELETE …/{email}`                                                                                                                                                                                                                                                           | P3               |
| users          | `GET/POST /users`, `GET/PATCH/DELETE /users/{id}`, `/users/{id}/addresses` (GET/POST), `/users/{id}/preferences`, `POST /users/{id}/login` (verify pwd — compat), `/users/{id}/all/preferences`                                                                                                           | P1               |
| addresses      | `GET /addresses/{email}`, `POST …/verify`, `…/unverify`, `GET/POST /addresses/{email}/user`, `DELETE` unlink, `/addresses/{email}/memberships`, `/addresses/{email}/preferences`                                                                                                                          | P1               |
| queues         | `GET /queues`, `GET /queues/{name}`, `POST /queues/{name}` (inject)                                                                                                                                                                                                                                       | P2               |
| templates/uris | `/templates/{id}`, `/uris`, list/domain uris                                                                                                                                                                                                                                                              | P3               |
| owners         | `GET /owners` (server owners)                                                                                                                                                                                                                                                                             | P1               |
| plugins        | `GET /plugins` (trả rỗng/ list Rust plugins)                                                                                                                                                                                                                                                              | P6               |
| archive (mới)  | `/api/v1/archive/...` (search, threads, mbox export)                                                                                                                                                                                                                                                      | P5               |
| webhooks (mới) | `/api/v1/webhooks`                                                                                                                                                                                                                                                                                        | P6               |
| Auth           | `/3.1/`: Basic (khi `compat_basic_auth`, CIDR allowlist) hoặc Bearer; `/api/v1/`: Bearer scoped token                                                                                                                                                                                                     | P1               |
| OpenAPI        | `/openapi.json` + trang tham chiếu SSR tại `/api/docs` (không nhúng Swagger UI: không tải asset bên thứ ba, không script — P1-API-DOCS-ORIGIN)                                                                                                                                                                                                                                                              | P1               |

### 4.11 Core — CLI (`mailman` → `listmngr`)

| Mailman                                                            | listmngr                                                                                                                                                                                                                                        | Phase |
|--------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|
| `start/stop/restart/status`                                        | `listmngr serve` (+ systemd), `listmngr status`                                                                                                                                                                                                 | P0    |
| `conf`                                                             | `listmngr conf [--key]`                                                                                                                                                                                                                         | P0    |
| `version`                                                          | `listmngr version`                                                                                                                                                                                                                              | P0    |
| `info`                                                             | `listmngr info`                                                                                                                                                                                                                                 | P0    |
| `create`, `remove`, `lists`                                        | `listmngr lists create/remove/ls`                                                                                                                                                                                                               | P1    |
| `addmembers`, `delmembers`, `members`, `syncmembers`, `findmember` | `listmngr members add/del/ls/sync/find`                                                                                                                                                                                                         | P1    |
| `digests`                                                          | `listmngr digests --send/--bump/--periodic`                                                                                                                                                                                                     | P3    |
| `inject`, `qfile`, `unshunt`                                       | `listmngr queue inject/show/unshunt`                                                                                                                                                                                                            | P2    |
| `aliases`                                                          | `listmngr aliases regen`                                                                                                                                                                                                                        | P2    |
| `notify`                                                           | `listmngr notify` (pending moderation reminders)                                                                                                                                                                                                | P3    |
| `gatenews`                                                         | `listmngr nntp gate`                                                                                                                                                                                                                            | P6    |
| `import21`                                                         | `listmngr import21 <list> config.pck` + `archive import mbox`                                                                                                                                                                                   | P6    |
| `withlist`, `shell`                                                | `listmngr shell` (REPL nhỏ với repo ops) hoặc `listmngr eval --list …` — giảm scope                                                                                                                                                             | P6    |
| `reopen`                                                           | log rotation → không cần (tracing appender)                                                                                                                                                                                                     | –     |
| mới                                                                | `listmngr migrate`, `listmngr user create/passwd/2fa`, `listmngr token create/revoke`, `listmngr dkim gen/rotate/dns`, `listmngr archive import/export/reindex`, `listmngr import3`, `listmngr doctor` (kiểm tra DNS/MTA/DB), `listmngr backup` | P0–P6 |

### 4.12 Postorius — Web UI (admin + member)

| Màn hình                | Nội dung                                                                                                                                                                                                                         | Phase |
|-------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|
| Accounts                | signup + verify email, login, logout, password reset, change password, TOTP setup/recovery codes, passkeys (WebAuthn), OIDC (Google/GitHub/generic OIDC), sessions list/revoke, delete account                                   | P4    |
| Profile                 | addresses (add/verify/primary/remove), display name, locale, timezone, per-scope preferences (global / per address / per subscription), subscriptions list (đổi delivery, unsubscribe), API tokens                               | P4    |
| List index              | lọc advertised, theo domain, search, role badges                                                                                                                                                                                 | P4    |
| List summary            | description/info, subscribe form (anon → confirm), unsubscribe, link archive, member count, owners                                                                                                                               | P4    |
| List create             | chọn domain, style, owner, advertised, description                                                                                                                                                                               | P4    |
| List settings           | 9 nhóm §4.1, form theo nhóm, validate inline, diff preview + audit                                                                                                                                                               | P4    |
| Members                 | rosters (members/nonmembers/owners/moderators), search/paginate, per-member options (moderation_action, delivery, preferences), mass subscribe (textarea/file, pre_confirm/pre_approve/invite/welcome), mass removal, export CSV | P4    |
| Held messages           | list + preview (rendered + raw), reason, bulk accept/reject/discard, reject reason, forward, "moderate sender", "ban sender", "add header match"                                                                                 | P4    |
| Subscription requests   | pending confirm / pending approval, accept/reject/discard/defer + reason                                                                                                                                                         | P4    |
| Unsubscription requests |                                                                                                                                                                                                                                  | P4    |
| Header filters          | CRUD, order, regex test                                                                                                                                                                                                          | P4    |
| Templates               | per list/domain/site, editor + preview + placeholders help, language                                                                                                                                                             | P4    |
| Bans                    | list + global                                                                                                                                                                                                                    | P4    |
| Delete list             | confirm + archive policy option                                                                                                                                                                                                  | P4    |
| Domains                 | CRUD, owners, templates, DKIM keys + DNS record hiển thị                                                                                                                                                                         | P4    |
| Users admin             | list/search users, edit roles, force verify, impersonate?, subscriptions                                                                                                                                                         | P4    |
| System                  | versions, config view (masked), runner/queue status, MTA maps status, audit log viewer                                                                                                                                           | P4    |
| Bounce                  | member bounce info, re-enable, bounce log                                                                                                                                                                                        | P4    |

### 4.13 HyperKitty — Archive

| Tính năng                                                                                                                                                              | Phase | Thay đổi   |
|------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------|------------|
| Archiver handler nội bộ (`to-archive` → queue → index), không cần HTTP plugin                                                                                          | P5    |            |
| Remote HyperKitty archiver (POST `/api/mailman/archive` key) cho ai vẫn muốn HyperKitty                                                                                | P6    |            |
| Threading: `In-Reply-To` / `References` → parent; không có → thread mới; reattach thủ công                                                                             | P5    |            |
| Message-ID-Hash URL `/archives/list/{list_id}/message/{hash}/`, `Archived-At` header                                                                                   | P5    | compat URL |
| List overview: recent activity, active/popular threads, top posters, thread count, participants                                                                        | P5    |            |
| Thread list: latest, theo năm/tháng, unread indicator (last view)                                                                                                      | P5    |            |
| Thread page: messages, quote fold, attachments, votes ±1, tags, category, favorite, permalink, reply/new thread (web post → inject qua pipeline với address đã verify) | P5    |            |
| Sender pages, user profile (posts, votes, favorites, subscriptions)                                                                                                    | P5    |            |
| Search: list + toàn site, facets (list, sender, date), highlight                                                                                                       | P5    | tantivy    |
| Export mbox (thread/tháng/list, gzip)                                                                                                                                  | P5    |            |
| Import mbox (`listmngr archive import`) + reindex                                                                                                                      | P5    |            |
| Private archive: auth + membership check; `never` = không lưu                                                                                                          | P5    |            |
| Admin: delete message/thread, hide, reattach, category CRUD                                                                                                            | P5    |            |
| Rendering: `text`/`markdown` mode, link auto, emoji, gravatar (opt-in, proxied), email obfuscate                                                                       | P5    |            |
| RSS/Atom feed per list/thread                                                                                                                                          | P5    | mới        |
| Attachments serve an toàn (nosniff, download disposition, riêng path)                                                                                                  | P5    |            |

### 4.14 django-mailman3 / mailman-web

| Tính năng                                                                                                   | Phase |
|-------------------------------------------------------------------------------------------------------------|-------|
| Account = Mailman user (không cần sync 2 chiều)                                                             | P4    |
| Email addresses của account = addresses (verify chung)                                                      | P4    |
| Timezone/locale profile                                                                                     | P4    |
| Social login (allauth) → OIDC/OAuth2 providers                                                              | P4    |
| `mailman-web` cli (migrate, collectstatic, qcluster) → `listmngr migrate`, không cần collectstatic/qcluster | P0    |

### 4.15 Cải tiến (không có trong Mailman)

- DB-backed queue crash-safe, multi-node; không mất mail khi crash giữa chừng (Mailman qfiles rename dance).
- Scoped API tokens, audit log, webhook (events: message.held, member.subscribed, list.created…).
- DKIM key mgmt trong app (gen/rotate/DNS record), ARC seal, `Authentication-Results` ghi vào archive.
- RFC 8058 one-click unsubscribe.
- TOTP/WebAuthn/OIDC built-in, session mgmt.
- Prometheus metrics (queue depth, delivery latency, bounce rate), `listmngr doctor`.
- SQLite mode: 1 file, không Postgres, cho list nhỏ/dev.
- Dark mode, responsive, a11y, htmx partial updates, keyboard shortcuts cho moderation queue.
- GDPR: export/erase user data.
- OpenAPI + typed SDK sinh tự động (TS/Python) từ spec.

## 5. Security design

### 5.1 Auth & session

| Hạng mục                | Thiết kế                                                                                                                                                                                                                        |
|-------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Password                | Argon2id (m=64MiB, t=3, p=1), optional pepper (env), `zxcvbn` score ≥ 3, không giới hạn ký tự ngoài length ≤ 1024                                                                                                               |
| Lockout                 | progressive delay theo (account, IP); không lộ user tồn tại                                                                                                                                                                     |
| 2FA                     | TOTP (RFC 6238, drift ±1), 10 recovery codes hashed; WebAuthn passkeys (resident/non-resident); bắt buộc cho server_owner (config)                                                                                              |
| OIDC                    | Authorization Code + PKCE, `state`/`nonce`, email chỉ trust khi `email_verified=true`, không auto-link email chưa verify                                                                                                        |
| Session                 | `tower-sessions` DB store, cookie `__Host-lm_session`, `Secure; HttpOnly; SameSite=Lax`, rotate ID khi login/privilege change, idle 12h/absolute 7d, revoke all                                                                 |
| CSRF                    | double-submit token gắn session + kiểm `Sec-Fetch-Site`/`Origin`; htmx gửi `X-CSRF-Token` qua `hx-headers` global                                                                                                               |
| API token               | `lm_<id>_<secret>`; DB lưu SHA-256(secret); scopes: `system:read`, `lists:read`, `lists:write`, `members:read`, `members:write`, `moderation`, `users:write`, `archive:write`, `admin`; giới hạn list/domain; expiry; last_used |
| Basic compat            | chỉ khi `compat_basic_auth=true` + IP trong allowlist; user/pass = token id/secret                                                                                                                                              |
| Email confirm tokens    | 32 byte CSPRNG, base32 lower, lưu SHA-256, TTL `pending_request_life`, single-use, so sánh `subtle::ConstantTimeEq`                                                                                                             |
| `Approved:` posting key | Argon2 hash per list; strip header + dòng body; không dùng list password kiểu 2.1                                                                                                                                               |

### 5.2 Email

| Hạng mục         | Thiết kế                                                                                                                               |
|------------------|----------------------------------------------------------------------------------------------------------------------------------------|
| Inbound verify   | SPF/DKIM/DMARC qua `mail-auth` → `Authentication-Results` (trusted authserv-id), lưu kết quả vào msgdata; rule `validate-authenticity` |
| DMARC mitigation | tra `_dmarc.<from-domain>` và org domain (PSL), `p=reject/quarantine` (+ `sp=`, `pct=`) → munge_from / wrap / reject / discard         |
| Outbound DKIM    | sign `From/To/Subject/Date/List-*` + body; key per domain, RSA-2048 + Ed25519 (dual), rotate, DNS record UI                            |
| ARC              | seal khi list sửa nội dung (subject prefix/footer/munge) → giữ được deliverability                                                     |
| VERP             | luôn có sender riêng; bounce token HMAC để chống giả mạo bounce                                                                        |
| Loop/abuse       | `loop` rule, `X-Loop`, max hops, rate-limit per sender & per list (posts/hour, config)                                                 |
| Size/DoS         | reject tại LMTP nếu > `max_message_size` site-wide; MIME depth ≤ 20, parts ≤ 1000, header count ≤ 500; streaming write to store        |
| Attachments      | content-type sniff, block executable ext theo list config, serve `Content-Disposition: attachment` + `nosniff`                         |
| Address parse    | strict RFC 5322 + IDNA, reject control chars, normalize case cho match nhưng giữ `original_email`                                      |

### 5.3 Web

| Hạng mục      | Thiết kế                                                                                                                                                                     |
|---------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| CSP           | `default-src 'self'; script-src 'self' 'nonce-…'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'; form-action 'self'`; htmx `allowEval=false`, không `hx-on` |
| Headers       | HSTS (khi TLS), `X-Content-Type-Options`, `Referrer-Policy: strict-origin-when-cross-origin`, `Permissions-Policy`                                                           |
| HTML sanitize | `ammonia` allowlist cho archive/markdown; không remote image (proxy opt-in); link `rel="noopener nofollow ugc"`                                                              |
| Rate limit    | `governor` keyed IP/account: login, signup, reset, subscribe, search, API                                                                                                    |
| Proxy         | `trusted_proxies` CIDR mới tin `X-Forwarded-For`                                                                                                                             |
| Privacy       | `hide_address`, roster visibility, archive obfuscate `user at domain`, `anonymous_list`, robots noindex cho private                                                          |
| Upload        | mass subscribe file ≤ 5MB, parse streaming                                                                                                                                   |
| Errors        | không lộ stack; error id tương quan log                                                                                                                                      |

### 5.4 Data & ops

| Hạng mục     | Thiết kế                                                                                                                                                    |
|--------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Secrets      | `SecretString` + `zeroize`; không Debug/log; `*_file` config; DKIM private key + TOTP secret mã hoá at rest bằng `security.master_key` (XChaCha20-Poly1305) |
| Audit        | mọi thay đổi config/roles/moderation/user data → `audit_log` (actor, ip, diff); UI xem; export                                                              |
| Backup       | `listmngr backup` (pg_dump + message store) + restore doc                                                                                                   |
| Supply chain | `Cargo.lock` commit, `cargo deny` (advisories, licenses, dup), `cargo audit` CI, SBOM (CycloneDX), cosign sign image, Dependabot                            |
| Build        | `unsafe_code = forbid`, clippy `pedantic` + `nursery` (warn), `-D warnings` CI, `overflow-checks` release                                                   |
| Container    | static musl hoặc distroless, non-root uid 1000, read-only FS, `cap_drop ALL`, healthcheck                                                                   |
| systemd      | `ProtectSystem=strict`, `PrivateTmp`, `NoNewPrivileges`, `ProtectHome`, `SystemCallFilter`                                                                  |
| TLS          | ưu tiên reverse proxy; option `web.tls` rustls + ACME (phase 6)                                                                                             |
| Threat model | `docs/SECURITY.md`: assets, actors (anon, member, moderator, owner, server owner, MTA, attacker qua mail), STRIDE table, mitigations map                    |
| Disclosure   | `SECURITY.md` + `security.txt`                                                                                                                              |

## 6. UI/UX

### 6.1 Nguyên tắc

- SSR (askama) + htmx cho partial update (moderation queue, member table, search). Không SPA, không build step Node cho runtime.
- CSS thuần với design tokens (màu, spacing, type scale), dark mode qua `prefers-color-scheme` + toggle, responsive mobile-first.
- A11y: semantic HTML, focus ring, ARIA cho htmx swap (`aria-live`), contrast AA, keyboard (moderation: `a` accept, `r` reject, `d` discard, `j/k`).
- i18n UI strings (fluent), ngôn ngữ theo user pref → Accept-Language.
- Consistent layout: sidebar (list nav) + content; breadcrumbs; command palette (`/`) tìm list/user.

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
/moderation                     tổng hợp held + requests của mọi list mình quản
```

## 7. Roadmap

Effort tương đối: S < M < L < XL. A phase tag is created only after its acceptance gates pass and CHANGELOG/FEATURE_PARITY carry the evidence; a package version alone does not imply phase completion. ADR-0003 establishes `0.1.0` as the current unreleased Phase 1 development baseline, so the earlier mechanical `v0.<phase>.0` rule does not apply retroactively to Phase 0.

### 7.0 Quy ước thực thi

- **Một acceptance ID = một nhánh** `feat/<id-slug>` tách từ `main`; qua đủ
  gates trong CLAUDE.md (kể cả PostgreSQL và `scripts/test-mailmanclient.py`
  khi chạm REST) → merge `--no-ff` → xoá nhánh. Không xếp nhiều tính năng lên
  một nhánh.
- Mỗi work package dưới đây có ID `P<phase>-<TÊN>`; row ledger cùng ID ghi đúng
  lệnh và kết quả. Checkbox trong roadmap chỉ được tick khi row ledger tồn tại.
- **Stage ↔ Phase**: một số row ledger gọi work package theo chữ cái (A–F,
  ví dụ "B6", "C4", "D4", "F3") từ kế hoạch thực thi ngoài repo trước đây:
  A–C = Phase 2–3 (đã xong), D = Phase 4, E = Phase 5, F = Phase 6. Từ nay chỉ
  dùng ID `P<phase>-*`.
- **Web UI**: server-rendered theo ADR-0002 — askama + htmx 2.x vendored + CSS
  thuần, không Node runtime, không SPA. Câu chữ "Stage D SPA" trong ledger là
  trôi dạt và đã sửa; ADR-0004 ghi quyết định hoà giải và lộ trình chuyển các
  trang `format!` hiện có sang askama theo từng work package Phase 4.

### Phase 0 — Bootstrap (S)

Mục tiêu: workspace compile, chạy `serve` trả healthz, CI xanh. Checkbox indicates artifact presence only; formal completion follows the ID-based acceptance ledger in `FEATURE_PARITY.md`.

- [x] Workspace 9 crates (lib + bin), `[workspace.dependencies]`, lints, exact Rust toolchain, rustfmt/clippy/deny config
- [x] `listmngr-core`: `Config` (figment), `Error`, ids (`ListId`, `UserId`…), enums cơ bản
- [x] `listmngr-db`: pool init (pg/sqlite), `migrate!()` infra, migration `0000_init`
- [x] `listmngr` CLI: `version`, `conf`, `info`, `migrate`, `serve` (axum `/healthz`, `/readyz`, Prometheus `/metrics`), tracing init
- [x] **P0-08** CI GitHub Actions: blocking fmt, locked build, clippy `-D warnings`, tests, mandatory `TEST_POSTGRES_URL` migration/connectivity gate, deny, audit; cache
- [x] **P0-09** Multi-stage static-musl/scratch non-root Dockerfile, PostgreSQL Compose with runtime credentials, `.env.example`, `.dockerignore`, tool-free healthcheck
- [x] **P0-10** Hardened systemd unit with managed state/working directory, syscall filter, and kernel/device/filesystem protections
- [x] **P0-11** Actionable CLAUDE, README, ARCHITECTURE, FEATURE_PARITY, SECURITY, disclosure, deploy/MTA-boundary docs, and ADRs
- [x] **SEC-10** Supply-chain/deployment baseline: lockfile, pinned CI/action/tool/image inputs, deny/audit policy, full AGPLv3 text, least-privilege container/systemd artifacts
- Acceptance: `scripts/check-phase0-artifacts.sh`; `cargo build --locked --workspace`; `cargo test --locked --workspace --all-targets`; `TEST_POSTGRES_URL=… scripts/test-postgres.sh`; `docker compose --env-file .env -f deploy/docker-compose.yml up --build --wait` → host `/healthz` and `/readyz` 200. Artifact checkboxes do not assert these live gates passed; evidence/status is recorded in `docs/FEATURE_PARITY.md`.

### Phase 1 — Core domain, DB, REST cơ bản (M)

Mục tiêu: tạo domain/list/user/member qua REST + CLI; `mailmanclient` subset chạy.

- [x] Migrations: domains, users, credentials, addresses, mailing_lists (+settings cột), members, preferences, api_tokens, audit_log, header_matches, bans, templates, list_styles
- [x] Repos (sqlx): Domain, User, Address, List, Member, Preferences (layered resolve), Token, Audit
- [x] Styles: 3 built-in + apply khi create
- [x] REST `/3.1/`: system, domains, lists (+config GET/PUT/PATCH toàn bộ attr), styles, users, addresses, members (roster, subscribe pre_* only, preferences), owners, find
- [x] Auth: Bearer token + scopes; Basic compat (allowlist); rate-limit
- [x] `/api/v1/` cùng handler, OpenAPI (utoipa), trang tham chiếu SSR `/api/docs` phục vụ từ chính origin
- [x] CLI: `lists create/remove/ls`, `members add/del/ls/sync/find`, `user create/passwd`, `token create/revoke`, `domains add/rm/ls`
- [x] Audit ghi cho mọi write
- [x] Acceptance: integration tests on PostgreSQL + SQLite cover the Phase 1 CRUD contract; Python `mailmanclient==3.3.5` script passes create domain → list → subscribe → set config → roster; exhaustive differential test covers nullable preference layering. Exact evidence is recorded in `docs/FEATURE_PARITY.md`.

### Phase 2 — Mail path (L)

Mục tiêu: gửi thư vào list → member nhận; hold/accept qua REST.

Checkbox tick theo row ledger nêu bên cạnh; deviation của từng row vẫn có hiệu lực.

- [x] Message store (fs, db) + `messages` index + `Message-ID-Hash` — P2-STORE, P2-ARCHIVE-AUTHORITY (fs store là thư viện standalone; intake CLI/LMTP dùng DB; GC qua P3-TASK-RUNNER)
- [x] Queue (`queue_jobs`) + claim/backoff/shunt + runner supervisor + graceful shutdown — P2-QUEUE, P2-RUNTIME, P2-LEASE-HEARTBEAT, P2-DELIVERY-POLICY
- [x] LMTP server (RFC 2033: LHLO, MAIL, RCPT, DATA, RSET, NOOP, QUIT, PIPELINING, SIZE, 8BITMIME, per-recipient status), sub-address routing, early reject — P2-TRANSPORT, P2-LMTP-PARAMETERS
- [x] `in` runner + chains + 15 rules (§4.3 trừ news/digests) + header-match chain + DMARC lookup + `munge_from` — P2-CHAIN-ENGINE, P2-CHAIN-RULES, P2-DMARC-MUNGE, P2-VALIDATE-AUTHENTICITY, P2-HEADER-MATCHES-REST
- [x] Held messages: DB, notices (owner/user), REST held endpoints + actions — held REST, P2-TEMPLATES (hold notices), P3-MODERATOR-REJECTION-NOTICE, P2-HELD-FORWARD
- [x] Pipeline runner + handlers §4.4 (trừ to-usenet, arc-sign): mime-delete đầy đủ, decorate + placeholders, personalize, VERP — P2-PIPELINE-HANDLERS, P2-MIME-DELETE, P2-HANDLERS-DECORATE, P2-COOK-HEADERS, P2-PERSONALIZE-VERP
- [x] Templates engine (built-in `mailman:///` bodies port từ Mailman en) + loader DB/file/http — P2-TEMPLATES (`https://` chấp nhận nhưng không fetch trong transaction; xem deviation)
- [x] `out` runner: `mail-send`, chunk, TLS, DKIM sign (dkim_keys + CLI `dkim gen/dns`), `retry`, `virgin`, `bad` — P2-RECIPIENT-LIMIT, P2-STARTTLS, P2-SMTP-AUTH, P3-DKIM, P3-DKIM-BODY, P2-DELIVERY-POLICY
- [x] RFC 2369/8058 headers + HTTP one-click unsubscribe endpoint (token HMAC) — P2-COOK-HEADERS, P2-ONE-CLICK-UNSUBSCRIBE
- [x] Postfix/Exim map generation + `aliases regen`; docker-compose thêm postfix — P2-MTA-INTEGRATION, P2-MTA-MAPS
- [x] CLI `queue inject/show/unshunt`, `status` — P2-CLI, P2-RUNTIME (+ `queue recipients/resolve`)
- [x] Metrics: queue depth, deliveries, latency — P2-METRICS
- [x] REST `/queues` — P2-QUEUES-REST
- [x] **P2-HELD-FORWARD** (S): `forward=True&forward_to=…` trên `POST /lists/{id}/held/{id}` như Mailman/Postorius — bọc bản gốc `message/rfc822`, gửi từ `-bounces`, kết hợp mọi action kể cả `defer`; `moderation_log.forward_to` + audit.
- [x] **P2-E2E-ACCEPTANCE** (M): ma trận acceptance dưới đây trong `crates/cli/tests/mailpath_e2e.rs` (binary thật + SMTP sink): member delivery, nonmember hold, accept-once, headers (List-*, subject prefix, footer) + DKIM `dkim=pass` với key test, ban → notice từ chối, max-size → hold, `personalize=full` + `verp_personalized_deliveries` → N msg VERP + one-click, inject khi server tắt → giao đúng 1 lần, SIGKILL giữa out (relay giữ DATA) → không mất, không double-deliver. Kill giữa stage `in` không lập lịch được trong harness; lease expiry ở đó do contract repository bao phủ.
- Acceptance: e2e test harness = pg + smtp sink (Rust mock hoặc `mailhog`) → gửi qua LMTP → assert N member nhận, headers đúng (List-*, subject prefix, footer, DKIM verify pass với key test), held → accept → delivered; nonmember → hold; ban → reject DSN; max-size → hold; `personalize=full` → N msg riêng với VERP đúng; crash giữa pipeline → job không mất (kill -9 test).

### Phase 3 — Subscription, commands, bounces, digests (L)

Mục tiêu: parity Core hoàn chỉnh (trừ NNTP/DMARC wrap).

- [x] Subscription/unsubscription workflow state machine + pendings + tokens + policies + invitations + moderation requests + REST `requests` — P3-SUBSCRIPTION-POLICY, P3-SUBSCRIPTION-REQUESTS-REST, P3-ADMIN-SUBSCRIBE, P3-HTTP-CONFIRM-RECEIPT, P3-EMAIL-CONFIRM-RECEIPT
- [x] Email command runner: `confirm`, `join/subscribe`, `leave/unsubscribe`, `help`, `echo`, `end/stop`; `-request/-join/-leave/-confirm` routing; autoresponder + grace period; administrivia — P3-EMAIL-COMMANDS, P3-AUTORESPONDER, P3-HELP-REPLY
- [x] Welcome/goodbye/invite/hold/refuse/rejected/warning/probe notices; template scopes + language fallback; REST templates/uris; ban REST — P2-TEMPLATES, P3-WELCOME, P3-GOODBYE, P3-ADMIN-SUBSCRIBE (invite), P3-MODERATOR-REJECTION-NOTICE, P3-BOUNCE-INCREMENT-NOTICE, P3-BOUNCE-DISABLE-NOTICE, P3-BOUNCE-PROBES, P3-LIST-POSTING-BANS, P3-BAN-RESOURCE-GET, P3-SITE-BANS-REST
- [x] Bounce runner: VERP decode, DSN + ENVID, events/scoring/stale/disable/owner notices, forward unrecognized, detectors + fixture corpus, warnings + removal, probes — P3-BOUNCE-RUNNER, P3-BOUNCE-DETECTORS, P3-BOUNCE-PROBES, P3-BOUNCE-MAINTENANCE, P3-BOUNCE-SCHEDULER, P3-BOUNCE-INBOX, P3-BOUNCE-ACK, P3-DIRECT-BOUNCE-SCORE, P3-SMTP-BOUNCE-EVENT
- [x] Digest runner: durable collection (DB, not mbox), thresholds/periodic, MIME + RFC1153 builders, volume/number rollover, masthead/header/footer, CLI `digests`, REST `/lists/{id}/digest` — P3-DIGEST-SETTINGS, P3-DIGEST-REST
- [x] `task` runner: expire pendings/workflows, cleanup orphan messages (refcount), stale bounce reset — P3-TASK-RUNNER
- [x] `notify` (pending reminders), `admin_notify_mchanges`, `admin_immed_notify` (held posts) — P3-TASK-RUNNER, P3-ADMIN-NOTIFY-MCHANGES, P2-TEMPLATES
- [x] i18n framework (fluent) + `en`, `vi` — P3-I18N; import `.po` Mailman templates (tooling) → P6
- [x] **P3-DUAL-BACKEND-CI** (M): CI chạy suite SQLite (mọi test không ignore), gate PG chọn lọc (`scripts/test-postgres.sh`, 38) và **mọi** test `#[ignore]` PostgreSQL (`scripts/test-postgres-all.sh`, 53) trên cùng một server dùng một lần; từng test tự tạo/drop schema qua `listmngr_db::test_support::IsolatedSchema`, không cần DB rỗng riêng, không silent fallback.
- [ ] **P3-CLIENT-SUITE** (M): bộ test tương đương doctest `mailmanclient==3.3.5` trong `tests/compat/` chạy qua `scripts/test-mailmanclient.py`: domains, lists, settings 9 nhóm, members/roster/preferences, header matches, bans (list + site), held, requests, templates/uris, queues, users/addresses, digest counters. Hiện có: Phase 1 flow + held flow + header matches + site bans + queues listing + `admin_notify_mchanges`.
- [ ] **P3-DIGEST-SNAPSHOT** (S): snapshot RFC 1153/MIME (insta) so với output Mailman 3.3 từ fixture thật.
- [ ] **P3-SUBSCRIPTION-E2E** (S): confirm-token round-trip qua email trong e2e harness (`-join` → challenge → `-confirm` → member; `-leave` tương tự).
- Acceptance: full `mailmanclient` doctest-equivalent suite pass; bounce corpus detection ≥ flufl.bounce; digest snapshot tests (insta) so với Mailman output; subscription flow e2e qua email (confirm token round-trip).

### Phase 4 — Web UI, Postorius parity (L)

**Kiến trúc (ADR-0002, tái khẳng định bởi ADR-0004):** server-rendered. askama
(compile-time, auto-escape) + htmx 2.x vendored (progressive enhancement; mọi
form hoạt động không JS) + CSS thuần với design tokens + CSP strict có nonce,
không Node runtime, không SPA. Các trang hiện có (`crates/api/src/webui.rs`,
HTML dựng bằng `format!` qua `crates/web`) là lát tạm: từ P4-SHELL trở đi mọi
màn hình render qua askama; không giữ hai cách render song song sau khi
P4-SHELL merge.

Trạng thái: đã có login/password session (CSRF, exact Origin), list index có
lọc/badge + list create + list summary (P4-LIST-CREATE-INDEX),
subscribe/confirm, held review có scope, members/policy, settings lát nhỏ,
`/moderation` lát nhỏ, own postings, bounce recovery (rows `P4-WEB-*`,
P4-LIST-COPY, P4-BOUNCE-WEB-RECOVERY) — và từ P4-SHELL, tất cả render qua askama
với shell i18n en/vi, design tokens + dark mode, a11y baseline. Chưa có accounts
đầy đủ, 2FA/OIDC, admin đầy đủ.

Work packages, theo thứ tự; mỗi ID một nhánh:

| ID                   | Effort | Phạm vi                                                                                                                                                                                                                                                                                           | Acceptance riêng                                                                                |
|----------------------|--------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------|
| ~~P4-SHELL~~ (xong)  | M      | askama layout + design tokens + dark mode; security headers, rate limit, `trusted_proxies` (đã có từ trước); htmx vendored (hash pinned, không CDN, chưa trang nào nạp → chưa cần CSP nonce); i18n UI bằng Fluent (`en`, `vi`, cùng catalog `listmngr_i18n`); a11y baseline (landmarks, focus, skip link, `aria-current`); đã chuyển 20 route hiện có + one-click + archive compat sang template | ĐẠT: axe không critical/serious trên 4 trang; CSP không violation; `scripts/test-webui-browser.py` PASS không đổi hành vi |
| ~~P4-ACCOUNTS~~      | —      | **Đã tách** thành các ID dưới đây: một vertical = một nhánh. Lý do: signup/reset/addresses cần đường thư ngoài phạm vi list (outbound runner hiện đòi `context["list_id"]`), nên phải có P4-SITE-NOTICES trước; các phần còn lại không chạm mail và đi trước được. | (xem từng ID)                                                                                    |
| ~~P4-ACCOUNT-SESSIONS~~ (xong) | S | Liệt kê phiên trình duyệt của chính mình (id đục, thời điểm tạo/hết hạn, đánh dấu phiên hiện tại), kết thúc một phiên hoặc mọi phiên khác; mỗi write một audit event trong cùng transaction | ĐẠT: `crates/api/tests/webui/sessions.rs` + contract PostgreSQL trong `scripts/test-postgres.sh` |
| ~~P4-ACCOUNT-PROFILE~~ (xong) | S | display name, locale, timezone; locale của user thắng `Accept-Language` ở shell; một validator dùng chung cho form và REST user patch                                                                                                                       | ĐẠT: `crates/api/tests/webui/profile.rs` + contract PostgreSQL; Chromium đổi sang `vi` và về lại  |
| ~~P4-SITE-NOTICES~~ (xong) | M | Thư mức site (không thuộc list nào): envelope từ `site.site_owner`, outbound xử lý job không có `list_id`, template scope site (`site:user:action:verify`/`reset`, en+vi). Tiền đề cho signup/reset/addresses                                            | ĐẠT: `crates/runners/src/site_notice_tests.rs` — sink nhận `MAIL FROM:<>`, From = site owner, DKIM theo domain site, template site override, không binding list |
| ~~P4-ACCOUNT-SIGNUP~~ (xong) | M | signup + verify email (token băm SHA-256, một lần, 24h, 1 thư/địa chỉ/giờ), phản hồi không lộ tồn tại tài khoản, rate limit; công tắc `web.signup`                                                                                                        | ĐẠT: `crates/api/tests/webui/signup.rs` signup → verify → login + contract PostgreSQL; Chromium signup |
| ~~P4-ACCOUNT-RESET~~ (xong) | S | đặt lại mật khẩu qua token email (change password đã có); mọi phiên của tài khoản kết thúc                                                                                                                                                                   | ĐẠT: `crates/api/tests/webui/reset.rs` request → confirm → login mới + contract PostgreSQL; Chromium |
| ~~P4-ACCOUNT-ADDRESSES~~ (xong) | M | addresses add/verify/primary/remove; gỡ = unlink + quên verified, giữ membership; địa chỉ của tài khoản khác → im lặng                                                                                                                                       | ĐẠT: `crates/api/tests/webui/addresses.rs` thêm → verify → primary → gỡ + contract PostgreSQL; Chromium |
| ~~P4-ACCOUNT-TOKENS~~ (xong) | S | API token của user (tạo/liệt kê/thu hồi) trên UI; scope theo thẩm quyền: chủ list chỉ scope list gắn với list của mình, chủ máy chủ mọi scope không gắn                                                                                                  | ĐẠT: `crates/api/tests/webui/tokens.rs` + contract PostgreSQL; token chỉ hiện một lần; thu hồi tức thì; Chromium |
| ~~P4-ACCOUNT-DELETE~~ (xong) | S | xoá tài khoản (gốc erase cho P4-GDPR): xác nhận lại mật khẩu; membership/address/token/credential/session/domain-owner xoá cùng transaction; chủ máy chủ cuối cùng bị từ chối                                                                             | ĐẠT: `crates/api/tests/webui/delete_account.rs` + contract PostgreSQL; mỗi membership một audit `member.delete` |
| ~~P4-TOTP~~ (xong)   | S      | TOTP (RFC 6238, drift ±1, mỗi mã một lần) + 10 recovery code băm; QR SVG server-side; `require_2fa_for = ["server_owner"]` chặn trang quản trị/kiểm duyệt/mint token đến khi enrol                                                                                                        | ĐẠT: `crates/api/tests/webui/totp.rs` + contract PostgreSQL; Chromium enrol → login 2 bước → recovery code |
| ~~P4-WEBAUTHN~~ (xong) | M    | passkeys (`webauthn_rp` thuần Rust — không OpenSSL, giữ build musl), đăng ký/xoá key, đăng nhập không mật khẩu; passkey = yếu tố thứ hai; script first-party duy nhất `passkeys.js` dưới `script-src 'self'`                                                                        | ĐẠT: `crates/api/tests/webui/passkeys.rs` với soft authenticator ES256 + contract PostgreSQL; Chromium CDP virtual authenticator |
| ~~P4-OIDC~~ (xong)   | M      | OIDC generic (Google chạy qua discovery chuẩn; GitHub chỉ OAuth2 → không hỗ trợ); Authorization Code + PKCE S256, `state`/`nonce`, ID token verify RS256/ES256 qua JWKS; link/unlink dưới account (unlink cần mật khẩu, từ chối lối vào cuối); JIT account chỉ khi `email_verified`, mật khẩu ngẫu nhiên `usable=0`; login qua provider vẫn qua bước TOTP nếu đã bật | ĐẠT: `crates/api/tests/webui/oidc.rs` với mock provider (`mock_oidc.rs`) + contract PostgreSQL |
| ~~P4-LIST-SETTINGS~~ (xong) | L | 9 nhóm §4.1 mỗi nhóm một form trên cùng patch engine/validator của REST config; refusal inline theo field (400 giữ giá trị đã nhập); preview diff trước/sau không ghi; header rules CRUD/lên-xuống/test giá trị; bans list + site (`/web/admin/bans`); templates catalogue + editor + preview placeholders + ngôn ngữ; digest send/bump; archivers (ghi, hiệu lực khi P5-REMOTE-ARCHIVERS); delete list gõ lại id + nêu archive policy | ĐẠT: `crates/api/tests/webui/list_settings_groups.rs` + contract PostgreSQL; Chromium: preview/refusal/save, rule add-test-remove, ban, template preview, delete refused |
| ~~P4-MEMBERS~~ (xong) | L     | rosters 4 role (`?role=`), search/paginate: htmx swap `#roster` khi `HX-Request`, trang đầy đủ khi không; per-member options (moderation_action, display name, role, delivery mode/status, ack/hide/list copy/own posts/language với Inherit) + giá trị hiệu lực; bounce score + reset/re-enable; mass subscribe textarea/file (multipart) qua registrar workflow với `pre_*`/invitation, outcome từng địa chỉ; mass removal chọn/dán; export CSV (10k) | ĐẠT: `crates/api/tests/webui/members_admin.rs` (fragment vs full page) + contract PostgreSQL; Chromium: htmx swap giữ document, mass subscribe, options, bounce reset, export, removal |
| ~~P4-HELD-QUEUE~~ (xong) | M  | held list + preview (From/To/Date decode, text body, raw 64 KiB), decision từng bài + bulk (1 tx, skip bài đã quyết), reject reason → notice, forward_to (từ chối địa chỉ của list inline), moderate sender (member override / nonmember row), ban sender, link header rule prefill, `moderation.js` phím tắt j/k/a/r/d/h/s/?; requests queue accept/reject/discard/defer; index đếm held/requests | ĐẠT: `crates/api/tests/webui/held_queue.rs` + contract PostgreSQL; Chromium: sender policy, bulk discard, accept bằng phím, request accept |
| ~~P4-LIST-CREATE-INDEX~~ (xong) | S | list create (domain, style, owner, advertised, description) cho server owner/domain owner, một transaction gồm list + owner + audit; list index filters (search/domain/`show=all` cho list không advertised của mình, badge vai trò); list summary (địa chỉ, chính sách, vai trò, link settings) + subscribe (anon → confirm) | ĐẠT: `crates/api/tests/webui/list_create_index.rs` create → anon join request + contract PostgreSQL; Chromium: tạo list, id trùng từ chối inline, directory lọc, summary |
| ~~P4-DOMAINS-USERS~~ (xong) | M | domains index/add/delete (gõ lại host, chỉ khi rỗng) + owners add/remove + templates scope domain (cùng editor) + DKIM DNS record từ key đã cấu hình (không sinh key); users admin: search, display name + server owner (chặn hạ cấp owner cuối), force verify/unverify address, memberships | ĐẠT: `crates/api/tests/webui/domains_users.rs` + contract PostgreSQL; Chromium: thêm domain, owner add/remove, xoá từ chối rồi xoá, tìm và sửa tài khoản |
| ~~P4-SYSTEM~~ (xong) | S | versions + DB backend; config masked (`redacted_json`, dotted keys); queue depth theo state + oldest ready + runner giữ lease; MTA maps status (kind/directory/target/generation `current`); audit log viewer lọc action prefix + target, phân trang | ĐẠT: `crates/api/tests/webui/system.rs` + contract PostgreSQL; Chromium: system page, audit filter |
| ~~P4-MODERATION-CROSS~~ (xong) | S | `/moderation` tổng hợp cross-list: mọi held post + request của các list mình kiểm duyệt (macro dùng chung với queue từng list, cap 50/loại), quyết định quay về trang tổng hợp (`back=moderation`); không bulk cross-list | ĐẠT: `crates/api/tests/webui/moderation_cross.rs` + contract PostgreSQL; Chromium: trang tổng hợp 3 held + 1 request |
| ~~P4-GDPR~~ (xong)   | S      | export JSON (account, preferences, addresses, memberships, token metadata, domains owned, sessions count, audit events) cho chính mình và cho admin; erase qua trang admin (gõ lại địa chỉ, chặn owner cuối) và CLI `user export/erase`; không anonymise archive | ĐẠT: `crates/api/tests/webui/gdpr.rs` + contract PostgreSQL + `crates/cli/tests/gdpr.rs`; Chromium: export JSON, erase owner cuối bị từ chối |
| ~~P4-ACCEPTANCE~~ (xong) | M | journey Playwright ở viewport 390×844 (signup → verify → login → create list → subscribe+confirm → post held → accept → settings → logout) với bridge token/owner/post từ harness Rust; axe mỗi stop; Lighthouse a11y ≥ 95 trên 4 trang; CSP (harness cũ); job CI `browser` chạy cả hai harness với tool pin | ĐẠT: `chromium_acceptance_journey` + `scripts/test-webui-journey.py`; `.github/workflows/ci.yml` job `browser` |

- Acceptance: Playwright e2e (signup → create list → subscribe → post → moderate → settings), axe a11y không lỗi critical, CSP không violation, Lighthouse a11y ≥ 95, mobile viewport pass — **đạt** qua P4-ACCEPTANCE (ledger); "post" là held message do harness chèn, mail path có e2e riêng.

### Phase 5 — Archive, HyperKitty parity (L)

Trạng thái: `crates/archive` có parse, threading root ids, Message-ID-Hash tương
thích HyperKitty, `Archived-At`, archive đọc SSR cơ bản
(`/web/lists/{id}/archive`, export mbox ≤ 20 message) — P2-ARCHIVE-AUTHORITY,
P2-DSN-INSPECTION; P5-RENDER đã thêm sender/date/parent, cây thread, attachments
lưu riêng, render text/markdown an toàn, obfuscate, reattach, gravatar proxy.
P5-SEARCH đã thêm index tantivy, P5-UI các trang overview/threads/sender/feed, P5-INTERACTIONS votes/tags/categories/favourites, P5-WEB-POST đăng bài từ web, P5-MBOX import/export mbox. P5-ADMIN đã thêm trang quản trị archive và P5-REMOTE-ARCHIVERS ba archiver từ xa. P5-ACCEPTANCE đối chiếu hash và threading với HyperKitty thật; Phase 5 hoàn tất theo ledger. UI theo cùng kiến trúc Phase 4 (askama + htmx).

| ID                  | Effort | Phạm vi                                                                                                                                                                                                                                            | Acceptance riêng                                  |
|---------------------|--------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------|
| ~~P5-RENDER~~ (xong) | M | sender/date/parent indexed; thread tree (`threading::order`, 500 post); attachments lưu ở `archive_attachments`, serve path riêng + nosniff + attachment disposition + deny-list type; text (quote folding, linkify) và markdown subset an toàn (`pulldown-cmark` events → writer riêng); obfuscate cho khách; reattach của owner (audit); gravatar opt-in proxied + cache | ĐẠT: `crates/archive/tests/render_threading.rs` (snapshot mbox theo thuật toán HyperKitty, không phải import HyperKitty thật) + `crates/api/tests/webui/archive_render.rs` + contract PostgreSQL |
| ~~P5-SEARCH~~ (xong) | M | tantivy 0.26 index (list, hash, thread, subject, body, sender, date), lọc thread/date qua `search::Query` (chưa có facets trên trang), commit batching (100 thay đổi/2 s, luôn khi shutdown) trong archive runner, `listmngr archive reindex`; trang archive dùng index khi có, fallback LIKE | ĐẠT: `crates/archive/tests/search.rs` (+ bench 100k ignored, số liệu ở ledger), `crates/api/tests/webui/archive_search.rs` + contract PostgreSQL, `crates/cli/tests/archive_reindex.rs`, runner test |
| ~~P5-UI~~ (xong) | L | `/archive/overview` (tổng số, tháng, 10 thread mới nhất, 10 thread sôi nổi và 10 người đăng nhiều trong 30 ngày), `/archive/threads[/{y}/{m}]` (20/trang, badge `new` theo `archive_thread_views` của reader), `/archive/thread/{hash}` (ghi last-view), `/archive/senders/{sha256(email)}`, trang search có số kết quả + `<mark>` (chỉ trên text node), `/archive/feed.atom` và `/archive/feed.rss` | ĐẠT: `crates/api/tests/webui/archive_ui.rs` + contract PostgreSQL; axe/CSP qua harness Chromium (overview, threads, thread, sender, search) |
| ~~P5-INTERACTIONS~~ (xong) | S | votes ±1 (`archive_votes`, một phiếu/người/bài, rút lại), tags (`archive_tags`, chuẩn hoá, người gắn hoặc owner gỡ, trang `/archive/tags/{tag}`), categories (`archive_categories` + `archive_thread_categories`, owner đặt, trang `/archive/categories/{name}`), favorites (`archive_favorites`, trang `/archive/favorites`); last-view đã có từ P5-UI; vote/tag/category audit cùng transaction | ĐẠT: `crates/api/tests/webui/archive_interactions.rs` + contract PostgreSQL; harness Chromium (vote, tag, favourite) |
| ~~P5-WEB-POST~~ (xong) | M | `GET/POST /archive/post[?reply=hash]`: member có address đã verify soạn new thread/reply; server compose (`listmngr_mail::web_post::compose`, mail-builder) và inject vào queue `in` với context `web_post{user_id,address,reply}`; `in` runner `approve_web_post` = approved-like khi sender đúng address, không bị ban, member không bị moderate (defer/accept) | ĐẠT: `crates/api/tests/webui/archive_post.rs` + contract PostgreSQL; `crates/runners/src/web_post_tests.rs`; e2e `web_post_is_delivered_and_archived` (binary thật: web login → post → SMTP sink nhận → archive page hiện); harness Chromium |
| ~~P5-MBOX~~ (xong) | M | `listmngr archive import` (mboxrd, `.gz`, batch/transaction, skip theo hash, sinh `Message-ID` khi thiếu; 100k msg < 10 phút máy dev) + export mbox (cả kho/thread/tháng, gzip) ở CLI và `…/archive/export.mbox[.gz]` streaming; `archive_policy=never` chặn cả import lẫn export; private archive auth + membership check; export đi qua `publication` nên list ẩn danh vẫn ẩn tác giả | ĐẠT: `crates/archive/tests/mbox.rs` + `crates/cli/tests/archive_import.rs` (CLI import/export + benchmark 100k `#[ignore]`, số liệu ở ledger) + `crates/api/tests/webui/archive_export.rs` + contract PostgreSQL |
| ~~P5-ADMIN~~ (xong) | S | owner ẩn/hiện và xoá hẳn một bài hoặc cả thread (migration `0051` thêm `archive_messages.hidden_at`; mọi đường đọc lọc `hidden_at IS NULL`; xoá bài nối replies vào cha và re-root thread khi mất gốc, mang theo tag/category/favourite/last-view); reattach đã có từ P5-RENDER; category CRUD trên `…/archive/admin` (trả nợ P4-LIST-SETTINGS và P5-INTERACTIONS) | ĐẠT: `crates/api/tests/webui/archive_admin.rs` + contract PostgreSQL; harness Chromium (thêm/xoá category, ẩn rồi hiện lại một bài) |
| ~~P5-REMOTE-ARCHIVERS~~ (xong) | S | `[archive] archivers` (`mail_archive_address`, `mhonarc_command` dạng argv, `prototype_path`) + toggle `list_archivers` mới bật một archiver; `mail-archive` enqueue bản sao vào `Queue::Out` ngay trong transaction của `ArchiveRepo::complete` (chỉ list `public`), `mhonarc` pipe qua `tokio::process::Command` với `$listname`/`$hostname`/`$hash`, `prototype` ghi maildir `<root>/<list>/new/<hash>` (ghi `tmp/` rồi rename); bài bị ẩn hoặc `archive_policy=never` không chuyển tiếp | ĐẠT: `crates/runners/src/remote_archivers_tests.rs` + contract PostgreSQL |

| ~~P5-ACCEPTANCE~~ (xong) | S | gom bốn tiêu chí acceptance Phase 5: benchmark import 100k và search p95 (số ở ledger), `crates/archive/tests/hyperkitty_parity.rs` đối chiếu hash và threading với HyperKitty thật (`mailman-users@mailman3.org`, 2025-03, 138 message, fixture chỉ giữ header/hash/quan hệ); sửa divergence tìm được — reply tới cha vắng mặt tự làm root như HyperKitty (`resolve_thread` + `adopt_orphans` dùng chung cho runner và importer) | ĐẠT: `hyperkitty_parity.rs` (138/138 hash; 133/138 parent, 132/138 thread — phần còn lại là root/cha tháng 2 ngoài fixture) + `archive.rs::verify_orphans_adopted_by_a_late_parent` + contract PostgreSQL |

- Acceptance: import 100k msg mbox (Mailman list public) < 10 phút máy dev, search p95 < 100ms, URL hash trùng HyperKitty với cùng Message-ID, threading snapshot so với HyperKitty import cùng mbox — **đạt** qua P5-ACCEPTANCE (ledger); hash và threading đối chiếu với HyperKitty thật qua REST API, không phải import HyperKitty tại chỗ.

### Phase 6 — Advanced & migration (M)

- [ ] DMARC đầy đủ: `wrap_message`, reject/discard, unconditional, `dmarc_addresses`, PSL; ARC seal handler
- [ ] NNTP gateway: `to-usenet`, `nntp` runner, `gatenews`, `news-moderation`, watermark
- [ ] `import21` (`config.pck` via serde-pickle, member lists, mbox) ; `import3` (đọc trực tiếp Mailman 3 DB + HyperKitty DB → mapping; hoặc qua REST)
- [ ] `.po` import → 40+ ngôn ngữ templates
- [ ] Webhooks + deliveries retry; plugins (Rust trait registry: rules/handlers/archivers)
- [ ] Remote HyperKitty archiver, Exim snippets, built-in inbound SMTP (experimental), S3 message store
- [ ] `web.tls` + ACME, MySQL (cân nhắc)
- [ ] `doctor`, `backup/restore`
- Acceptance: migrate 1 site Mailman 2.1 thật + 1 site Mailman 3 thật → lists/members/settings/archive URL không đổi.

### Phase 7 — Hardening & 1.0 (M)

- [ ] Security review theo checklist §5 + external review nếu có; fuzz (LMTP parser, bounce detectors, VERP, template placeholders, mime-delete) `cargo-fuzz` 24h/target
- [ ] Load test: 10k members list, 1k posts/h; đo queue latency; tune `max_recipients`, pool
- [ ] Chaos: kill runner giữa job, DB restart, MTA down → không mất mail, không double-deliver (idempotency key per job)
- [ ] Docs site (mdBook): install, migrate, admin guide, API, ops runbook
- [ ] Release: cross-compile (linux amd64/arm64, macOS), deb/rpm, Docker multi-arch, Helm chart, SBOM + cosign, upgrade path & DB migration policy (backward-compatible N-1)
- [ ] 1.0 tag

## 8. Testing strategy

| Loại        | Công cụ                                                                                                  | Phạm vi                                                                                                                                                |
|-------------|----------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| Unit        | `cargo test`                                                                                             | rules, handlers (message in → out), VERP, hash, templates, preferences layering, bounce detectors (fixture corpus), digest builders (snapshot `insta`) |
| Property    | `proptest`                                                                                               | VERP encode/decode, address normalize, token, chunking                                                                                                 |
| Integration | `testcontainers` (pg), sqlite in-memory                                                                  | repos, REST (axum `oneshot`), migrations up/down, queue claim concurrency (N workers không double-claim)                                               |
| E2E mail    | harness: pg + LMTP client + SMTP sink (Rust)                                                             | flow §7 P2/P3                                                                                                                                          |
| E2E web     | Playwright (Node dev-only, CI)                                                                           | flows §7 P4/P5                                                                                                                                         |
| Compat      | Python `mailmanclient` test script trong CI (docker)                                                     | REST `/3.1/` shape                                                                                                                                     |
| Fuzz        | `cargo-fuzz`                                                                                             | parsers                                                                                                                                                |
| Security    | `cargo audit/deny`, ZAP baseline scan CI (P7), CSP report endpoint                                       |                                                                                                                                                        |
| Perf        | criterion (rules/pipeline), k6 (HTTP), custom (LMTP→SMTP throughput)                                     | P7                                                                                                                                                     |
| Test data   | corpus mbox public (Mailman/Python lists), flufl.bounce fixtures, DMARC DNS mock (hickory test resolver) |                                                                                                                                                        |

## 9. Migration từ Mailman

| Nguồn             | Cách                                                                                                              | Ghi chú                                                                                                                                                                              |
|-------------------|-------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Mailman 2.1       | `listmngr import21 <list_id> /path/config.pck` + `archive import --mbox`                                          | map settings 2.1 → 3 giống Mailman `import21` (bảng mapping trong `docs/MIGRATION.md`); password 2.1 (sha/plain) → bắt reset                                                         |
| Mailman 3 Core    | `listmngr import3 --db postgres://…` (hoặc `--rest URL`)                                                          | copy domains/lists/settings/members/preferences/templates/bans/header_matches/held/pendings; user passwords (passlib pbkdf2/sha512_crypt) → verify legacy on login rồi rehash Argon2 |
| HyperKitty        | `listmngr import3 --hyperkitty-db …`                                                                              | threads/votes/tags/categories/favorites; hoặc import mbox + rebuild (mất votes/tags)                                                                                                 |
| Postorius/allauth | user emails/social accounts → `users`/`addresses`/`user_oidc`                                                     |                                                                                                                                                                                      |
| URL compat        | `/archives/list/{list}/message/{hash}/` giữ nguyên; redirect map cho `/hyperkitty/…` và `/postorius/…` → path mới |                                                                                                                                                                                      |
| MTA               | thay transport LMTP port; alias/transport regen                                                                   |                                                                                                                                                                                      |
| Rollback          | song song: listmngr đọc DB riêng; giữ Mailman standby; cutover đổi transport map                                  |                                                                                                                                                                                      |

## 10. Quyết định cần chốt

| # | Câu hỏi                                     | Khuyến nghị                                                                                                                                                                      |
|---|---------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 1 | License                                     | **Đã chốt: AGPL-3.0-or-later**; xem ADR-0003 và `LICENSE`.                                                                                                                       |
| 2 | DB                                          | Postgres-first, SQLite hỗ trợ đầy đủ nhưng single-node. Query viết portable, dùng `sqlx::query` runtime + test cả 2 backend (không dùng macro compile-time để tránh 2 bộ query). |
| 3 | Frontend                                    | SSR askama + htmx (khuyến nghị) vs Leptos/Dioxus. SSR: đơn giản, CSP strict, không WASM bundle, dễ i18n.                                                                         |
| 4 | Tên                                         | crate prefix `listmngr-*`, binary `listmngr`, env `LISTMNGR__*`, header `X-Listmngr-*`. Có cần alias `X-Mailman-*` cho compat? (khuyến nghị: emit cả 2 trong P2, config tắt).    |
| 5 | REST compat depth                           | `/3.1/` đủ để `mailmanclient` chạy; không mô phỏng `/3.0/`.                                                                                                                      |
| 6 | Deploy target                               | Docker + systemd binary. Helm ở P7.                                                                                                                                              |
| 7 | Templates i18n                              | import `.po` Mailman (GPL) — cần xác nhận license cho phép nếu chọn non-GPL. Nếu không: chỉ `en`/`vi` ban đầu + cộng đồng dịch.                                                  |
| 8 | Built-in inbound SMTP                       | experimental P6, không phải mục tiêu chính.                                                                                                                                      |
| 9 | Python `mailmanclient` compat test trong CI | có (docker python), chi phí CI thấp, đảm bảo wire-compat.                                                                                                                        |

## 11. Rủi ro

| Rủi ro                               | Giảm thiểu                                                                                                  |
|--------------------------------------|-------------------------------------------------------------------------------------------------------------|
| Parity list quá dài, dễ sót          | `FEATURE_PARITY.md` là checklist sống, mỗi PR tick; đối chiếu bằng cách chạy `mailmanclient` test suite.    |
| Bounce detection heuristics khó port | dùng fixture corpus flufl.bounce làm oracle; ưu tiên DSN chuẩn, heuristics sau.                             |
| Deliverability (DKIM/ARC/DMARC sai)  | test với key thật + `mail-tester`, `dkimvalidator`; e2e verify bằng `mail-auth`.                            |
| SQLite + Postgres divergence         | test matrix cả 2; hạn chế SQL đặc thù, wrap ở repo layer.                                                   |
| `sqlx 0.9` / `askama 0.16` API mới   | pin version, ADR ghi lý do; upgrade theo phase.                                                             |
| Scope UI lớn                         | dùng component partial dùng lại; ưu tiên moderation & settings trước, "nice-to-have" (command palette) sau. |
| Import 2.1 pickle                    | `serde-pickle` + fixture `config.pck` thật; fallback: script Python export JSON.                            |
