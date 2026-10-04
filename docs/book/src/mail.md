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

- **DKIM** per domain: a `[[mta.dkim_signing]]` table per selector with
  `domain`, `selector` and `private_key_file` — an RSA or an Ed25519
  key, several selectors per domain signing side by side; the domain
  page shows the TXT records to publish. `listmngr dkim gen` makes a
  key and prints its record, `dkim records` prints every configured
  record, and `dkim dns` checks what DNS publishes; the operations
  chapter has the rotation steps.
- **ARC**: `[mta.arc]` seals every delivered post so the list's changes
  (subject prefix, footer, `From` munging) do not break DMARC
  downstream.
- **Inbound checks**: SPF, DKIM and DMARC are evaluated at intake and
  recorded in `Authentication-Results`; a list's
  `dmarc_mitigate_action` decides what happens to a post from a domain
  with `p=reject` or `p=quarantine`.

## Limits at the intake

`mta.max_message_bytes` bounds the size the listeners accept, and
`mta.max_header_count`, `max_mime_parts` and `max_mime_depth` (500,
1000 and 20 by default) bound the shape: a message over any of them is
refused at `DATA` with `554 5.6.0` for every recipient and nothing is
stored, and the news gateway passes over such an article. The per-list
`max_message_size` is a moderation rule, not a listener limit. Two more
rules of the posting chain guard against loops and floods:
`mta.max_received_hops` (30; 0 off) discards a post with more
`Received:` headers than that, and `security.rate_limit.post` (unset by
default; `"20/hour"`, say) holds a sender's post for a moderator once
that many of theirs were accepted on the list inside the window.

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
