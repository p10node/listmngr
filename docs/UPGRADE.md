# Upgrading and the migration policy

## The rule

A release's binary runs against exactly the schema its own migrations
produce. `listmngr migrate` applies the migrations the database does
not have yet; `listmngr serve` does the same at start, so a new binary
over an old database upgrades itself. Neither accepts a database that
carries a migration the binary does not know: `migrate` and `serve`
stop with a migration error, and `listmngr doctor` reports
`schema_mismatch` (the applied migrations are compared one by one, by
version and checksum) or `schema_dirty` (a migration that did not
finish). A binary never runs against a newer schema than its own.

Within a minor series (`1.x`), a migration only **adds**: a table, a
column with a default, an index. No migration removes a column,
changes a column's meaning or rewrites rows so that the previous
release could not read them. That is what makes a rollback a restore
of the previous release's backup rather than a data conversion, and
what keeps a backup made by release N readable by release N+1 (the
restore applies release N's rows into N+1's schema, where every new
column has a default). A major release may break that rule and says
so in its notes.

## Upgrading one node

1. Read the release notes: a migration that needs time, a changed
   default or a removed setting is called out there.
2. `listmngr --config … backup /var/backups/listmngr/<date>` with the
   old binary still installed; then `listmngr --config … doctor` and
   keep its output.
3. Stop the service.
4. Install the new binary (package, archive or image). The
   configuration file is kept; a key the new binary does not know is
   ignored, so compare `listmngr conf` with the notes.
5. `listmngr --config … migrate`, then `listmngr --config … doctor`:
   `database` must be `ok` / `migration_ledger_current`.
6. Start the service; `listmngr status`.

## Rolling back

The previous binary will not start against the migrated database (it
does not know the new migrations), so a rollback is a restore:

1. Stop the service and install the previous binary.
2. Point it at an empty database (a new file for SQLite, a new
   database for PostgreSQL) and `listmngr --config … migrate` with the
   previous binary.
3. `listmngr --config … restore /var/backups/listmngr/<date>` — the
   backup made before the upgrade, by the same release.
4. `listmngr --config … doctor`, start the service.

Mail accepted between the backup and the rollback is in the newer
database only; export what matters from it (`listmngr backup` of the
newer database stays readable by the newer binary) before dropping
it.

## Containers and Helm

The image tag is the version, and so is the chart's: `helm upgrade
lists oci://ghcr.io/p10node/charts/listmngr --version <new> -f
values.yaml` moves both together (a chart installed from a checkout
deploys its `appVersion` unless `image.tag` says otherwise). The
upgrade runs `migrate` in an init container before the new pod starts,
with one replica and `Recreate`, so a rollout is the sequence above;
`helm rollback` alone does not roll the schema back — restore a backup
as above. Keep the database's own backups beside `listmngr backup`.

The chart's own PostgreSQL (`postgresql.enabled`) is pinned to one image
digest of major version 17; a chart release never changes that major on
its own, because PostgreSQL's data directory is tied to it. Moving to a
later major is an operator's step: `pg_dump` from the running
StatefulSet, change `postgresql.image`, delete the StatefulSet's volume
claim, `helm upgrade`, restore the dump, then `listmngr doctor`. The
database password is read at the server's first start; a changed
`postgresql.auth.password` changes the application's URL only, so
`ALTER ROLE … PASSWORD` first, then upgrade.

## The master key and restores

A backup carries sealed TOTP secrets as they are. Restore it on a node
configured with the same `security.master_key` (or `master_key_file`),
or `listmngr doctor` reports `master_key` as `fail` and no enrolled
account can complete its second step until the key is configured. The
key never travels in the backup; keep it where the backups' other
secrets are kept.
