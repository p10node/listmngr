# Connect the mail system

listmngr does not receive mail from the internet itself (unless you
enable the experimental inbound SMTP listener); an MTA — Postfix or Exim
— accepts it, looks the recipient up in maps listmngr writes, and hands
the message over LMTP. Delivered mail goes to a relay over SMTP.

```toml
[mta]
enabled = true
incoming = "postfix"                  # or "exim": which maps to write
map_directory = "/var/lib/listmngr/mta"
lmtp_listen = "127.0.0.1:8024"
smtp_relay = "127.0.0.1:25"
smtp_tls = "required"                 # STARTTLS to the relay; "plaintext_trusted_relay" only on loopback
local_hostname = "lists.example.com"  # the EHLO name
```

## Postfix

`deploy/postfix/main.cf` is the reference configuration: the list
domains in `relay_domains`, `transport_maps` and `virtual_alias_maps`
pointing at the maps listmngr regenerates (`listmngr aliases regen`
writes a new immutable generation; the server does it at start and on
every list change) and
`lmtp:127.0.0.1:8024` as the transport. `postmap` is run by Postfix; the
maps are plain `hash:` tables.

## Exim

`deploy/exim/listmngr.conf` is a router and transport pair that looks
the recipient up in the `lsearch` maps and delivers over LMTP.

## Signing and checking

- **DKIM** per domain: one `[[mta.dkim_signing]]` table per domain with
  `domain`, `selector` and `private_key_file`; the domain page shows
  the TXT record to publish.
- **ARC**: `[mta.arc]` seals every delivered post so the list's changes
  (subject prefix, footer, `From` munging) do not break DMARC
  downstream.
- **Inbound checks**: SPF, DKIM and DMARC are evaluated at intake and
  recorded in `Authentication-Results`; a list's
  `dmarc_mitigate_action` decides what happens to a post from a domain
  with `p=reject` or `p=quarantine`.

## Delivery

Posts go out in SMTP transactions of at most `mta.max_recipients`
recipients, with VERP return paths so bounces come back per recipient;
the bounce processor scores and, by policy, disables or removes.
`mta.retry_initial_secs` and `retry_max_secs` bound the backoff when the
relay refuses.

## TLS for the web

`[web.tls]` adds a TLS listener beside the plain one, with your
certificate or one ordered through ACME (`acme_domains`); the plain
listener stays for probes and a reverse proxy.
