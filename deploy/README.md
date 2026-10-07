# Deployment artifacts

These artifacts cover deployment bootstrapping: the web/API service, PostgreSQL and a Postfix front MTA. Verified in containers against fixture maps; a deployed end-to-end mail acceptance remains the operator's gate.

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

With the example `.env`, Postfix listens on `127.0.0.1:2525`; set
`POSTFIX_SMTP_LISTEN=0.0.0.0:25` and `LISTMNGR_MAIL_HOSTNAME` for a real
deployment.

Compose interpolates one `POSTGRES_PASSWORD` into both PostgreSQL and the application URL. The committed example is not a production secret. Use URL-escaped password characters or provide a complete protected URL through another deployment mechanism.

`down` preserves the database volume. The `-v` flag deletes it and is reserved for explicitly disposable acceptance projects.

The image is built from the repository root, produces a musl-linked release binary, and runs it in `scratch` as UID/GID 1000. Compose provides a read-only root filesystem, drops all capabilities, enables `no-new-privileges`, and supplies only a bounded `/tmp`. `listmngr status` is the in-image readiness probe, avoiding a shell/HTTP client.

The digest-pinned Rust Alpine builder omits C library headers. It installs exactly `musl=1.2.5-r12` and `musl-dev=1.2.5-r12` for `ring` and bundled SQLite; neither the package manager nor development headers enter the runtime image. These exact package versions fail closed if removed from the Alpine repository and must be upgraded together through review. Base-image pinning alone does not supply a usable C toolchain.

## systemd

