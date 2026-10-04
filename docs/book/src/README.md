# listmngr

listmngr is a mailing-list manager written in Rust that replaces GNU
Mailman 3 — core, Postorius and HyperKitty — with one binary, one
database (SQLite or PostgreSQL) and one configuration file. It speaks
LMTP to the MTA in front of it and SMTP to the relay behind it, serves
the web interface and the archive itself, offers Mailman's REST API
(`/3.1/`) to the tools that already know it and its own (`/api/v1`) to
the rest, and imports a Mailman 2.1 or Mailman 3 site with its archive.

This book is for the operator: how to install, configure and connect
it, how to move a site over, and what to watch once it runs. The
engineering record — every behaviour with the test that proves it — is
`docs/FEATURE_PARITY.md` in the repository, and the design is
`docs/PLAN.md` and `docs/ARCHITECTURE.md`.

## What it does

- **Lists**: Mailman's list settings, roles (owner, moderator, member,
  non-member), subscription policies, moderation, held messages, digests
  (RFC 1153 and MIME), topics, headers and footers, content filtering,
  DMARC mitigation, VERP, bounce processing, email commands.
- **Mail**: LMTP intake, a durable queue in the database, delivery to a
  relay with STARTTLS, DKIM and ARC signing, SPF/DKIM/DMARC checks on
  what comes in, Postfix and Exim maps generated for it, an optional
  inbound SMTP listener.
- **Web**: sign-up and sign-in with passwords, TOTP, passkeys and OIDC,
  member self-service, moderation queues, list and site administration,
  an archive with threads, search, votes, tags and categories, posting
  from the web, data export and erasure.
- **APIs**: Mailman's `/3.1/` for `mailmanclient` and friends,
  `/api/v1` with OpenAPI, scoped tokens, webhooks for audit events,
  compile-time plugins.
- **Operations**: TLS on its own listener with ACME, a message store in
  the database, on disk or in S3, backup and restore across backends,
  `doctor`, metrics, a task sweep, a fuzzed and reviewed code base.

## Status

`1.0.0` is the first release; its tag `v1.0.0` closes Phase 7 of the
plan. Every feature in this book has a bounded, recorded acceptance in
the repository's ledger.
