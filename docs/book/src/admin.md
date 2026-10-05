# Administration on the web

The web interface lives under `/web`. What a signed-in person sees
depends on their roles; nothing is reachable that the role does not
grant, and every change is audited with the actor.

| Who | Where | What |
| --- | --- | --- |
| Anyone | `/web` | the directory of advertised lists; a public list's archive at `/web/lists/<list>/archive` |
| A member | `/web/account`, `/web/lists/<list>` | their addresses, password, TOTP, passkeys, sessions; per-list delivery mode and digest choice; export of their data; leaving |
| A moderator | `/web/lists/<list>/held`, `/web/lists/<list>/requests` | held posts and subscription requests, with the message, the sender's history and the reasons; accept, reject, discard, defer; cross-list queues under `/web/moderation` |
| An owner | `/web/lists/<list>/settings` | every list setting Mailman has, by section; the roster with mass subscribe and remove; bans and header matches; templates; the archive's administration; webhooks |
| A server owner | `/web/admin` | domains and their DKIM records, lists (create, copy, remove), accounts (export, erase), site-wide bans, webhooks, the audit log, the system page with versions and queues |

Sign-in accepts a password, a password with TOTP or a passkey, and
OIDC when a `[[web.oidc]]` table names a provider; `security.require_2fa_for`
makes a second factor mandatory for server owners. An account the operator
created at the console (`listmngr user create`) has its address verified
already; one created over the REST API or by signup must prove the mailbox
first. Sessions are rotated
on sign-in and privilege change and expire idle after `web.session_idle`
and in any case after `web.session_absolute`.

HyperKitty's and Postorius's URLs (`/archives/list/<address>/message/<hash>/`,
`/hyperkitty/…`, `/postorius/lists/<id>/`) redirect to the pages here,
so links from the old site keep working after a migration.
