# Install

listmngr is one static binary, `listmngr`, that is both the server
(`listmngr serve`) and the command line. It needs a database — SQLite
(a file; the default) or PostgreSQL — and, for mail, an MTA in front of
it and a relay behind it (see [Connect the mail system](mail.md)).

## The binary

A tagged release ships archives and packages (see
[Releases](release.md)): a `.tar.gz` per platform with the binary, the
licence and the systemd unit; a `.deb` and an `.rpm` for x86_64 Linux
that install `/usr/bin/listmngr`, the unit (disabled), the `listmngr`
service account, `/etc/listmngr` and `/var/lib/listmngr`; a container
image; a Helm chart. `SHA256SUMS` beside them is signed with cosign.

```sh
apt install ./listmngr_<version>-1_amd64.deb      # or: dnf install ./listmngr-<version>-1.x86_64.rpm
```

From source (Rust 1.88, see `rust-toolchain.toml`):

```sh
cargo build --locked --release -p listmngr
install -m 0755 target/release/listmngr /usr/bin/listmngr
```

The unit's `ExecStart` names `/usr/bin/listmngr`; a binary installed
elsewhere needs a drop-in (`systemctl edit listmngr`) that overrides it.

## First run

```sh
# A configuration file; everything has a default, see Configure.
install -d -m 0750 /etc/listmngr
cat > /etc/listmngr/listmngr.toml <<'TOML'
[site]
name = "Example Lists"
site_owner = "postmaster@example.com"
base_url = "https://lists.example.com"

[database]
url = "sqlite:///var/lib/listmngr/listmngr.db?mode=rwc"
TOML

listmngr --config /etc/listmngr/listmngr.toml migrate
listmngr --config /etc/listmngr/listmngr.toml user create admin@example.com \
    --display-name Admin --server-owner      # prompts for the password
listmngr --config /etc/listmngr/listmngr.toml domains add example.com
listmngr --config /etc/listmngr/listmngr.toml lists create dev.example.com --display-name Developers
listmngr --config /etc/listmngr/listmngr.toml serve
```

`serve` listens for the web on `web.listen` (`127.0.0.1:8000` by
default) and, once `[mta] enabled = true`, for LMTP on
`mta.lmtp_listen`. The account `user create` made signs in at `/web/login`
straight away: the operator vouches for the address, so it is verified
without a mail round trip. With the default
`security.require_2fa_for = ["server_owner"]` the account page then asks
for a second sign-in step — enrol one at `/web/account/totp` with an
authenticator app — before `/web/admin` and the moderation pages open; a
development site may set the option to `[]`. `listmngr status` probes the running server;
`listmngr doctor` checks the database, the relay and the mail domains'
DNS without changing anything.

## systemd

`deploy/systemd/listmngr.service` runs the binary as the `listmngr`
user under a hardened unit (`ProtectSystem=strict`, `PrivateTmp`,
`NoNewPrivileges`, a system-call filter, `StateDirectory=listmngr`).
Port 25, 443 or 587 need `CAP_NET_BIND_SERVICE` or a port redirect; the
unit grants no capability on purpose.

```sh
install -m 0644 deploy/systemd/listmngr.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now listmngr
```

## Docker

`deploy/Dockerfile` builds the binary on Alpine (musl) into a `scratch`
image that runs as UID 1000; `deploy/docker-compose.yml`
runs it with PostgreSQL and Postfix. Build from the repository root:

```sh
cp .env.example .env            # replace POSTGRES_PASSWORD; never commit .env
docker compose --env-file .env -f deploy/docker-compose.yml config
docker compose --env-file .env -f deploy/docker-compose.yml up -d --build --wait
curl --fail http://127.0.0.1:8000/readyz
```

## Helm

Every release publishes the chart as an OCI artifact,
`oci://ghcr.io/p10node/charts/listmngr`, signed keyless with cosign; the
same chart is `deploy/helm/listmngr` in the repository and the release's
`.tgz` is among its assets. A chart version deploys the binary of the
same version (chart `<version>` runs image `<version>`).

