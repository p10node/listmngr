# listmngr

`listmngr` is a security-focused mailing-list manager written in Rust. The current package version is **0.1.0 (unreleased development)** and is licensed **AGPL-3.0-or-later**. Phase 0 bootstrap artifacts and a Phase 1 application surface exist, but release and full parity are not claimed until the acceptance evidence in [`docs/FEATURE_PARITY.md`](docs/FEATURE_PARITY.md) passes. The Phase 2 mail path is not implemented.

## Prerequisites

- Rust 1.88.0 (see `rust-toolchain.toml`)
- Docker with Compose v2 for the PostgreSQL/container path

## Locked build and local tests

```sh
cargo fmt --all --check
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
scripts/check-phase0-artifacts.sh
```

The clippy gate is intentionally blocking in CI. A local failure is not evidence that another gate failed.

## Run against PostgreSQL from the host

`.env` is **not loaded automatically** by the Rust process. The example contains only disposable local-development values:

```sh
cp .env.example .env
set -a
. ./.env
set +a
# Start only the database; the Rust process itself remains on the host.
docker compose --env-file .env -f deploy/docker-compose.yml up -d postgres
export LISTMNGR__DATABASE__URL="$LISTMNGR_HOST_DATABASE_URL"
cargo run --locked -p listmngr -- migrate
cargo run --locked -p listmngr -- serve
```

Then, from another shell:

```sh
curl --fail --show-error http://127.0.0.1:8000/healthz
curl --fail --show-error http://127.0.0.1:8000/readyz
```

`127.0.0.1` is correct for host-side commands. The hostname `postgres` is a Docker Compose network name and does not resolve on the host.

## Run with Docker Compose

Build context must be the repository root; the Compose file already resolves it correctly.

```sh
cp .env.example .env
# Replace POSTGRES_PASSWORD in .env; never commit .env.
docker compose --env-file .env -f deploy/docker-compose.yml config
docker compose --env-file .env -f deploy/docker-compose.yml up -d --build --wait
curl --fail --show-error http://127.0.0.1:8000/healthz
curl --fail --show-error http://127.0.0.1:8000/readyz
docker compose --env-file .env -f deploy/docker-compose.yml down -v
```

For a direct image build, use `docker build -f deploy/Dockerfile -t listmngr:dev .`; `docker build deploy` is invalid because it omits workspace manifests. The runtime image is a static musl binary in `scratch`, runs as UID/GID 1000, and uses `listmngr status` for its tool-free healthcheck.

## Configuration and deployment

Configuration is TOML plus `LISTMNGR__SECTION__KEY` environment overrides. Prefer `database.url_file` or a root-readable environment file in production; `conf` output redacts credentials. See:

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for crate, process, data, and deployment boundaries;
- [`deploy/README.md`](deploy/README.md) for Compose, systemd, and intentionally disabled MTA snippets;
- [`docs/SECURITY.md`](docs/SECURITY.md) and [`security.txt`](security.txt) for the threat model and private reporting path;
- [`docs/PLAN.md`](docs/PLAN.md) for the canonical roadmap and acceptance IDs.

## License and versioning

All workspace packages are version `0.1.0`; no released tag is implied. The project is licensed under GNU Affero General Public License v3 or later. The complete license is in [`LICENSE`](LICENSE); rationale is in ADR-0003.
