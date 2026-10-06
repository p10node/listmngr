# listmngr

[![CI](https://github.com/p10node/listmngr/actions/workflows/ci.yml/badge.svg)](https://github.com/p10node/listmngr/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/p10node/listmngr)](https://github.com/p10node/listmngr/releases)
[![License: AGPL-3.0-or-later](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue.svg)](LICENSE)

listmngr is a mailing-list manager written in Rust that replaces GNU
Mailman 3 — core, Postorius and HyperKitty — with one static binary, one
database (SQLite or PostgreSQL) and one configuration file. It speaks LMTP
to the MTA in front of it and SMTP to the relay behind it, serves the web
interface and the archive itself, offers Mailman's REST API (`/3.1/`) to
the tools that already know it and its own (`/api/v1`) to the rest, and
imports a Mailman 2.1 or Mailman 3 site with its archive.

## Features

- **Lists.** Mailman's list settings, roles (owner, moderator, member,
  non-member), subscription policies, moderation and held messages, digests
  (RFC 1153 and MIME), topics, headers and footers, content filtering,
  DMARC mitigation, VERP, bounce processing, email commands.
- **Mail.** LMTP intake, a durable queue in the database, delivery to a
  relay with STARTTLS, DKIM (RSA and Ed25519) and ARC signing, SPF, DKIM and
  DMARC checks on incoming mail, generated Postfix and Exim maps, an
  optional inbound SMTP listener, loop and abuse rules, structure ceilings
  at the intake.
- **Web.** Sign-up and sign-in with passwords, TOTP, passkeys and OIDC;
  member self-service; moderation queues; list and site administration; an
  archive with threads, search, votes, tags and categories; posting from the
  web; data export and erasure.
- **APIs.** Mailman's `/3.1/` for `mailmanclient` and friends, `/api/v1`
  with OpenAPI, scoped tokens, webhooks for audit events, compile-time
  plugins.
- **Operations.** TLS on its own listener with ACME, a message store in the
  database, on disk or in S3, backup and restore across backends, `doctor`,
  Prometheus metrics, a task sweep, hardened systemd and container
  deployments, a fuzzed and reviewed code base.

Every behaviour above has a recorded acceptance in
[docs/FEATURE_PARITY.md](docs/FEATURE_PARITY.md).

## Install

The current release is `1.1.0`. A tagged release publishes, under
[Releases](https://github.com/p10node/listmngr/releases):

- a `.tar.gz` per platform (x86_64 and aarch64 Linux as static musl
  binaries, x86_64 and aarch64 macOS) with the binary, the licence, this
  README, `UPGRADE.md` and the systemd unit;
- a `.deb` and an `.rpm` for x86_64 Linux that install `/usr/bin/listmngr`,
  the unit (disabled), the `listmngr` service account, `/etc/listmngr` and
  `/var/lib/listmngr`;
- a container image, `ghcr.io/p10node/listmngr`, for `linux/amd64` and
  `linux/arm64`, signed with cosign;
- a Helm chart, [deploy/helm/listmngr](deploy/helm/listmngr);
- `SHA256SUMS` over everything, signed keyless with cosign, and a CycloneDX
  SBOM.

```sh
apt install ./listmngr_1.1.0-1_amd64.deb      # or: dnf install ./listmngr-1.1.0-1.x86_64.rpm
```

From source, with the Rust toolchain pinned in `rust-toolchain.toml`:

```sh
cargo build --locked --release -p listmngr
install -m 0755 target/release/listmngr /usr/bin/listmngr
```

## First run

```sh
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

The web interface listens on `127.0.0.1:8000` and the account signs in at
`/web/login`. Every setting has a default; `LISTMNGR__SECTION__KEY`
environment variables override the file. `listmngr status` probes the
running server, and `listmngr doctor` checks the database, the relay and
the mail domains' DNS without changing anything. Connecting Postfix or
Exim, TLS and ACME, PostgreSQL, the message store and the rest are in the
operator's book (below).

## Docker

`deploy/Dockerfile` builds the binary into a `scratch` image that runs as
UID 1000 with no shell; `deploy/docker-compose.yml` runs it with PostgreSQL
and Postfix. Build from the repository root, never from `deploy/`:

```sh
cp .env.example .env            # set POSTGRES_PASSWORD; never commit .env
docker compose --env-file .env -f deploy/docker-compose.yml config
docker compose --env-file .env -f deploy/docker-compose.yml up -d --build --wait
curl --fail http://127.0.0.1:8000/readyz
```

## Moving from Mailman

listmngr imports a Mailman 2.1 site from its list pickles, members and
archives, and a Mailman 3 site over its REST API or straight from its
database, HyperKitty's archive included. The procedure, what is mapped and
what is not, is the book's Migration chapter and
[docs/MIGRATION.md](docs/MIGRATION.md); the feature-by-feature comparison
is [docs/MAILMAN_REPLACEMENT.md](docs/MAILMAN_REPLACEMENT.md).

## Documentation

- **The operator's book**, [docs/book](docs/book) (mdBook; `mdbook serve
  docs/book`): install, configure (with a configuration reference generated
  from the source), connect the mail system, migration, operations,
  administration on the web, the API and webhooks, security, architecture.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crates, processes, data and
  deployment boundaries.
- [docs/PLAN.md](docs/PLAN.md): the product contract and roadmap, by phase
  and acceptance ID (in Vietnamese).
- [docs/FEATURE_PARITY.md](docs/FEATURE_PARITY.md): the ledger, one row per
  acceptance ID with the test or command that proves it;
  [docs/ACCEPTANCE_NOTES.md](docs/ACCEPTANCE_NOTES.md): the prose record
  behind each row.
- [docs/OPERATIONS.md](docs/OPERATIONS.md), [docs/UPGRADE.md](docs/UPGRADE.md),
  [docs/SECURITY.md](docs/SECURITY.md), [deploy/README.md](deploy/README.md)
  (Compose, systemd, Helm, the MTA snippets), [docs/adr](docs/adr) and
  [CHANGELOG.md](CHANGELOG.md).

## Development

You need the Rust toolchain in `rust-toolchain.toml` (rustup installs it on
first use), Docker with Compose v2 for the PostgreSQL path, and Python 3 for
the scripts. The gates below are the ones CI runs; all of them pass before a
change is merged.

```sh
scripts/check-phase0-artifacts.sh
cargo fmt --all --check
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo deny check
cargo audit --ignore RUSTSEC-2023-0071   # documented inactive SQLx/MySQL lock-only edge
python3 -m unittest discover -s scripts/tests
python3 scripts/check-production-crates.py
```

The workspace suite runs on SQLite. PostgreSQL is a separate, mandatory gate
against a disposable server; each test creates and drops its own schema, so
the server never has to be empty:

```sh
cp .env.example .env
docker compose --env-file .env -f deploy/docker-compose.yml up -d postgres
TEST_POSTGRES_URL='postgres://listmngr:<password-from-.env>@127.0.0.1:5432/listmngr' scripts/test-postgres.sh
TEST_POSTGRES_URL='postgres://listmngr:<password-from-.env>@127.0.0.1:5432/listmngr' scripts/test-postgres-all.sh
```

To run the server from the host against that database, export
`LISTMNGR__DATABASE__URL="$LISTMNGR_HOST_DATABASE_URL"` after sourcing
`.env` (the Rust process does not load `.env` itself), then
`cargo run --locked -p listmngr -- migrate` and `-- serve`. `127.0.0.1` is
right on the host; `postgres` is the Compose network name. The real-client
suite (`scripts/test-mailmanclient.py`, `mailmanclient==3.3.5`) and the
browser suite (`scripts/test-webui-browser.py`) start their own loopback
server on a fresh database; see
[docs/ACCEPTANCE_NOTES.md](docs/ACCEPTANCE_NOTES.md) for the recipes.

## Contributing

Issues and pull requests are welcome. The contributor rules are in
[CLAUDE.md](CLAUDE.md); in short:

- one acceptance ID per branch (`feat/<id-slug>` from `main`), a failing
  test first, merged with `--no-ff` once every gate above passes;
- a change in behaviour updates the ledger, the acceptance notes, the
  architecture document and the book in the same change;
- a present file or route is not evidence; record the passing command;
- never commit `.env`, production URLs, passwords, tokens or private keys,
  and never log them.

## Security

Report vulnerabilities privately through the repository's
[security advisory form](https://github.com/p10node/listmngr/security/advisories/new);
do not file public exploit details. The policy and threat model are
[docs/SECURITY.md](docs/SECURITY.md); the machine-readable policy is
[security.txt](security.txt). Only the latest `1.x` release receives fixes.

## License

listmngr is free software under the GNU Affero General Public License,
version 3 or later (`AGPL-3.0-or-later`). The full text is in
[LICENSE](LICENSE); the rationale is
[ADR-0003](docs/adr/0003-version-license.md). The version moves only with
a release, under the schema policy in [docs/UPGRADE.md](docs/UPGRADE.md).
