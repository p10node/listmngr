#!/bin/sh
# Verify the shipped MTA configurations against fixture map generations in
# disposable containers: the Postfix image (deploy/postfix/Dockerfile) must
# accept exact list addresses and VERP bounces, refuse unknown recipients,
# plus-extensions and relaying from outside its networks; the Exim routers
# (deploy/exim/listmngr.conf) must route the same set with `exim -bt`.
# Needs Docker and registry access. No listmngr process, DNS or MTA daemon
# on the host is involved; delivery to LMTP is not exercised.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
work=$(mktemp -d)
trap 'rm -rf "$work"; docker rm -f listmngr-mta-check >/dev/null 2>&1 || true' EXIT

fail() { printf 'mta config check: %s\n' "$*" >&2; exit 1; }

# The rows `listmngr aliases regen` writes for lists alpha@ and alpha-join@
# on example.invalid (see crates/mail/tests/mta_maps.rs).
gen="$work/mta/generation-fixture"
mkdir -p "$gen"
ln -s generation-fixture "$work/mta/current"
printf '/^example\\.invalid$/ OK\n' > "$gen/domains.regexp"
: > "$gen/recipients.regexp"; : > "$gen/transport.regexp"
: > "$gen/exim_recipients"
printf 'example.invalid\n' > "$gen/exim_domains"
for local in alpha alpha-bounces alpha-confirm alpha-join alpha-leave alpha-owner alpha-request alpha-subscribe alpha-unsubscribe alpha-join-bounces alpha-join-owner; do
  printf '/^%s@example\\.invalid$/ OK\n' "$local" >> "$gen/recipients.regexp"
  printf '/^%s@example\\.invalid$/ lmtp:[172.28.0.10]:8024\n' "$local" >> "$gen/transport.regexp"
  printf '%s@example.invalid\n' "$local" >> "$gen/exim_recipients"
done
for list in alpha alpha-join; do
  printf '/^%s-bounces\\+[^@=]+=[^@=]+@example\\.invalid$/ OK\n' "$list" >> "$gen/recipients.regexp"
  printf '/^%s-bounces\\+[^@=]+=[^@=]+@example\\.invalid$/ lmtp:[172.28.0.10]:8024\n' "$list" >> "$gen/transport.regexp"
done
chmod -R a+rX "$work"

# --- Postfix ---
docker build -q -f deploy/postfix/Dockerfile -t listmngr-postfix-check . > /dev/null
docker run -d --name listmngr-mta-check -e POSTFIX_MYHOSTNAME=mx.example.invalid \
  -v "$work/mta:/var/lib/listmngr/mta:ro" listmngr-postfix-check > /dev/null
i=0
until docker exec listmngr-mta-check postfix status > /dev/null 2>&1; do
  i=$((i + 1)); [ "$i" -lt 30 ] || fail 'postfix did not start'; sleep 1
done
ip=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' listmngr-mta-check)
smtp=$(docker run --rm alpine:3.22@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce sh -c "
  ( sleep 1; for l in 'EHLO check.invalid' 'MAIL FROM:<someone@sender.invalid>' \
      'RCPT TO:<alpha@example.invalid>' 'RCPT TO:<alpha-bounces+a=b@example.invalid>' \
      'RCPT TO:<nobody@example.invalid>' 'RCPT TO:<alpha+x@example.invalid>' \
      'RCPT TO:<alpha@elsewhere.invalid>' 'QUIT'; do printf '%s\r\n' \"\$l\"; sleep 1; done
  ) | nc -w 20 $ip 25" | grep -v '^250-')
printf '%s\n' "$smtp" | grep -q '^250 2.1.5 Ok' || fail "postfix: list address not accepted: $smtp"
[ "$(printf '%s\n' "$smtp" | grep -c '^250 2.1.5 Ok')" -eq 2 ] || fail "postfix: expected two accepted recipients: $smtp"
printf '%s\n' "$smtp" | grep -q 'nobody@example.invalid.*User unknown' || fail "postfix: unknown recipient accepted: $smtp"
printf '%s\n' "$smtp" | grep -q 'alpha+x@example.invalid.*User unknown' || fail "postfix: plus extension accepted: $smtp"
printf '%s\n' "$smtp" | grep -q 'elsewhere.invalid.*Relay access denied' || fail "postfix: open relay: $smtp"
docker rm -f listmngr-mta-check > /dev/null
printf 'postfix container: PASS\n'

# --- Exim ---
sed 's#/var/lib/listmngr/mta#/work/mta#g' deploy/exim/listmngr.conf > "$work/fragment.conf"
{
  printf 'primary_hostname = mx.example.invalid\ndomainlist relay_to_domains = lsearch;/work/mta/current/exim_domains\n'
  printf 'exim_user = exim\nexim_group = exim\nspool_directory = /tmp/spool\nlog_file_path = /tmp/exim-%%slog\n'
  printf 'acl_smtp_rcpt = acl_check_rcpt\nbegin acl\nacl_check_rcpt:\n  accept domains = +relay_to_domains\n         verify = recipient\n  deny message = relay not permitted\n'
  printf 'begin routers\n'
  sed -n '/^listmngr_lists:/,/^# --- transports ---/p' "$work/fragment.conf" | grep -v '^# --- transports'
  printf 'dnslookup:\n  driver = dnslookup\n  domains = ! +relay_to_domains\n  transport = remote_smtp\n  no_more\n'
  printf 'begin transports\n'
  sed -n '/^listmngr_lmtp:/,/^# Activation gate/p' "$work/fragment.conf" | grep -v '^# Activation gate'
  printf 'remote_smtp:\n  driver = smtp\n'
} > "$work/exim.conf"
exim=$(docker run --rm -v "$work:/work:ro" alpine:3.22@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce sh -c '
  apk add --no-cache exim=4.98.2-r0 > /dev/null 2>&1 && mkdir -p /tmp/spool && cp /work/exim.conf /etc/exim/exim.conf
  for a in alpha@example.invalid alpha-bounces+a=b@example.invalid alpha-join@example.invalid nobody@example.invalid alpha+detail@example.invalid alpha-bounces+x@example.invalid alpha@other.invalid; do
    printf "== %s\n" "$a"; exim -bt "$a" 2>&1 | grep -v "purging\|Suggested"
  done')
for a in alpha@example.invalid alpha-bounces+a=b@example.invalid alpha-join@example.invalid; do
  printf '%s\n' "$exim" | grep -A2 "^== $a" | grep -q 'router = listmngr_lists, transport = listmngr_lmtp' || fail "exim: $a not routed: $exim"
done
for a in nobody@example.invalid alpha+detail@example.invalid alpha-bounces+x@example.invalid; do
  printf '%s\n' "$exim" | grep -A1 "^== $a" | grep -q 'no such list address' || fail "exim: $a not refused: $exim"
done
printf '%s\n' "$exim" | grep -A1 '^== alpha@other.invalid' | grep -q 'Unrouteable' || fail "exim: foreign domain routed: $exim"
printf 'exim routers: PASS\n'
