# Security policy and threat model

## Supported versions

`1.2.0` (tag `v1.2.0`, 2026-10-08) is the current release; `1.1.0` (tag `v1.1.0`, 2026-10-05) and `1.0.0` (tag `v1.0.0`, 2026-10-04, the first) came before. Security fixes land on `main` and are released as the next `1.2.x`; only the latest `1.x` release is supported. Earlier `0.1.0` development snapshots are not.

## Assets, actors, and trust boundaries

Assets include database and SMTP credentials, API token secrets, subscriber identity and preferences, list configuration, audit history, and service availability. Actors include anonymous clients, members, moderators, list/domain owners, server owners, the MTA, operators, reverse proxies, and hostile mail/web clients.

Primary boundaries are public HTTP to Axum, authenticated adapters to repositories, process to PostgreSQL, MTA to the future LMTP listener, and operator configuration to the process. Phase 2 mail processing is not yet an implemented boundary.

## STRIDE analysis

| Threat | Example | Present mitigation | Residual/operational requirement |
|---|---|---|---|
| Spoofing | Forged bearer/basic identity | SHA-256 token digest, constant-time comparison, expiry/revocation, peer CIDR checks | Terminate TLS; do not trust unsanitized forwarding headers |
| Tampering | Unaudited member/config write | Scope/resource checks and atomic business-write plus audit transaction | Protect DB credentials and backups; review migration privileges |
| Repudiation | Privileged action denied later | Structured append-oriented audit actor/target/diff model | Centralize/retain logs outside an attacker-controlled host |
| Information disclosure | DSN/password in output or image context | Config redaction, `.dockerignore`, no shell in runtime image | Never commit `.env`; use `url_file` or protected environment files |
| Denial of service | Repeated invalid auth or oversized input | Peer-keyed pre-auth limits and input bounds | Reverse-proxy limits and resource monitoring remain required |
| Elevation of privilege | Scoped token crosses list/domain | Resource-bound authorization checks | Regression tests must cover every new write endpoint |

## API rate limiting

API requests pass through two in-memory stages. Before credential parsing, `security.rate_limit.api_pre_auth` limits the actual TCP socket peer IP; `X-Forwarded-For` is deliberately ignored. After successful credential validation, `security.rate_limit.api` limits the stable account/token identity. If `api_pre_auth` is omitted, it inherits `api`. Values use a positive `COUNT/WINDOW` form with `s`, `min`, `hour`, or `day` windows and are validated when configuration loads. Rejections return HTTP 429 with an integer `Retry-After` header and occur before token usage timestamps, handlers, business writes, or audit writes at that stage.

These maps are node-local: replicas do not coordinate counters, and restarting a process clears its buckets. Deployments requiring a global limit must add a shared limiter or an edge/reverse-proxy policy; this process-local control must not be treated as multi-node abuse protection.

## Deployment controls

The Compose application uses a static musl binary in `scratch`, numeric UID/GID 1000, a read-only root filesystem, all capabilities dropped, `no-new-privileges`, and a bounded tmpfs. The healthcheck executes `listmngr status`; no shell or `curl` is installed. PostgreSQL credentials are supplied at runtime and must match the application URL.

The systemd unit creates `/var/lib/listmngr` with `StateDirectory`, fixes its working directory, clears capabilities, and restricts syscalls, address families, devices, kernel interfaces, home access, and host filesystem writes. Operators must validate the unit on their target Linux/systemd version before rollout and relax protections only with documented runtime evidence.

Base images, GitHub Actions, and CI tools are pinned. `Cargo.lock`, `cargo deny`, and `cargo audit` are blocking controls. Pinning reduces unintended drift but does not replace periodic reviewed upgrades.

### Temporary audit exception

`Cargo.lock` contains `rsa 0.9.10` through SQLx's disabled MySQL dependency graph. RUSTSEC-2023-0071 has no fixed release, while `cargo tree --locked --target all -i rsa` returns no active dependency path for this PostgreSQL/SQLite build. CI therefore runs `cargo audit --ignore RUSTSEC-2023-0071` with an inline explanation; all other advisories remain blocking, and `cargo deny check` evaluates the active graph without this exception. Remove the exception when SQLx no longer records the edge or a fixed `rsa` is available.

### The chart's Postfix sidecar

With `mta.enabled`, the Helm chart runs Postfix in the application pod as
root with eight capabilities (`CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `FSETID`,
`KILL`, `NET_BIND_SERVICE`, `SETGID`, `SETUID`), a writable root and an
emptyDir spool — the same set Compose grants it, because Postfix's master
binds port 25, switches to its own users and writes its queue. The
application container keeps UID 1000, a read-only root and no
capabilities; the two share only the state volume, read-only on the
Postfix side, and loopback (LMTP in, the plaintext relay out), which
never leaves the pod. `allowPrivilegeEscalation` stays false and the pod's
seccomp profile applies to both.

## Secret handling

- Do not put production secrets in `.env.example`, Compose YAML, command lines, issue reports, logs, or screenshots.
- `.env` is ignored from Docker context and must remain untracked.
- Prefer `database.url_file` or `/etc/listmngr/listmngr.env` with root ownership and mode `0600`.
- Rotate any credential suspected of exposure; redact it before attaching diagnostics.
- CLI user creation/password changes use a hidden prompt, `--password-stdin`, or Unix `--password-fd`; `--password VALUE` is rejected to prevent process-list/history disclosure. Password input is bounded to 1,024 bytes and malformed/oversized streams fail closed.
- Runtime CLI errors use typed, stable categories and correlation UUIDs instead of raw error chains or string-based classification. Secret-file I/O errors never include the underlying OS/decoder error or file contents.
- The unauthenticated `status` probe targets the configured local HTTP listener, bypasses environment proxies, does not follow redirects, and times out per request. Readiness is observed from the running service, not inferred from a separate successful database connection.

## Vulnerability reporting

Please use the repository's **private security advisory** form:

<https://github.com/p10node/listmngr/security/advisories/new>

Do not file public exploit details. Include the affected version/commit, minimal reproduction, impact, environment, and any suggested remediation. Do not include live credentials or personal subscriber data. Response timing depends on maintainer availability; no fixed SLA is promised while the project is unreleased. The machine-readable policy is at [`../security.txt`](../security.txt).
