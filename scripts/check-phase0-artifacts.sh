#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

fail() { printf 'phase0 contract: %s\n' "$*" >&2; exit 1; }
contains() { grep -Eq "$2" "$1" || fail "$1 must match: $2"; }
not_contains() { ! grep -Eq "$2" "$1" || fail "$1 must not match: $2"; }

[ -f .dockerignore ] || fail '.dockerignore is required'
contains .dockerignore '(^|/)target/?$'
contains .dockerignore '(^|/)\.git/?$'
contains .dockerignore '(^|/)\.env($|\*)'

contains .github/workflows/ci.yml 'cargo build --locked --workspace'
contains .github/workflows/ci.yml 'TEST_POSTGRES_URL'
contains .github/workflows/ci.yml 'scripts/test-postgres\.sh'
not_contains .github/workflows/ci.yml 'uses: [^#[:space:]]+@v[0-9]+'

contains deploy/Dockerfile '^FROM rust:[^ ]+-alpine[^ ]* AS builder$'
contains deploy/Dockerfile '^FROM scratch$'
contains deploy/Dockerfile 'cargo build --locked --release'
contains deploy/Dockerfile '^USER 1000:1000$'
not_contains deploy/Dockerfile '(curl|apt-get|apk add)'
contains deploy/Dockerfile 'HEALTHCHECK.*listmngr.*status'

contains deploy/docker-compose.yml 'POSTGRES_PASSWORD:.*POSTGRES_PASSWORD'
contains deploy/docker-compose.yml 'LISTMNGR__DATABASE__URL:.*POSTGRES_PASSWORD'
not_contains deploy/docker-compose.yml 'postgres://listmngr:\*\*\*'
not_contains .env.example 'postgres://listmngr:\*\*\*'

for directive in \
  'NoNewPrivileges=true' 'PrivateTmp=true' 'ProtectHome=true' \
  'ProtectSystem=strict' 'SystemCallFilter=@system-service' \
  'StateDirectory=listmngr' 'WorkingDirectory=/var/lib/listmngr' \
  'ProtectKernelTunables=true' 'ProtectKernelModules=true' \
  'ProtectControlGroups=true' 'PrivateDevices=true' 'RestrictSUIDSGID=true'
do
  contains deploy/systemd/listmngr.service "^${directive}$"
done

[ "$(wc -l < LICENSE | tr -d ' ')" -gt 500 ] || fail 'LICENSE must contain the full AGPLv3 text'
[ -f security.txt ] || fail 'security.txt is required'
for id in P0-08 P0-09 P0-10 P0-11 SEC-10; do
  grep -Rqs "$id" README.md CLAUDE.md docs deploy .github scripts security.txt || fail "missing acceptance traceability for $id"
done

contains deny.toml '^yanked = "deny"$'
contains deny.toml '^unmaintained = "workspace"$'
contains deny.toml '^allow-wildcard-paths = false$'
contains deny.toml '^unknown-registry = "deny"$'
contains deny.toml '^unknown-git = "deny"$'

printf 'phase0 artifact contracts: PASS\n'
