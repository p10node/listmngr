# The API and webhooks

## Tokens

`listmngr token create <user-id> <name> --scopes <scope,…>` prints a
token once; the database keeps its hash. Scopes bound what the token
may do (`system:read`, `lists:read`, `lists:write`, `members:read`,
`members:write`, `moderation`, `users:write`, `archive:write`,
`webhooks`, `admin`), and a token may be confined to one list or
domain. Send it as `Authorization: Bearer <token>`.

## `/api/v1`

The native API: lists and their configuration, members and their
preferences, domains, users, held messages and requests, templates,
bans, header matches, digests, queues, webhooks. `/openapi.json` is
the OpenAPI document and `/api/docs` renders it without a CDN. Every
read carries an `ETag`; every write is audited.

## `/3.1` — Mailman's API

The resources `mailmanclient` 3.3.5 uses, with Mailman's shapes and
status codes, so existing scripts and Postorius-era tooling keep
working. Basic authentication is accepted only with
`api.compat_basic_auth = true` and from `api.compat_basic_auth_allow`.

## Webhooks

A webhook subscribes to audit actions (`*`, `member.*`, `list.config`)
for the site or one list and receives each as a signed JSON `POST`
(`X-Listmngr-Signature: sha256=HMAC(secret, timestamp.body)`); targets
must be `https://` and public unless the lab switches say otherwise,
deliveries retry with backoff, and `listmngr webhooks` or the web pages
manage them. Set `[webhooks] enabled = true` and a
`signing_key_file`.
