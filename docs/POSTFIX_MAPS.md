# MTA map generations (Postfix and Exim)

listmngr does what Mailman's `IMailTransportAgentAliases` does: it writes the
lookup data a front MTA needs to accept mail for list addresses and hand it
to LMTP. `[mta] incoming` selects the MTA, and maps are published

- at server startup,
- after every list creation or removal (REST, CLI), and
- on demand with `listmngr aliases regen`.

Each publication is an immutable `generation-<uuid7>` directory under
`[mta] map_directory`; the `current` symlink (relative, so another mount of
the directory resolves it) is switched to it afterwards, and generations
beyond `map_generations_kept` are pruned, oldest first. An MTA therefore never
reads a partial map, and a failed publish leaves the selected generation in
place.

## Configuration

```toml
[mta]
incoming = "postfix"                 # none | postfix | exim
map_directory = "data/mta"           # generations and the `current` symlink
lmtp_map_target = "127.0.0.1:8024"   # host:port as the MTA reaches LMTP; defaults to lmtp_listen
transport_file_type = "regex"        # Postfix: regex (read directly) | hash (compiled by postmap)
postmap_command = "/usr/sbin/postmap"
map_permissions = "group"            # owner (0700/0600) | group (0750/0640) | world (0755/0644)
map_generations_kept = 5
```

`lmtp_map_target` accepts an IP literal or a DNS name (a Compose service
name, for instance); wildcard, multicast and scoped addresses and port zero
are rejected, and a wildcard `lmtp_listen` needs an explicit target. Hidden
list identities are in the maps, so the default grants read access to the
service account's group only: add the MTA user to it (the Compose Postfix
image does this with a `listmngr` group of GID 1000) or choose `world`
deliberately.

## Generate and inspect

```sh
listmngr --config /etc/listmngr/listmngr.toml aliases regen \
  [--output DIRECTORY] [--lmtp-target HOST:PORT] [--mta postfix|exim]
```

Stdout is the absolute generation directory. Flags override the
configuration; with `incoming = "none"` the command writes Postfix maps. The
command reads lists with one SELECT and performs no business or audit write,
no migration, no MTA configuration change and no reload.

### Postfix, `transport_file_type = "regex"`

- `domains.regexp`: domains that currently contain lists → `OK`.
- `recipients.regexp`: every exact posting and command address (`-join`,
  `-subscribe`, `-leave`, `-unsubscribe`, `-request`, `-confirm`, `-owner`,
  `-bounces`) plus one VERP pattern per list
  (`list-bounces\+[^@=]+=[^@=]+@host`) → `OK`.
- `transport.regexp`: the same keys → `lmtp:[HOST]:PORT`.

Regexps are anchored, dots escaped, case-insensitive by Postfix default. No
wildcard domain, subdomain or other plus extension is emitted. Do not
`postmap` these files; query them with `postmap -q KEY regexp:FILE`. Postfix
reads a regexp table when a daemon opens it, so run `postfix reload` (the
Compose entrypoint does) after selecting a new generation. Regexp lookup is
linear; large-site performance is not certified.

### Postfix, `transport_file_type = "hash"`

Mailman's file names and rows: `postfix_domains` (`example.org example.org`)
and `postfix_lmtp` (`list@example.org lmtp:[HOST]:PORT`, one row per exact
address, no VERP rows), each compiled by `postmap_command` into `.db` inside
the staging directory before the generation is published; a failing `postmap`
publishes nothing. Use `hash:` tables with `recipient_delimiter = +` so
Postfix strips the VERP extension before the lookup, exactly as Mailman
documents. Alpine's Postfix ships without `hash` support (`lmdb` instead);
the Compose image therefore uses `regex`.

### Exim

- `exim_domains`: one domain per line (`lsearch`).
- `exim_recipients`: one exact address per line (`lsearch`).

`deploy/exim/listmngr.conf` carries the `listmngr_lists` manualroute router
(`local_part_suffix = +*`, optional; the condition accepts a suffix only on a
`-bounces` local part in the `+local=domain` shape), a `listmngr_unknown`
router that fails every other address in a list domain with
`no such list address`, and the `listmngr_lmtp` transport
(`protocol = lmtp`, `rcpt_include_affixes`). Add the domains file to
`domainlist relay_to_domains` so the default RCPT ACL verifies recipients
through these routers.

## Supported addresses and fail-closed limits

The generator and the LMTP dispatcher share the command-suffix table. An exact
list named `team-join` still takes precedence over the `team` join command at
the dispatcher; the maps contain the address once either way. A `-confirm+TOKEN`
extension is not a runtime address and is not advertised. Alias domains are not
emitted. Matching is not a promise of acceptance: sender checks and policy can
still refuse at DATA.

## Publication and regeneration

Maps are rendered from one list SELECT, sorted and deduplicated, written into a
private staging directory (each file synced, permissions applied), the staging
directory synced, renamed to a fresh generation on the same filesystem, the
`current` link swapped through a temporary link and rename, the parent synced,
then old generations pruned. Failed database reads publish nothing; filesystem
failure never replaces or unselects a published generation. A crash may leave a
hidden `.staging-*` directory. The durability guarantee assumes the output
directory and its ancestors already exist durably; the CLI can create a missing
tree but does not sync every new ancestor.

After a REST or CLI list change, a failed regeneration is logged (or printed to
stderr) and the list change stands; rerun `aliases regen`. At server startup an
unwritable map directory is a startup failure.

## Reviewed activation boundary

`deploy/postfix/main.cf` is the configuration of the Compose Postfix image
(`deploy/postfix/Dockerfile`): relay-domain MTA, no local delivery,
`relay_domains` / `relay_recipient_maps` / `transport_maps` on
`/var/lib/listmngr/mta/current/`, `recipient_delimiter` empty, relay only from
`mynetworks`. On a shared MTA copy those lines only; keep list domains out of
`mydestination` and the virtual domain classes. Selecting a generation and
reloading are operator actions outside Compose.

Before any authorized cutover, run the held/delivery/restart/unknown-recipient
and bounce acceptance against a disposable real MTA deployment plus the release
security gates.

## Reproducible checks

```sh
cargo test --locked -p listmngr-mail --test mta_maps
cargo test --locked -p listmngr --test aliases
POSTMAP_BIN=/usr/sbin/postmap cargo test --locked -p listmngr --test aliases -- --ignored
scripts/check-mta-configs.sh   # Docker: Postfix image RCPT matrix, Exim `-bt` matrix
```

The ignored tests need an explicitly selected installed `postmap`; they create
their own Postfix configuration and SQLite database and query (or compile and
query) fixture maps, without a daemon, mail or deployment credentials.
`scripts/check-mta-configs.sh` builds the Postfix image, runs it against a
fixture generation and checks a RCPT matrix from inside and outside
`mynetworks`, then routes the same matrix through the shipped Exim routers with
`exim -bt` in an Alpine container. Neither delivers to LMTP.
