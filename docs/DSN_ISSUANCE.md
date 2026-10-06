# Durable ordinary-delivery DSN issuance

Acceptance: `P3-DSN-ISSUANCE`. Outbound issuance and real RFC3461 ENVID only;
**no incoming token route, DSN scoring, reporting-MTA authentication or full
Mailman-replacement claim**. This supersedes the no-carrier boundary in
[DSN_PRODUCER_PREREQUISITE.md](DSN_PRODUCER_PREREQUISITE.md).

## Operator contract

```toml
[mta]
# Existing mail enablement and explicit TLS/AUTH policy remain mandatory.
smtp_single_recipient = true
# Default false; enable deliberately after provisioning the relay and key.
dsn_issuance_enabled = true
dsn_key_id = "primary-1"
dsn_key_file = "/run/secrets/listmngr-dsn-key"
dsn_ttl_secs = 604800
```

Provision exactly 32 binary random bytes in a private regular file. Unix loading
rejects symlinks, non-regular files and group/other permissions; nonblocking open
and a 33-byte read cap prevent FIFO and oversized-file hangs. Errors/Debug do not
expose key material. Key IDs are 1–16 ASCII alphanumeric/hyphen characters. TTL is
60–2592000 seconds. Singleton isolation is a configuration prerequisite, not a
silently forced setting. TLS and AUTH retain their existing validation; required
STARTTLS never downgrades to plaintext. Explicit trusted plaintext remains an
operator-selected supported policy, not a new security fallback.

Ordinary member deliveries receive one ENVID per recipient per reserved attempt.
Null-envelope workflow/owner notices and digest-provenance deliveries do not
issue ENVIDs. Default-off transport behavior is unchanged. New producer bindings
are nevertheless recorded while disabled so later opt-in need not trust old JSON.

## Durable authority and producer inventory

Migration `0026_dsn_issuance.sql` adds list delivery epochs, preference generations,
message-to-list bindings, planned recipient member IDs and an issuance ledger.
Current pre-migration lists receive their existing creation timestamp as an epoch;
new lists receive fresh UUIDs. **No historical message or recipient is backfilled.**

* `mail_queue::enqueue_batch` (also used by `enqueue`, LMTP intake and CLI injection)
  binds a newly stored message to the current list epoch inside its intake/audit
  transaction. Context selects the list only here, not retrospectively at SMTP.
  Unknown/unparseable list contexts remain unbound.
* `insert_child_job` snapshots the current planned membership ID using that stored
  message binding and exact original address spelling. This covers shared child
  handoff and both ordinary moderation acceptance paths. Recreating a textual
  list or replacing a membership cannot supply authority for the old job.
* Internal workflow, owner and digest producers retain their existing dedicated
  job provenance. They are explicitly excluded, not inferred from MIME headers.

Opted-in ordinary historical/unbound jobs fail closed (quarantine on invalid
local authority). Database dependencies retry; no unsigned/plain-ENVID fallback.
The ledger resolves job/message from the locked stored queue row, not mutable
`Lease.job` fields. It verifies list epoch and planned membership, then locks the
member/address/layered preferences before reading current eligibility/generations.
Original transport spelling and canonical mailbox identity are both signed.

Every supported re-enable writer is covered by generation changes:

| Writer                                               | Covered surface                             |
|------------------------------------------------------|---------------------------------------------|
| `MembersRepo::update_with_context` preference update | member PATCH delivery status/mode           |
| `PreferencesRepo::update_tx`                         | generic member/address/user preference APIs |
| `web_sessions` membership preference transaction     | authenticated browser preferences           |
| `web_membership` recovery transaction                | browser bounce recovery and counter reset   |

Generations advance on each business preference write (including conservative
extra invalidation for non-status edits), not lock-only no-op UPDATEs. New/absent
preference links are distinguished by their IDs; claims also contain member,
address and user IDs. Direct SQL edits are outside supported preference APIs.
These are issuance snapshots. A future incoming consumer must independently
revalidate all current IDs/generations and eligibility before any mutation.

`begin_delivery_with_dsn` composes pending→ambiguous reservation, immutable ledger
INSERTs and per-issuance audit in one transaction. The last queue audit is followed
by the existing live lease fence, before commit. No SMTP command starts unless
this transaction commits. The ledger includes job/message/list epoch, original
and canonical recipient, planned/current member, address/user and preference
ID/generation tuples, random 128-bit nonce, version/key ID, issue/expiry times and
opaque attempt identity. Audit targets the issuance ID and does not contain ENVID
or key bytes. There is no issuance-update or incoming-consumption API.

## Carrier and recovery semantics

The shared SMTP transaction sends `MAIL FROM:<list-bounces@host> ENVID=...` only
when the **actual current EHLO extension list** advertises DSN. A first-line
hostname containing DSN or XDSN is not capability. After STARTTLS only the fresh
TLS EHLO counts; authenticated transport forwards capability from its post-TLS
AUTH negotiation EHLO. Missing capability is a known transient before MAIL, with
normal durable backoff and no bounce event.

The token is `1.<key-id>.<128-bit nonce hex>.<base64url HMAC-SHA256>` (at most 96
ASCII bytes). Its alphabet is already safe RFC3461 xtext; there is no recipient
plaintext, truncation, NOTIFY or ORCPT. RustCrypto `hmac`/`sha2` supply HMAC and
constant-time tag verification. The signed input uses the versioned domain
`listmngr/outbound-dsn/envid/v1`, a NUL separator and explicit big-endian lengths
for key ID, nonce and exact stored canonical JSON claims. Verification rejects
expired/not-yet-issued, malformed, wrong-key and tampered claims. HMAC proves
local issuance correlation, **not who sent a DSN or whether its contents are true**.

Known transient retry creates a distinct immutable issuance for the new attempt;
old records remain readable, not overwritten. Pool/process restart does not
reissue unresolved reservations: all unresolved recipients remain ambiguous and
are excluded from automatic retry, even if SMTP never started. Existing operator
ambiguity resolution is still explicit. A reservation is not relay acceptance,
and relay acceptance is not mailbox delivery. There is no exactly-once claim.

Keep the DB/spool and issuer keys in protected backups. Use a distinct key ID when
rotating; retain old keys separately for any future historical verifier. This
stage loads one active issuer only, has no incoming keyring/consumer and no ledger
retention/GC API. Do not erase epochs or backfill old jobs to force opt-in delivery.

## Verification scope

Local evidence is in `target/dsn-issuance/`. The first actual-MAIL ENVID tracer and
recreated-membership regression have retained behavioral RED→GREEN logs. Later
negative/rollback/expiry/transport controls are supplementary GREEN regressions,
not retroactive RED evidence. The tracer asserts ledger visibility while the
relay has not yet accepted MAIL, exact singleton RCPT spelling, distinct tokens,
and unchanged ENVID readback after closing/reopening the pool. PostgreSQL runs
in an owned schema on an owned native cluster and also exercises audit sabotage,
claim tampering, expiry and known retry history; the fixture is stopped/removed.

Post-TLS DSN capability differential is exercised over real Rustls sockets.
Inherited TLS/AUTH and ambiguity tests are separate regression evidence. This is
not a new independent Postfix/Exim oracle, executable kill/restart experiment,
DSN-plus-AUTH interoperability matrix, or PostgreSQL contention matrix for every
new identity/generation intersection. Full workspace/product acceptance belongs
to parent integration. Exact final commands/exits, hashes, baseline-relative patch
and remaining acceptance limits are recorded in `target/dsn-issuance/HANDOFF.json`.
