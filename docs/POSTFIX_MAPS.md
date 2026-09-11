# Explicit Postfix map generations

`listmngr aliases regen --output DIRECTORY [--lmtp-target IP:PORT]` generates
Postfix lookup data from the current list identity snapshot. This is an
experimental deployment building block, **not permission to activate live mail**
and not full Phase 2/Mailman acceptance.

## Generate and inspect

Use your existing protected application configuration and an already migrated
database. The command does not migrate, change business records, start workers,
run `postmap`, alter Postfix configuration, or reload any service.

```sh
listmngr --config /etc/listmngr/listmngr.toml aliases regen \
  --output /var/lib/listmngr/postfix \
  --lmtp-target 127.0.0.1:8024
```

Stdout is one absolute `generation-UUID` directory path containing:

- `domains.regexp`: only domains that currently contain lists.
- `recipients.regexp`: exact supported recipient addresses, including hidden lists.
- `transport.regexp`: the same exact addresses mapped to `lmtp:[IP]:PORT`.

These are **regexp** maps, not `hash` databases or virtual-address rewrites.
Do not compile them with `postmap FILE`. Query with `postmap -q KEY regexp:FILE`.
Regexps are anchored and dots escaped; Postfix's default case-insensitive matching
is retained. No wildcard recipient/domain, subdomain, or plus-extension rule is
emitted. Regexp lookup is linear; large-site performance is not certified.

The destination defaults to `mta.lmtp_listen`. Specify `--lmtp-target` when the
listener binds a wildcard or Postfix connects from a different network namespace.
IPv4 and IPv6 socket literals are supported; wildcard/multicast addresses, port
zero and scoped IPv6 destinations are rejected. A syntactically valid target is
not a connectivity check. LMTP is plaintext and must remain behind the trusted MTA
on loopback or a deliberately secured private network.

## Supported addresses and fail-closed limits

For each list, the generator includes its exact posting address plus `-join`,
`-subscribe`, `-leave`, `-unsubscribe`, `-request`, bare `-confirm`, `-owner`,
and bare `-bounces`.
The generator and LMTP dispatcher share the command-suffix table. An exact list
named, for example, `team-join` still takes precedence over the `team` join command
at the dispatcher; deduplication does not change this rule.

The runtime does **not** implement `-bounces+VERP` or `-confirm+TOKEN`
commands, so the generator does not claim those addresses. An actual list with
a reserved suffix still has its ordinary exact posting address. Alias domains
are not emitted: the current LMTP lookup does not resolve them. Command parsing,
sender checks and subscription policy can still reject a message at DATA after
an address matches; matching is not a promise of message acceptance. `-owner`
uses experimental administrative forwarding to owners and moderators, not
subscriber posting. Automatic/list traffic is refused and empty/unsafe rosters
are shunted. Existing generations must be explicitly regenerated and reviewed
to include the new route; no map activation/reload occurs automatically.

Bare `-bounces` now retains null-sender/automatic reports in a durable, untrusted
inbox; `queue ls --queue bounces` and `queue show ID --raw` permit explicit
operator inspection. It does not forward, score or disable anyone; no automatic
consumer/cleanup is implemented. Regenerate and review maps explicitly to add
this route; do not activate a deployment as part of these fixture checks.

**Bounce processing remains a release blocker.** Outbound list mail has a
receiving inbox but no authenticated VERP/DSN processing pipeline. This map generator
must not be represented as a ready-to-cut-over replacement for a live Mailman
installation.

## Publication and regeneration

The operator must own and protect DIRECTORY and its parent path. All maps come
from one list SELECT, are sorted/deduplicated, written into a private staging
directory, and individually synced. The staging directory is synced, renamed to
a fresh generation on the same filesystem, and its parent synced before success.
No currently published generation is overwritten. Failed database reads publish
nothing; filesystem failure does not replace an old generation. A crash may
leave a hidden `.staging-*` directory, or a complete unselected generation. No
automatic activation or crash-cleanup process is implied.

The durability guarantee assumes the output directory and its ancestors already
exist durably. The CLI can create a missing directory tree, but does not sync
every newly created ancestor entry. A power loss can therefore lose that new
tree even after successful generation. Provision and durably establish the
protected output root separately when crash durability is required. No
power-loss test has been performed; directory rename atomicity and full-tree
crash durability are different guarantees.

Generation directories initially have private permissions (0700 on Unix), since
hidden list identities are sensitive. Explicitly grant the local Postfix account
only the traverse/read access it requires; do not make these files public or
serve the directory over HTTP. Keep old generations for controlled rollback.
Automatic garbage collection and automatic regeneration after REST/CLI list
mutations are not implemented. Rerun after creating/removing lists and inspect the
new generation before selecting it.

## Reviewed activation boundary

`deploy/postfix/main.cf` is a commented example, not an enabled configuration.
For a **dedicated relay-domain MTA**, reviewed configuration must point
`relay_domains`, `relay_recipient_maps`, and `transport_maps` at the three files
inside the **same explicit generation path**. `local_recipient_maps` is not the
recipient guard for relay domains. Preserve unknown-recipient checks, reject
unauthorized relay, and keep list domains out of `mydestination` and virtual
mailbox/alias domain classes. Disable recipient-delimiter fallback for this
bounded runtime; it does not accept arbitrary plus extensions.

Do not replace unrelated settings on a shared MTA with the example. Chroot paths,
file permissions, IPv6 availability, firewall/MAC policy and reload semantics
must be checked on the deployment host. Selecting a new generation is a separate
operator action: no atomic multi-process Postfix reload or zero-stale-map window
is claimed by atomic file publication. List removal after generation may leave a
stale MTA map until refreshed; the LMTP lookup still rejects the removed list.

Before any authorized cutover, run the full held/delivery/restart/unknown-recipient
and bounce acceptance against a disposable actual MTA deployment, plus release
security/backend gates. The source still lacks full transport authentication,
bounces, notices and other required product behaviors.

## Reproducible non-delivery checks

```sh
cargo test --locked -p listmngr --test aliases
POSTMAP_BIN=/usr/sbin/postmap cargo test --locked -p listmngr --test aliases \
  real_postfix_lookup_agrees_with_runtime_recipient_validation -- --exact --ignored
```

The second test requires an explicitly selected installed Postfix `postmap`.
It creates its own Postfix configuration and SQLite database, generates maps
through the real binary, then compares actual map lookup with the production
LMTP recipient handler. It does not start an SMTP/LMTP listener or Postfix daemon,
send email, read deployment credentials, or modify system configuration.
This proves lookup compatibility, **not SMTP-to-LMTP delivery or IPv6 connectivity**.
