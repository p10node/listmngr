{{#include ../../OPERATIONS.md}}

## The master key

`security.master_key` (or `master_key_file`) seals the TOTP secrets the
database holds; `listmngr secrets new-key` mints one, `secrets encrypt`
seals the rows a site already has, `secrets rewrap` moves them to a new
key, and `listmngr doctor` reports `master_key`. Keep the key with the
backups: a restore without it leaves every enrolled account unable to
pass its second step. The repository's `docs/OPERATIONS.md` has the steps.
