# Security review against the design (`docs/PLAN.md` §5)

Status of each item of the security design as of 2026-10-01, with the
evidence (a test, a ledger row, a file) and what the review found. Every
finding marked **fixed** was closed in `P7-SECURITY-REVIEW` with a test;
every **open** item says what stands in for it and why it is not done.
No external review has taken place.

Legend: **done** as designed · **deviates** done another way, on purpose
· **open** not done · **fixed** closed by this review.

## 5.1 Auth and session

| Item | Status | Evidence | Review |
| --- | --- | --- | --- |
| Password: Argon2id m=64 MiB t=3 p=1, zxcvbn ≥ 3, length ≤ 1024 | done | `SecurityConfig` (`argon2.memory_kib` 65536, `password_min_score` 3); `crates/db/src/web_password*.rs` tests | No pepper (deviates): the hash is salted Argon2id in the database; a pepper would need a second secret to manage and rotate, and the threat it answers (an offline database copy) is answered by the parameters and the score. |
| Lockout: progressive delay by (account, IP), no user enumeration | done | `security.rate_limit.login` (`COUNT/WINDOW`), `webui_password.rs`; the lockout and recovery-lock tests under `crates/api/tests/webui`; one answer for an unknown and a wrong password | — |
| 2FA: TOTP ±1 step, 10 hashed recovery codes, passkeys, required for `server_owner` | done | `crates/db/src/totp.rs` (constant-time compare), `user_recovery_codes`, `web_passkeys.rs` (`webauthn_rp`), `security.require_2fa_for` | — |
| OIDC: code + PKCE, `state`/`nonce`, email trusted only when `email_verified` | done | `crates/api/src/oidc.rs` (`state`, `nonce` random; `email_verified` carried, `false` when absent); `webui_oidc.rs` links only verified addresses | — |
| Session: DB store, `__Host-` cookie, `Secure; HttpOnly; SameSite`, rotation, idle 12 h / absolute 7 d, revoke all | deviates | `web_sessions.rs` (rotation on login and privilege change, idle and absolute from `web.session_idle`/`session_absolute`, revoke all from the sessions page); cookie `listmngr_session; Path=/web; HttpOnly; SameSite=Strict`, `Secure` when the site is `https` | The name is not `__Host-`-prefixed because the cookie is scoped to `Path=/web` (the prefix requires `Path=/`); `SameSite=Strict` is stricter than the design's `Lax`. Accepted. |
| CSRF: double-submit token bound to the session + `Sec-Fetch-Site`/`Origin` | done | `WebSession::csrf` compared constant-time; every web `POST` checks the token and the origin (`crates/api/tests/webui.rs`, `phase0_security.rs`) | — |
| API token `lm_<id>_<secret>`, SHA-256 at rest, scopes, list/domain bound, expiry, last used | done | `crates/db/src/lib.rs` (`lm_{id}_{secret}`, `Sha256(secret)`, constant-time check), scopes incl. `webhooks`; `crates/api/tests/rest.rs` | — |
| Basic compat only with `compat_basic_auth=true` and an IP allowlist | done | `api.compat_basic_auth`, `compat_basic_auth_allow` (loopback by default), `crates/api/src/lib.rs`; `phase0_security.rs` proves the allowlist uses the socket peer, never `X-Forwarded-For` | — |
| Email confirm tokens: 32 bytes CSPRNG, base32, SHA-256 at rest, TTL, single use, constant-time | done | `workflows.rs` (`pending_request_life`), `P3-SUBSCRIPTION-E2E` (a spent token replayed) | — |
| `Approved:` posting key: Argon2 per list, header and body line stripped | done | `rules.rs` (`approved`), `handlers.rs` strips; the mail crate's tests | — |

## 5.2 Email

