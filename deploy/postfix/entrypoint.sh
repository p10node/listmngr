#!/bin/sh
# Apply the deployment's identity from the environment, wait for the first
# map generation, then run Postfix in the foreground and reload it whenever
# listmngr publishes a new generation (regexp tables are read at process
# start, so a reload makes list changes visible at once).
set -eu
MAPS=/var/lib/listmngr/mta
postconf -e "myhostname = ${POSTFIX_MYHOSTNAME:-lists.example.invalid}"
postconf -e "mynetworks = 127.0.0.0/8 [::1]/128 ${POSTFIX_MYNETWORKS:-172.28.0.0/24}"
if [ -n "${POSTFIX_RELAYHOST:-}" ]; then
  postconf -e "relayhost = ${POSTFIX_RELAYHOST}"
fi
waited=0
until [ -e "$MAPS/current/transport.regexp" ]; do
  if [ "$waited" -ge 120 ]; then
    echo "no map generation under $MAPS after ${waited}s; is listmngr running with [mta] incoming = postfix?" >&2
    exit 1
  fi
  sleep 2
  waited=$((waited + 2))
done
(
  last=$(readlink "$MAPS/current")
  while sleep 5; do
    now=$(readlink "$MAPS/current" 2>/dev/null || true)
    if [ -n "$now" ] && [ "$now" != "$last" ]; then
      postfix reload
      last=$now
    fi
  done
) &
postfix check
exec postfix start-fg
