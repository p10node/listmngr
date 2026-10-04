# Install

listmngr is one static binary, `listmngr`, that is both the server
(`listmngr serve`) and the command line. It needs a database — SQLite
(a file; the default) or PostgreSQL — and, for mail, an MTA in front of
it and a relay behind it (see [Connect the mail system](mail.md)).

## The binary

From source (Rust 1.88, see `rust-toolchain.toml`); release archives
and packages are produced by the release pipeline described in
[Releases](release.md) once a version is tagged:

```sh
cargo build --locked --release -p listmngr
install -m 0755 target/release/listmngr /usr/local/bin/listmngr
```

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
`mta.lmtp_listen`. `listmngr status` probes the running server;
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

## PostgreSQL

Point `database.url` (or `database.url_file`, a file only the service
user can read) at the server, run `listmngr migrate` once, and start.
Every schema change ships as a migration the binary applies itself;
`listmngr doctor` tells you when the database and the binary disagree.

## Upgrading

Stop the server, `listmngr backup <dir>` (see Operations), install the
new binary, `listmngr migrate`, start. Migrations only add; a binary
runs against the ledger it knows and refuses any other. To go back:
install the old binary, `listmngr restore <dir>` into an empty database
migrated by it.