Install the binary at `/usr/bin/listmngr` (where the Debian and RPM packages put it; the unit's `ExecStart` names that path), configuration at `/etc/listmngr/listmngr.toml`, and optionally secrets at `/etc/listmngr/listmngr.env` (root-owned, mode `0600`). Create the service account without a login shell; `StateDirectory=listmngr` creates and owns `/var/lib/listmngr`.

```sh
sudo install -m 0644 deploy/systemd/listmngr.service /etc/systemd/system/listmngr.service
sudo systemd-analyze verify /etc/systemd/system/listmngr.service
sudo systemctl daemon-reload
sudo systemctl enable --now listmngr.service
```

The syscall filter uses systemd's `@system-service` allowlist. Verify it on the target Linux distribution; if a legitimate syscall is blocked, document the exact denial and narrowly amend the allowlist rather than removing hardening.

## Helm

[`helm/listmngr`](helm/listmngr) is the chart: one Deployment (one replica,
`Recreate`; the mail role and the runners are one process over one database),
its Service and a ServiceAccount of its own with no API token mounted, the
configuration file as a value (`config`), plain-key secrets as environment
variables in a Secret (`secrets`, or `existingSecret`), file-only secrets
copied at start into an in-memory volume with mode `0400` (`secretFiles`, or
`existingFilesSecret`), a PersistentVolumeClaim for the state directory, an
optional Ingress, and a `helm test` hook that asks the Service for `/healthz`
and `/readyz`. `values.schema.json` describes every value, so a misspelled or
mistyped key fails `helm install` instead of being ignored. The pod runs as
UID 1000 with a read-only root, no capabilities and the runtime seccomp
profile; `migrate` runs in an init container before every start. The image
is `scratch` — no shell, no `PATH` — so `kubectl exec` names the binary as
`/listmngr` (a bare `listmngr` fails with "executable file not found in
$PATH"; the chart's notes said that until the harness ran).

PostgreSQL is in the chart (`postgresql.enabled`, the default): a one-replica
StatefulSet from the image and digest Compose and CI use
(`postgres:17-alpine@sha256:18cfe3ef…`), a headless Service, its own volume
claim, `pg_isready` probes, UID/GID 70 with a read-only root and no
capabilities; the application's `LISTMNGR__DATABASE__URL` is assembled by the
chart from `postgresql.auth` and the Service name, and a `wait-db` init
container holds `migrate` until the server answers. `postgresql.auth.password`
is required — `helm install` fails early without it, like Compose's
`POSTGRES_PASSWORD:?` — or `postgresql.auth.existingSecret` names a Secret with
a `password` key. The password goes into the URL unescaped, so it is
letters, digits and `._~-` only (the schema refuses anything else); PostgreSQL
reads it at its first start, so a later `helm upgrade` with another value
changes the application's URL but not the server — rotate it in SQL first.
Changing a value under `secrets` or the database password replaces the
application pod (`checksum/secrets`). With `postgresql.enabled=false`,
`secrets.LISTMNGR__DATABASE__URL` (or an `existingSecret` carrying it) is
required: PostgreSQL elsewhere, or SQLite on the volume for a trial.
`networkPolicy.enabled` adds two NetworkPolicies: PostgreSQL accepts 5432 from
the application pod only; the application accepts its web port from
`networkPolicy.webFrom` and reaches DNS, PostgreSQL and `networkPolicy.extraEgress`.
The mail role still needs an MTA that reaches the LMTP Service
(`P10-HELM-MTA` in `docs/PLAN.md`).

```sh
helm lint --strict deploy/helm/listmngr
helm install lists deploy/helm/listmngr --wait \
  --set postgresql.auth.password="$(LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 32)" \
  --set-file 'secretFiles.dkim-example\.com\.pem=dkim.pem'
helm test lists --logs
kubectl exec -i deploy/lists-listmngr -c listmngr -- /listmngr --config /etc/listmngr/listmngr.toml \
  user create admin@example.com --display-name Admin --server-owner --password-stdin < password
```

`scripts/test-helm.sh` installs the chart on a disposable kind cluster from
an image built here and exercises it as an operator would: install with
`--wait` and the chart's PostgreSQL under a password made on the spot
(`--sqlite` installs with `postgresql.enabled=false` and SQLite on the volume
instead), `pg_isready` in the database pod, `helm test`, the in-image
`listmngr status` probe, the first server owner through `kubectl exec`,
`/healthz`, `/readyz` and `/web/login` through a port-forward, a configuration
change through `helm upgrade` that replaces the application pod
(`checksum/config`) while the account stays in PostgreSQL (`select count(*)
from users` is still 1), then uninstall. CI's `helm` job runs it on every
push. It needs Docker, kind, kubectl and helm; `--keep` leaves the cluster for
inspection and `--image NAME:TAG` skips the build. This is one kind node with
local-path volumes, not a production cluster: Ingress, TLS, the storage class
and mail delivery remain the operator's acceptance.

## REQUIRED outbound STARTTLS configuration

See [`starttls.example.toml`](starttls.example.toml) for a deliberately non-live
configuration fragment. Set `smtp_relay` to a numeric `IP:port` (IPv6 `[IP]:port`)
and `smtp_tls = "required"`. Set `smtp_tls_server_name` to the relay certificate's
DNS identity; if omitted, its SAN must match the relay IP. This field does not
change routing or EHLO. Public CA roots are built into the binary, including in
the scratch image; system CA bundles and OS keychains are not automatically used.

For a private relay, mount a PEM CA certificate bundle read-only and point
`smtp_tls_ca_file` at its path **inside the container** (for example a read-only
bind at `/etc/listmngr/relay-ca.pem`, readable by UID 1000). On systemd, place it
under `/etc/listmngr/`, readable by the service account but writable only by the
administrator. This is public CA material, never a relay private key. It adds
trusted roots rather than disabling verification or replacing public roots.
Restart after CA rotation; rebuild to update bundled public roots. No change to
read-only/non-root/capability/systemd hardening is necessary for this fragment.

Environment equivalents are `LISTMNGR__MTA__SMTP_TLS=required`,
`LISTMNGR__MTA__SMTP_TLS_SERVER_NAME` and `LISTMNGR__MTA__SMTP_TLS_CA_FILE`; explicitly
pass them to the process/container (Compose does not forward arbitrary variables).
Do not enable this against a relay that requires AUTH; AUTH and implicit TLS are
not implemented. Required mode never falls back; failures remain queue retries
without mailbox bounce events. LMTP remains plaintext and must stay isolated.
This is locally fixture-tested, not deployed Compose/systemd/MTA acceptance.

## MTA integration

Postfix/Exim remains responsible for internet SMTP; listmngr speaks LMTP in and
SMTP out. Compose now runs a Postfix front MTA (`deploy/postfix/Dockerfile`,
`deploy/postfix/main.cf`, `deploy/postfix/entrypoint.sh`) next to listmngr:

- listmngr runs with the mail role on (`LISTMNGR__MTA__ENABLED=true`), LMTP on
  the private network only, `incoming = postfix` and `map_directory =
  /var/lib/listmngr/mta` on the shared `mta-maps` volume, and Postfix as its
  `plaintext_trusted_relay` at the static address `172.28.0.25:25`.
- Postfix mounts the volume read-only, waits for the first generation, accepts
  mail for the list domains only (`relay_domains`, `relay_recipient_maps`,
  `transport_maps` on `current/`), hands it to `lmtp:[172.28.0.10]:8024`, relays
  from `mynetworks` (the Compose subnet) and reloads when `current` changes.
- `LISTMNGR_MAIL_HOSTNAME` is the hostname Postfix announces and listmngr's
  `local_hostname`; `POSTFIX_SMTP_LISTEN` binds port 25 (`0.0.0.0:25` for a
  real deployment, a loopback port for trials); `POSTFIX_RELAYHOST` names an
  optional smart host, otherwise Postfix delivers to MX directly. Postfix is the
  only service that keeps capabilities (the minimum `master` needs to bind port
  25 and switch to the `postfix` user); it is not read-only because of its spool.

Verify the shipped configurations without a deployment:

```sh
scripts/check-mta-configs.sh   # Docker: Postfix RCPT matrix and Exim `-bt` matrix on fixture maps
```

Before exposing port 25, operators must still verify:

1. DNS (MX, SPF for the announced hostname, DKIM signing keys) for every list domain.
2. Unknown recipients fail closed (the script's matrix) and relaying is refused from outside `mynetworks`.
3. End-to-end acceptance on the deployment host: accepted, held, rejected and delivered messages, restart during delivery.
4. Firewall/MAC policy permits only the intended MTA-to-LMTP flow; LMTP is plaintext.

`deploy/exim/listmngr.conf` is the equivalent Exim 4 router/transport set for a
host-installed Exim reading `[mta] incoming = "exim"` maps; see
[the MTA map runbook](../docs/POSTFIX_MAPS.md). Bounce processing and full
Mailman replacement acceptance remain open.
