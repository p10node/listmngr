# Architecture

This document describes the implemented Phase 0/1 shape. Phase 2 mail delivery and later roadmap components are explicitly outside the current runtime.

## Dependency direction

The intended direction is:

```text
cli -> api / web / runners -> archive -> pipeline -> mail / db -> core
```

Dependencies must not point back up the graph. The nine workspace crates have these responsibilities:

| Crate | Responsibility in the current tree |
|---|---|
| `listmngr-core` | Domain identifiers/types, configuration loading and redaction, shared errors |
| `listmngr-db` | SQLx pool, embedded migrations, repositories, transactional persistence/audit boundary |
| `listmngr-mail` | Mail-domain types/helpers; not an active LMTP/SMTP path in Phase 1 |
| `listmngr-pipeline` | Future processing boundary; no Phase 2 delivery claim |
| `listmngr-runners` | Future background-runner boundary; no Phase 2 worker claim |
| `listmngr-archive` | Future archive boundary; no Phase 5 archive claim |
| `listmngr-api` | Axum `/api/v1` and Mailman-compatible `/3.1` adapters, auth, OpenAPI |
| `listmngr-web` | Web adapter boundary; full administration UI is Phase 4 |
| `listmngr` (`cli`) | Binary commands, configuration bootstrap, migration, and HTTP process startup |

## Process and request model

`listmngr serve` runs one Tokio process. In the current phase it starts HTTP endpoints for liveness (`/healthz`), database-backed readiness (`/readyz`), metrics, API compatibility, and OpenAPI. HTTP adapters authenticate and authorize before calling repositories. Later LMTP listeners, queue runners, SMTP delivery, digest, bounce, and archive workers are not part of this process yet.

`/healthz` proves that the process can answer. `/readyz` proves database-backed readiness. `listmngr status` is an HTTP client of these two endpoints, in that order, not a second database connection. It uses `web.listen`, maps wildcard binds to IPv4/IPv6 loopback, disables environment proxies and redirects, and limits each request to two seconds. Exit codes distinguish transport failure (3), unhealthy HTTP status (4), and not-ready HTTP status (5). The same binary probe runs in the shell-free container. None of these signals is a substitute for end-to-end mail delivery evidence.

## Data and transaction boundaries

PostgreSQL is the production backend. SQLite is a single-node development/test backend. SQLx migrations are embedded in `listmngr-db` and applied by `listmngr migrate` or server startup. Repository statements use numbered parameters accepted by both backends, and the live PostgreSQL contract exercises repeated migration plus Domain/User/Address/List/Member/Preferences/Token/Audit behavior rather than connectivity alone. Business mutations, owned-row cleanup, and their audit records share one transaction. IDs and timestamps use portable textual forms at repository boundaries.

The same numbered-parameter rule applies to SQL in API authorization helpers. A dedicated PostgreSQL router test exercises list/domain-scoped user reads, preferences, addresses, and collection filtering under both prefixes, including forbidden-user controls; repository-only tests cannot establish this boundary's portability.

Persistent application state belongs in the configured database. For systemd, `/var/lib/listmngr` is created by `StateDirectory=listmngr` and is the working directory. The strict service filesystem permits no broad host writes. In Compose, the application root filesystem is read-only and only `/tmp` is an ephemeral, bounded tmpfs; PostgreSQL owns its named volume.

## Configuration and secret flow

Configuration layers are TOML and `LISTMNGR__SECTION__KEY` environment overrides. The process does not parse `.env`; operators must source an env file or let Compose/systemd load it. Host-side database URLs use `127.0.0.1`; Compose-side URLs use service DNS `postgres`. Production deployments should use `database.url_file` or a protected environment file rather than command-line credentials. Diagnostic configuration output must remain redacted.

CLI passwords enter through a hidden terminal prompt, stdin, or an inherited Unix descriptor, never an argv password option. The reader rejects input beyond the shared 1,024-byte password limit and removes at most one terminal line ending. CLI adapter failures are classified by typed errors, not substring matching, and expose stable categories plus correlation UUIDs. Secret-file read errors are generic even when UTF-8 decoding fails. Exact member lookup/deletion reuses core email/IDNA validation; bulk synchronization retains the newer role-scoped atomic repository implementation.

## Deployment topology

```text
host client -> 127.0.0.1:8000 -> listmngr (UID 1000, read-only scratch image)
                                      |
                                      +-> postgres:5432 (Compose private network)

internet mail -> Postfix/Exim --[Phase 2 LMTP, currently disabled]--> listmngr
```

The Docker builder uses a pinned Alpine Rust image and emits a musl-linked release binary. The final `scratch` image contains the binary and CA roots only, runs as numeric UID/GID 1000, drops capabilities in Compose, and does not need a shell or `curl`. The systemd alternative uses the same non-root trust boundary plus syscall, address-family, kernel, device, home, and filesystem restrictions.

The builder also pins `musl` and `musl-dev` to `1.2.5-r12`: the Rust base includes a compiler but not the C headers required by `ring` and bundled SQLite. Package tools/headers remain outside the runtime image. The real-client acceptance harness (`scripts/test-mailmanclient.py`) uses a separate loopback process and disposable SQLite database, while `scripts/test-postgres.sh` independently proves PostgreSQL CRUD and schema semantics. CI runs these behavioral probes in addition to Rust and anti-stub gates.

The real-client harness feeds its fixture password through stdin. The PostgreSQL-only gate uses `domains ls` after migration to check repository access; it deliberately does not invoke the HTTP `status` command without a running server. CLI network fixtures exercise both address families, unhealthy/not-ready/timeout outcomes, proxy isolation, and redirect rejection independently of a database.

## Security boundaries

Bearer tokens carry scopes and optional list/domain bounds. Mailman Basic compatibility is disabled by default, is accepted only below `/3.1`, and, when configured, trusts the actual socket peer CIDR—not forwarded headers. `/api/v1` accepts Bearer authentication and successful typed GET responses carry content-derived ETags; `/3.1` retains the Mailman-compatible JSON shape instead. Secrets are returned once and only digests persist. Reverse proxies must sanitize forwarding headers and terminate TLS according to the operator threat model. See `SECURITY.md` for threats and residual risks.
