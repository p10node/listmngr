# Deployment artifacts

These artifacts cover Phase 0 deployment bootstrapping. They do **not** claim a Phase 2 mail path.

## Docker Compose

From the repository root:

```sh
cp .env.example .env
# Replace the disposable example password; .env must never be committed.
docker compose --env-file .env -f deploy/docker-compose.yml config
docker compose --env-file .env -f deploy/docker-compose.yml up -d --build --wait
curl --fail --show-error http://127.0.0.1:8000/healthz
curl --fail --show-error http://127.0.0.1:8000/readyz
docker compose --env-file .env -f deploy/docker-compose.yml down
```

Compose interpolates one `POSTGRES_PASSWORD` into both PostgreSQL and the application URL. The committed example is not a production secret. Use URL-escaped password characters or provide a complete protected URL through another deployment mechanism.

`down` preserves the database volume. The `-v` flag deletes it and is reserved for explicitly disposable acceptance projects.

The image is built from the repository root, produces a musl-linked release binary, and runs it in `scratch` as UID/GID 1000. Compose provides a read-only root filesystem, drops all capabilities, enables `no-new-privileges`, and supplies only a bounded `/tmp`. `listmngr status` is the in-image readiness probe, avoiding a shell/HTTP client.

The digest-pinned Rust Alpine builder omits C library headers. It installs exactly `musl=1.2.5-r12` and `musl-dev=1.2.5-r12` for `ring` and bundled SQLite; neither the package manager nor development headers enter the runtime image. These exact package versions fail closed if removed from the Alpine repository and must be upgraded together through review. Base-image pinning alone does not supply a usable C toolchain.

## systemd

Install the binary at `/usr/local/bin/listmngr`, configuration at `/etc/listmngr/listmngr.toml`, and optionally secrets at `/etc/listmngr/listmngr.env` (root-owned, mode `0600`). Create the service account without a login shell; `StateDirectory=listmngr` creates and owns `/var/lib/listmngr`.

```sh
sudo install -m 0644 deploy/systemd/listmngr.service /etc/systemd/system/listmngr.service
sudo systemd-analyze verify /etc/systemd/system/listmngr.service
sudo systemctl daemon-reload
sudo systemctl enable --now listmngr.service
```

The syscall filter uses systemd's `@system-service` allowlist. Verify it on the target Linux distribution; if a legitimate syscall is blocked, document the exact denial and narrowly amend the allowlist rather than removing hardening.

## MTA integration boundary

Postfix/Exim remains responsible for internet SMTP. The committed snippets document the planned hand-off, but all LMTP directives are disabled because the current Phase 0/1 binary does not listen on port 8024 and does not deliver mail. Enabling the snippets now would route mail into a dead endpoint.

Before Phase 2 activation, operators must verify all of the following:

1. `listmngr` has a tested LMTP listener on `127.0.0.1:8024` (or a deliberately secured private address).
2. Recipient/domain maps are generated atomically and refreshed with the MTA-specific command.
3. Unknown recipients fail closed without creating a backscatter/open-relay path.
4. End-to-end tests prove accepted, held, rejected, and delivered messages.
5. Firewall/MAC policy permits only the intended MTA-to-LMTP flow.

`deploy/postfix/main.cf` and `deploy/exim/listmngr.conf` contain commented Phase 2 examples and activation checks. Compose intentionally omits an MTA until that acceptance exists.