```sh
helm show values oci://ghcr.io/p10node/charts/listmngr --version <version>   # every value, with its comments
cosign verify ghcr.io/p10node/charts/listmngr:<version> \
  --certificate-identity-regexp 'github.com/p10node/listmngr' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

What it installs: one Deployment (one replica, `Recreate`; the mail
role and the runners are one process over one database), its Service
and a ServiceAccount of its own with no API token mounted, the
configuration file as the value `config`, plain-key secrets as
environment variables in a Secret (`secrets`, or `existingSecret`),
file-only secrets (DKIM and ARC keys, the webhooks signing key) in
`secretFiles` (or `existingFilesSecret`), copied at start into an
in-memory volume with mode `0400` because a Secret volume's files are
group-readable under `fsGroup` and the binary refuses a secret file
others can read, a volume for the state directory, an optional Ingress
and a `helm test` hook that asks the Service for `/healthz` and
`/readyz`. `migrate` runs in an init container before every start; the
pod runs as UID 1000 with a read-only root, no capabilities and the
runtime seccomp profile. `values.schema.json` refuses an unknown or
mistyped value at install. The image is `scratch` — no shell, no `PATH`
— so `kubectl exec` names the binary as `/listmngr`.

PostgreSQL is in the chart by default (`postgresql.enabled`): a
one-replica StatefulSet from the same pinned `postgres:17-alpine` image
Compose uses, with its own volume; the chart assembles the application's
`LISTMNGR__DATABASE__URL` from `postgresql.auth` and holds `migrate`
until the server answers. `postgresql.auth.password` is required (or
`postgresql.auth.existingSecret`, a Secret with a `password` key) and
must be URL-safe — letters, digits and `._~-`; PostgreSQL reads it at
its first start only. For a database elsewhere, set
`postgresql.enabled=false` and `secrets.LISTMNGR__DATABASE__URL`.

Mail comes with `mta.enabled` and `mta.hostname`: a Postfix sidecar in
the same pod (the image the release builds from `deploy/postfix`) reads
the maps listmngr publishes, hands mail to LMTP on loopback, relays the
outbound mail and delivers to MX or to `mta.relayhost`; a second
Service, `<release>-listmngr-smtp`, exposes port 25 as a `LoadBalancer`.
Point the list domains' MX at it and publish SPF and DKIM (the keys
through `secretFiles`); see [Connect the mail system](mail.md).

A first install with the chart's PostgreSQL, the Postfix sidecar and an
Ingress whose certificate cert-manager issues:

```yaml
# lists.yaml
config: |
  [site]
  name = "Example Lists"
  site_owner = "postmaster@example.com"
  base_url = "https://lists.example.com"

  [web]
  listen = "0.0.0.0:8000"
  trusted_proxies = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]
postgresql:
  auth:
    existingSecret: lists-postgresql   # a Secret with a URL-safe `password`
mta:
  enabled: true
  hostname: lists.example.com
ingress:
  enabled: true
  className: nginx
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt
  hosts:
    - host: lists.example.com
      paths:
        - path: /
          pathType: Prefix
  tls:
    - hosts: [lists.example.com]
      secretName: lists-tls
```

```sh
kubectl create secret generic lists-postgresql \
  --from-literal=password="$(LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 32)"
helm install lists oci://ghcr.io/p10node/charts/listmngr --version <version> -f lists.yaml --wait
helm test lists
kubectl exec -i deploy/lists-listmngr -c listmngr -- /listmngr --config /etc/listmngr/listmngr.toml \
  user create admin@example.com --display-name Admin --server-owner --password-stdin < password
```

The second step of "First run" above applies unchanged: the owner
enrols a second factor at `/web/account/totp` before `/web/admin` opens.
Upgrading is `listmngr backup` and `pg_dump` first, then `helm upgrade
lists oci://ghcr.io/p10node/charts/listmngr --version <new> -f lists.yaml
--wait` and `listmngr doctor`; `helm rollback` does not roll the schema
back (`docs/UPGRADE.md`). In the repository, `scripts/test-helm.sh`
installs the chart on a disposable kind cluster and exercises it end to
end — mail included, and with `--oci` from a registry it pushed to — and
CI runs it on every push.

## PostgreSQL

Point `database.url` (or `database.url_file`, a file only the service
user can read) at the server, run `listmngr migrate` once, and start.
Every schema change ships as a migration the binary applies itself;
`listmngr doctor` tells you when the database and the binary disagree.

## Upgrading

Stop the server, `listmngr backup <dir>`, install the new binary,
`listmngr migrate`, `listmngr doctor`, start. Migrations only add
within a minor series; a binary runs against exactly the ledger its
own migrations produce and refuses a database with a migration it does
not know, so going back is a restore of the backup into an empty
database migrated by the old binary. The policy and the steps are
`docs/UPGRADE.md` in the repository.
