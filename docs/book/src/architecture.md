# Architecture

One binary, `listmngr`, built from a workspace of crates: `core` (the
domain model and configuration), `db` (portable SQLx repositories for
SQLite and PostgreSQL, the migrations, the message store, backup),
`mail` (LMTP and SMTP, parsing, cooking, DKIM, ARC, bounces, templates),
`pipeline` (Mailman's rules, chains and handlers), `runners` (the
queues' processors: `in`, pipeline, `out`, bounces, digests, archive,
NNTP, webhooks, tasks), `archive` (threads, rendering, search, mbox),
`api` (the REST APIs and the web interface), `web` (templates),
`import` (Mailman 2.1 and 3), `plugins` and `cli`.

`listmngr serve` runs everything in one process: the web listeners, and
when the mail role is enabled, the LMTP acceptor and the runners, all
over one database pool. Messages are stored once by the SHA-256 of
their bytes; every queue is a table; every business write and its audit
event commit in one transaction.

`docs/ARCHITECTURE.md` in the repository is the full record, one
section per delivered behaviour.
