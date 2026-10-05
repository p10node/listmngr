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

`deploy/helm/listmngr` is the chart (one replica, `Recreate`; the mail
role and the runners are one process over one database). The
configuration file is a value, `config`; secrets with a plain key go in
`secrets` as their environment variable (`LISTMNGR__DATABASE__URL`, …);
secrets that only exist as files (DKIM and ARC keys, the webhooks
signing key) go in `secretFiles` and are copied at start into an
in-memory volume with mode `0400`, because a Secret volume's files are
group-readable under `fsGroup` and the binary refuses a secret file
others can read. `migrate` runs in an init container before every
start; the pod runs as UID 1000 with a read-only root, no capabilities
and the runtime seccomp profile; `/healthz` and `/readyz` are the
probes.

```sh
helm install lists deploy/helm/listmngr \
  --set secrets.LISTMNGR__DATABASE__URL='postgres://listmngr:…@postgresql:5432/listmngr' \
  --set-file 'secretFiles.dkim-example\.com\.pem=dkim.pem'
kubectl exec deploy/lists-listmngr -- listmngr --config /etc/listmngr/listmngr.toml \
  user create admin@example.com --display-name Admin --server-owner --password-stdin < password
```

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