| Item | Status | Evidence | Review |
| --- | --- | --- | --- |
| Inbound SPF/DKIM/DMARC → `Authentication-Results`, `validate-authenticity` rule | done | `crates/mail/src/authenticity.rs`, the `Authentication and DMARC mitigation` ledger row | — |
| DMARC mitigation incl. org domain (PSL), `sp=`, `pct=` | done | `P6-DMARC-WRAP` and the Phase 2 row; five actions | — |
| Outbound DKIM: RSA-2048 + Ed25519 dual, rotation, DNS record UI | done (1.1) | `crates/mail/src/dkim.rs` (RSA PKCS#8/PKCS#1 and Ed25519 PKCS#8, decided by the key file; several selectors per domain sign side by side; `read_key_file` owner-only); the domain page lists every selector's record; `listmngr dkim gen|records|dns` and the rotation runbook in `docs/OPERATIONS.md` (`P8-DKIM-ED25519`) | Done since `P8-DKIM-ED25519`: Ed25519 (RFC 8463) signs beside RSA-2048, which stays the default `dkim gen` makes because receivers verify it universally; rotation is a new selector, `dkim dns` confirming publication, the old entry removed after the longest verification delay. |
| ARC when the list alters content | done | `P6-ARC-SEAL` | — |
| VERP with an HMAC bounce token | done | `verp.rs`, bounce probes with a hashed token (`bounce_probes.token_hash`) | — |
| Loop and abuse: `loop` rule, max hops, per-sender and per-list posting rate | done (1.1) | `rules.rs` `loop` (the list's own `List-Post` marker), `max-hops` (`[mta] max_received_hops`, 30, discards as a loop) and `posting-rate` (`[security] rate_limit.post`, per sender and list, holds; the `posting_rate` ledger written in the accept transaction) since `P8-ABUSE-RULES`; `max-recipients`, `emergency`, `member-moderation`; per-address request cooldowns for commands; `security.rate_limit.subscribe` | The hop cap and the per-sender rate are rules of the posting chain since `P8-ABUSE-RULES`; the rate is per sender on one list, which is the design's per-list limit as well (a list-wide cap on all senders is not a separate control: `emergency` is). |
| Size and DoS: LMTP reject over the size, MIME depth ≤ 20, parts ≤ 1000, headers ≤ 500, streaming to the store | done (1.1) | LMTP `max_message_bytes` from the site (`lmtp.rs`; the line and size limits are fuzzed by `lmtp_session`), list `max_message_size` (`max-size` rule); `[mta] max_header_count = 500`, `max_mime_parts = 1000`, `max_mime_depth = 20` measured by `listmngr_mail::structure` and enforced at the LMTP/SMTP intake (`554 5.6.0`, nothing stored) and by the news gateway (`P8-MIME-LIMITS`; `measure` is fuzzed in `mime_filter`); `DefaultBodyLimit` on web posts (1 MiB) and unsubscribe (1 KiB) | The ceilings are explicit and independent of the parser's since `P8-MIME-LIMITS`. A message is still held in memory once (not streamed), bounded by the LMTP size limit — accepted. |
| Attachments: sniffing, blocked extensions, `attachment` + `nosniff` | done | `mime_delete` (extensions, types), `webui_archive.rs` (`Content-Disposition: attachment`, `X-Content-Type-Options: nosniff`), the `P5-RENDER` deny-list | — |
| Address parsing: strict RFC 5322 + IDNA, no control characters, case-normalised match, `original_email` kept | done | `Address::new` (`is_control` refused, IDNA), `addresses.email` / `original_email` | — |

## 5.3 Web

| Item | Status | Evidence | Review |
| --- | --- | --- | --- |
| CSP `default-src 'self'`, nonce scripts, `frame-ancestors 'none'`, `form-action 'self'` | done (stricter) | `webui.rs` `security_headers` (`default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'`, and `script-src 'self'; connect-src 'self'` only on the passkey pages), `webui_archive.rs`, `api_docs.rs`, `unsubscribe.rs` | `script-src 'self'` without nonces because no inline script exists; htmx is served from `self`. |
| Headers: HSTS when TLS, `X-Content-Type-Options`, `Referrer-Policy`, `Permissions-Policy` | **fixed** | `nosniff`, `X-Frame-Options: DENY` and `Referrer-Policy: strict-origin` were there; HSTS and `Permissions-Policy` were not | **F1** `Strict-Transport-Security: max-age=31536000; includeSubDomains` is now set on every response the TLS listener serves, never on the plain one; **F2** `Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=(), usb=()` on every web page. Tests: `crates/cli/tests/web_tls.rs`, `crates/api/tests/webui.rs`. |
| HTML sanitising (`ammonia`), no remote images, `rel="noopener nofollow ugc"` | deviates | `crates/archive/src/render.rs`: posts are rendered to text and a markdown subset by a writer of our own (no HTML pass-through, so nothing to sanitise), links carry `rel`, remote images are never fetched (gravatar is proxied, opt-in) | `ammonia` is not needed: no user HTML reaches a page. |
| Rate limiting: login, signup, reset, subscribe, search, API | done | `security.rate_limit.{login,subscribe,api,api_pre_auth}`; signup and reset share the login limiter; search is a signed-in page under the API limiter | — |
| Proxy: `trusted_proxies` before `X-Forwarded-For` | done | `web.trusted_proxies` (loopback by default), `ConnectInfo` on both listeners | — |
| Privacy: `hide_address`, roster visibility, obfuscation, `anonymous_list`, `noindex` for private | done | `render::obfuscate`, roster policies, the `anonymous_list` handler; private archives answer only a session | — |
| Upload: mass subscribe ≤ 5 MB, streaming | done | `webui_members.rs` (`DefaultBodyLimit`, a cap on addresses per form) | — |
| Errors: no stack, correlation id | done | `ApiError` logs `correlation_id` and answers `request failed; quote the correlation_id`; the CLI prints `error[CATEGORY]: …; correlation=…` | — |

## 5.4 Data and operations

| Item | Status | Evidence | Review |
| --- | --- | --- | --- |
| Secrets: `SecretString` + `zeroize`, no Debug/log, `*_file`, DKIM key and TOTP secret encrypted at rest with `security.master_key` | done (1.1) / deviates | `SmtpAuthSecret` (no `Debug`, serialises as `[REDACTED]`, zeroed on drop), every secret has a `*_file` read owner-only (`read_secret_file`), `redacted_json`; `[security] master_key`/`master_key_file` and `listmngr_db::keyring` seal TOTP secrets at rest (ChaCha20-Poly1305 under an HKDF-derived key, the account id as associated data), `listmngr secrets status|encrypt|rewrap|new-key`, the doctor's `master_key` check (`P8-MASTER-KEY`) | Done for the secret the database must read back (TOTP); the plaintext lives in `Zeroizing` buffers. Deviates, on purpose: the DKIM and ARC keys, the webhook signing key and the S3 secret stay owner-only files rather than being wrapped under the master key — wrapping a file key with another file key on the same host adds no protection the file mode does not give. A site without a key keeps the 1.0 behaviour and the doctor says so. |
| Audit: every change audited with actor, ip, diff; UI; export | done | `record_tx_with_context` at over a hundred sites, the audit page, `P4-GDPR` export | — |
| Backup: `listmngr backup` + restore | done | `P6-BACKUP` (any backend to any backend, message bytes included) | — |
| Supply chain: lock, `cargo deny`, `cargo audit`, SBOM, cosign, Dependabot | partly / **fixed** | lock committed, `deny.toml`, `cargo audit` in CI; SBOM and cosign are `P7-RELEASE` | **F4** `.github/dependabot.yml` (cargo at `/` and `/fuzz`, github-actions, weekly) was missing and is added. |
| Build: `unsafe_code = forbid`, clippy pedantic + nursery, `-D warnings`, `overflow-checks` in release | **fixed** | `[workspace.lints]` forbid/pedantic/nursery, CI `-D warnings`; no `[profile.release]` existed | **F3** `[profile.release] overflow-checks = true` added (the rest of the release profile is `P7-RELEASE`). |
| Container: static musl or distroless, non-root 1000, read-only, `cap_drop ALL`, healthcheck | done | `deploy/Dockerfile` (`FROM scratch`, `USER 1000:1000`), `docker-compose.yml` (`read_only`, `cap_drop`, `no-new-privileges`), the healthcheck through `listmngr status` | — |
| systemd hardening | done | `deploy/systemd/listmngr.service`, checked by `scripts/check-phase0-artifacts.sh` | — |
| TLS: proxy first; `web.tls` rustls + ACME | done | `P6-WEB-TLS`, `P6-WEB-ACME` | — |
| Threat model in `docs/SECURITY.md` | done | assets, actors, STRIDE table, deployment controls | — |
| Disclosure: `SECURITY.md` + `security.txt` | done | both present | — |

## Beyond the checklist

| Concern | Finding |
| --- | --- |
| SSRF | Webhook targets (set by list owners) are `https`-only, resolved first, private ranges refused and the connection pinned (`P6-WEBHOOKS-DELIVER`). The HyperKitty archiver URL, the ACME directory and the S3 endpoint are operator configuration, not user input. |
| Open redirect | The `compat` redirects are built from a parsed `ListId`, a URL-encoded hash or an alphanumeric thread; no page takes a `next` parameter. |
| Path traversal | `fs` store keys are 64 lowercase hex digits by construction and checked again on read; backup and restore take an operator path and table names from the database catalogue, never from input. |
| Dynamic SQL | The only built statements are `IN ($1,…,$n)` placeholders and catalogue-derived table names (`backup.rs`, `blobs.rs`); every value is bound. |
| Timing | Tokens, CSRF, TOTP and API secrets compare with `subtle::ConstantTimeEq`; password checks run Argon2 for an unknown user as for a known one. |
| Logs | DSNs, passwords, tokens and key material never appear: errors carry categories and correlation ids; `redacted_json` strips every secret; the webhook signing key and the S3 secret are read from files. |
| Fuzzing | Eight parsers under libFuzzer (`P7-FUZZ`), no crash found in the recorded runs. |

## What remains open for 1.x

1. An external review.

Closed since: explicit MIME depth, part and header-count ceilings in the intake (`P8-MIME-LIMITS`, 1.1); the `Received:` hop cap and the per-sender posting rate as rules (`P8-ABUSE-RULES`, 1.1); Ed25519 DKIM beside RSA with `listmngr dkim gen|records|dns` and the rotation runbook (`P8-DKIM-ED25519`, 1.1); TOTP secrets sealed at rest under `security.master_key` with `listmngr secrets` and the doctor's `master_key` check (`P8-MASTER-KEY`, 1.1 — the DKIM/ARC file keys deliberately stay files).
