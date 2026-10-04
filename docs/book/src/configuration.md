# Configure

Configuration is a TOML file (`--config`, or `LISTMNGR_CONFIG`) with an
environment override for every key: `LISTMNGR__SECTION__KEY`, sections
joined by `__` (`LISTMNGR__WEB__TLS__LISTEN` is `[web.tls] listen`).
Defaults are such that `listmngr serve` runs with no file at all on a
SQLite database under `data/`.

Secrets never go on a command line. Every secret has a `*_file`
variant — `database.url_file`, `webhooks.signing_key_file`,
`message_store.s3_secret_access_key_file`, `nntp.password_file`,
`mta.smtp_auth_password_file`, the DKIM and ARC `private_key_file` —
that names a file readable by the service user only; a file readable by
the group or others is refused at start.

```sh
listmngr conf                     # every effective value, secrets redacted
listmngr conf --key web.listen    # one value
listmngr info                     # the backends and listeners in one line each
```

The configuration is checked when it loads: an address that is not
`host:port`, a TLS section with only a certificate, an S3 store without
a bucket — each stops the start with a message that names the key and
never the value. A key the binary does not know is ignored, so check
`listmngr conf` after a rename.

The [reference](configuration-reference.md) lists every section and
key with its type and default; it is generated from the source, so it
is as current as the binary.
