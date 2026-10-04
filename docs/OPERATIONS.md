# Operations

What an operator runs, watches and tunes. The configuration reference is
`listmngr conf` (every key with its default, secrets redacted); this page
says which keys matter for which concern.

## Sizing and tuning

The mail path is one LMTP acceptor (up to fifty concurrent sessions),
one `in` processor, one pipeline processor and one `out` processor per
`listmngr serve`, all over one database pool. A post to a list of *N*
members becomes one `out` job with *N* recipient rows, delivered to the
relay in SMTP transactions of at most `mta.max_recipients` recipients
each (500 by default), each recipient row updated with its outcome.

The numbers below are the `P7-LOAD` benchmark
(`crates/cli/tests/load.rs`): a release build, SQLite on a laptop, a
loopback relay that accepts everything, one `out` worker; 10,000
members and 100 posts. The roster was written in 3.2 s. The 100 posts
were accepted through LMTP in 0.105 s (949 posts/s). The relay accepted
all 1,000,100 recipients in 155.7 s: 6,425 recipients/s in 2,100 SMTP
transactions of up to 500 recipients (21 per post), or 2,313 posts an
hour at that rate. The queue latency from a post's LMTP `250` to its
first accepted recipient was p50 86.5 s, p95 148.6 s, max 154.8 s, and
to its last p50 87.1 s, p95 149.2 s, max 155.4 s: the burst is served
in order by the one worker, so a post waits behind the ones before it
(about 1.55 s of service per post), and the latency is the queue, not
the work.

What they say for sizing:

- **Throughput is bounded by the relay round trip and the recipient
  bookkeeping, not by intake.** Intake through LMTP is hundreds of posts
  a second. The plan's target, a thousand posts an hour to a
  ten-thousand-member list, is 10 million recipients an hour; this path
  moved 23 million an hour, 2.3× the target. A burst still waits in
  line: with one `out` worker the hundredth post reached its first
  member 155 s after it was accepted.
- **`mta.max_recipients`** (default 500) is the recipients per SMTP
  transaction. Raise it towards what the relay allows (Postfix's
  `smtpd_recipient_limit` is 1000 by default) to halve the transactions
  per post; lower it for a relay that rate-limits by recipient.
- **`database.max_connections`** (default 20) bounds the pool shared by
  the web and the runners; the runners hold a few connections each.
  SQLite serialises writers regardless of the pool; PostgreSQL scales
  with it.
- **`mailman.finished_job_retention_secs`** keeps finished jobs and
  their recipient rows for the audit trail; a busy site lowers it so the
  task sweep keeps the tables small. The sweep runs on the mail role's
  schedule and `listmngr tasks run` by hand.
- **`mta.retry_initial_secs` / `retry_max_secs`** are the backoff of a
  refused or failed delivery; a relay that is often busy wants a longer
  ceiling, a loopback relay a shorter floor.
- **One `out` worker per process.** A burst is served in order; the
  latency a post sees is mostly the posts queued before it. The relay
  side is where the time goes (one SMTP transaction per 500 recipients,
  about 1.5 s per ten-thousand-member post on loopback); a faster relay
  or a smaller `max_recipients` round trip helps more than more cores.
- **Personalised delivery** (`personalize = full`) sends one message per
  member instead of one per transaction of `max_recipients`; expect the
  throughput to drop by that factor.

## Watching

- `listmngr status` probes `/healthz` and `/readyz` of the running
  server; `listmngr doctor` checks the database ledger, the relay
  greeting and the mail domains' DNS without changing anything.
- `/metrics` is Prometheus text: `listmngr_queue_jobs{queue,state}`
  (jobs per queue and state), `listmngr_queue_oldest_ready_age_seconds`,
  `listmngr_lmtp_recipients_total{result}`,
  `listmngr_posts_total{disposition}`,
  `listmngr_delivery_recipients_total{result}`,
  `listmngr_smtp_transactions_total{result}`, and the histograms
  `listmngr_smtp_transaction_seconds` and
  `listmngr_delivery_latency_seconds` (LMTP acceptance to the relay
  accepting a recipient — the latency this benchmark measures from
  outside).
- `listmngr queue ls --queue out`, `queue show <job>` and `queue
  recipients <job>` are the queue by hand; an `ambiguous` recipient is
  one whose SMTP outcome the server could not learn (it crashed
  mid-transaction) and waits for `queue resolve`.

## Keeping the data

- `listmngr backup <dir>` and `listmngr restore <dir>` move the whole
  database, message bytes included, between any two backends of the
  same version.
- `[message_store]` chooses where message bytes live; `listmngr
  message-store check` and `migrate` keep the rows and the store in
  step.
- The task sweep (`listmngr tasks run`, or the mail role's schedule)
  collects what nothing references any more.

## Certificates

`[web] tls` with `cert_file`/`key_file`, or `acme_domains` for a
certificate ordered and renewed through ACME on the listener itself;
the plain `web.listen` stays for probes and a proxy.
