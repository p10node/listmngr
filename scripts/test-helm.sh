#!/bin/sh
# Install the chart on a disposable kind cluster and exercise it as an
# operator would (P10-HELM-TEST): build the image from deploy/Dockerfile and
# load it into the cluster, `helm install --wait`, `helm test`, the in-image
# `listmngr status` probe, the first server owner through `kubectl exec`,
# `/healthz`, `/readyz` and `/web/login` through a port-forward, a
# configuration change through `helm upgrade` (the pod is replaced because
# `checksum/config` changed; the account on the volume survives), then
# `helm uninstall`. The cluster is deleted on exit unless --keep is given.
# Needs docker, kind, kubectl and helm. The chart's PostgreSQL (a StatefulSet
# from `postgres:17-alpine@sha256:…`, pulled by the node: a multi-architecture
# digest cannot be `kind load`ed from one platform's docker image) is the
# database, with a password made here; --sqlite installs with
# `postgresql.enabled=false` and SQLite on the volume instead. --mta builds
# the Postfix image from deploy/postfix/Dockerfile too and installs with
# `mta.enabled=true` (the smtp Service as ClusterIP: kind has no load
# balancer): after the install, `postfix status` in the sidecar, a domain and
# a list through the CLI, the maps on the shared volume, then from a probe pod
# outside `mynetworks` the RCPT matrix (list address 250, unknown 550, relay
# 554) and one message to the list, which Postfix must hand to LMTP on
# loopback (`status=sent`). Delivery to the internet is not attempted. --oci
# packages the chart, pushes it to a disposable local OCI registry
# (`registry:3`, pinned) and installs and upgrades from `oci://` — the way
# a release is consumed — instead of from the directory.
#
#   scripts/test-helm.sh [--keep] [--sqlite] [--mta] [--oci] [--image NAME:TAG] [--mta-image NAME:TAG]
#   (--image / --mta-image skip the builds)
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

cluster=${KIND_CLUSTER:-listmngr-helm}
# kind v0.33.0's default node image, pinned by digest like every other image.
node_image=kindest/node:v1.37.0@sha256:a1ed56cfb0e7b93589bdf97c8cd566405a265939e3620fc4f5de89adff580ae5
chart=deploy/helm/listmngr
release=t
deploy=$release-listmngr
keep=0
sqlite=0
mta=0
oci=0
image=
mta_image=
registry_image=registry:3@sha256:ddf754342cfc8acc51a56d5d0ab6af06826461864460636d8bd5c546dab2a7b8
registry_name=${KIND_CLUSTER:-listmngr-helm}-registry
# The chart's own pinned busybox (`secretFilesImage`), already on the node for `helm test`.
probe_image=busybox:1.37.0@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e
while [ $# -gt 0 ]; do
  case "$1" in
    --keep) keep=1 ;;
    --sqlite) sqlite=1 ;;
    --mta) mta=1 ;;
    --oci) oci=1 ;;
    --image) shift; image=${1:?--image needs NAME:TAG} ;;
    --mta-image) shift; mta_image=${1:?--mta-image needs NAME:TAG} ;;
    *) printf 'usage: %s [--keep] [--sqlite] [--mta] [--oci] [--image NAME:TAG] [--mta-image NAME:TAG]\n' "$0" >&2; exit 2 ;;
  esac
  shift
done
for tool in docker kind kubectl helm curl; do
  command -v "$tool" >/dev/null 2>&1 || { printf 'helm harness: %s is required\n' "$tool" >&2; exit 2; }
done

k() { kubectl --context "kind-$cluster" "$@"; }
h() { helm --kube-context "kind-$cluster" "$@"; }
fail() { printf 'helm harness: %s\n' "$*" >&2; exit 1; }
start=$(date +%s)
step() { printf '\n== %s (t+%ss)\n' "$1" "$(( $(date +%s) - start ))"; }

work=$(mktemp -d)
pf_pid=
cleanup() {
  status=$?
  if [ -n "$pf_pid" ]; then { kill "$pf_pid" && wait "$pf_pid"; } 2>/dev/null || true; fi
  if [ "$status" -ne 0 ]; then
    printf '\n== failure diagnostics\n'
    k get pods,pvc -o wide 2>/dev/null || true
    k describe pod -l "app.kubernetes.io/instance=$release" 2>/dev/null | tail -40 || true
    k logs "deploy/$deploy" --all-containers --tail=40 2>/dev/null || true
  fi
  docker rm -f "$registry_name" >/dev/null 2>&1 || true
  if [ "$keep" -eq 0 ]; then
    kind delete cluster --name "$cluster" >/dev/null 2>&1 || true
  else
    printf 'helm harness: cluster %s kept (kubectl --context kind-%s)\n' "$cluster" "$cluster"
  fi
  rm -rf "$work"
}
trap cleanup EXIT

if [ -z "$image" ]; then
  image=listmngr:helm-test
  step "docker build $image"
  docker build -f deploy/Dockerfile -t "$image" .
fi
case "$image" in *:*) ;; *) fail "--image needs NAME:TAG" ;; esac
repository=${image%:*}
tag=${image##*:}
if [ "$mta" -eq 1 ] && [ -z "$mta_image" ]; then
  mta_image=listmngr-postfix:helm-test
  step "docker build $mta_image"
  docker build -f deploy/postfix/Dockerfile -t "$mta_image" .
fi

step "kind create cluster $cluster"
kind delete cluster --name "$cluster" >/dev/null 2>&1 || true
kind create cluster --name "$cluster" --image "$node_image" --wait 120s
k get nodes -o wide

step "kind load docker-image $image"
kind load docker-image "$image" --name "$cluster"
if [ "$mta" -eq 1 ]; then
  kind load docker-image "$mta_image" --name "$cluster"
fi

# Where the chart comes from: the directory, or a registry it was pushed to.
chart_ref=$chart
if [ "$oci" -eq 1 ]; then
  step "helm package, push to a disposable OCI registry, show"
  chart_version=$(sed -n 's/^version: //p' "$chart/Chart.yaml")
  helm package "$chart" -d "$work" > /dev/null
  docker rm -f "$registry_name" >/dev/null 2>&1 || true
  docker run -d --name "$registry_name" -p 127.0.0.1:0:5000 "$registry_image" > /dev/null
  port=$(docker port "$registry_name" 5000/tcp | sed -n 's/.*://p' | head -1)
  i=0
  until curl -fsS "http://127.0.0.1:$port/v2/" > /dev/null 2>&1; do
    i=$((i + 1)); [ "$i" -lt 30 ] || fail "the registry never answered on 127.0.0.1:$port"
    sleep 1
  done
  helm push "$work/listmngr-$chart_version.tgz" "oci://127.0.0.1:$port/charts" --plain-http
  chart_ref="oci://127.0.0.1:$port/charts/listmngr"
  helm show chart "$chart_ref" --version "$chart_version" --plain-http | grep -E '^(name|version|appVersion):'
  set -- --version "$chart_version" --plain-http
else
  set --
fi
chart_flags=$*

step "helm install $release --wait"
if [ "$sqlite" -eq 1 ]; then
  set -- "$@" --set postgresql.enabled=false \
    --set-string 'secrets.LISTMNGR__DATABASE__URL=sqlite:///var/lib/listmngr/listmngr.db?mode=rwc'
else
  # URL-unreserved characters only: the chart writes it into the URL as is.
  password=$(LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 32)
  set -- "$@" --set "postgresql.auth.password=$password"
fi
if [ "$mta" -eq 1 ]; then
  set -- "$@" --set mta.enabled=true --set mta.hostname=lists.example.invalid \
    --set "mta.image.repository=${mta_image%:*}" --set "mta.image.tag=${mta_image##*:}" \
    --set mta.image.pullPolicy=Never --set mta.service.type=ClusterIP
fi
t0=$(date +%s)
h install "$release" "$chart_ref" --wait --timeout 5m \
  --set "image.repository=$repository" --set "image.tag=$tag" --set image.pullPolicy=Never "$@"
printf 'installed and ready in %ss\n' "$(( $(date +%s) - t0 ))"
k get pods,pvc,svc,sa,statefulset -l "app.kubernetes.io/instance=$release"
if [ "$sqlite" -eq 0 ]; then
  k exec "statefulset/$deploy-postgresql" -- pg_isready -U listmngr -d listmngr
fi
app="app.kubernetes.io/instance=$release,app.kubernetes.io/component=listmngr"
pod_before=$(k get pod -l "$app" -o jsonpath='{.items[0].metadata.name}')
checksum_before=$(k get deploy "$deploy" -o jsonpath="{.spec.template.metadata.annotations['checksum/config']}")

step "helm test $release"
h test "$release" --logs

if [ "$mta" -eq 1 ]; then
  step "front MTA: postfix status, a domain and a list, the maps"
  k exec "deploy/$deploy" -c postfix -- postfix status
  k exec "deploy/$deploy" -c listmngr -- /listmngr --config /etc/listmngr/listmngr.toml domains add example.invalid > /dev/null
  k exec "deploy/$deploy" -c listmngr -- /listmngr --config /etc/listmngr/listmngr.toml lists create alpha.example.invalid --display-name Alpha > /dev/null
  i=0
  until k exec "deploy/$deploy" -c postfix -- grep -q 'alpha@example' /var/lib/listmngr/mta/current/recipients.regexp 2>/dev/null; do
    i=$((i + 1)); [ "$i" -lt 30 ] || fail "the list never reached the maps on the shared volume"
    sleep 1
  done
  k exec "deploy/$deploy" -c postfix -- sh -c 'echo "maps: $(readlink /var/lib/listmngr/mta/current), $(grep -c . /var/lib/listmngr/mta/current/recipients.regexp) recipient rows"'
  sleep 6   # the entrypoint reloads Postfix within 5 s of a new generation

  step "front MTA: the RCPT matrix and one message, from a pod outside mynetworks"
  cat > "$work/probe.sh" <<'PROBE'
smtp="$1"
say() { for line in "$@"; do sleep 1; printf '%s\r\n' "$line"; done; }
code() { tr -d '\r' | grep -E '^[0-9]{3} ' | tail -2 | head -1 | cut -c1-3; }
rcpt() { say "EHLO probe.example.invalid" "MAIL FROM:<sender@example.org>" "RCPT TO:<$1>" "QUIT" | nc -w 15 "$smtp" 25 | code; }
printf 'list=%s unknown=%s relay=%s\n' "$(rcpt alpha@example.invalid)" "$(rcpt nobody@example.invalid)" "$(rcpt someone@elsewhere.invalid)"
say "EHLO probe.example.invalid" "MAIL FROM:<sender@example.org>" "RCPT TO:<alpha@example.invalid>" "DATA" \
  "From: sender@example.org" "To: alpha@example.invalid" "Subject: helm harness probe" "Message-ID: <probe@example.org>" "" "hello" "." "QUIT" \
  | nc -w 20 "$smtp" 25 | tr -d '\r' | grep -E '^250 2\.0\.0' | sed 's/^/data=/'
PROBE
  k run smtp-probe --image="$probe_image" --restart=Never --rm -i --quiet --command -- sh -c "$(cat "$work/probe.sh")" "probe" "$deploy-smtp" > "$work/probe.out" 2>&1 || true
  cat "$work/probe.out"
  grep -q '^list=250 unknown=550 relay=554$' "$work/probe.out" || fail "RCPT matrix is not 250/550/554"
  grep -q '^data=250 2.0.0 Ok: queued' "$work/probe.out" || fail "Postfix did not queue the message"
  i=0
  until k logs "deploy/$deploy" -c postfix 2>/dev/null | grep -q 'relay=127.0.0.1\[127.0.0.1\]:8024.*status=sent'; do
    i=$((i + 1)); [ "$i" -lt 30 ] || { k logs "deploy/$deploy" -c postfix --tail=20; fail "Postfix never handed the message to LMTP on loopback"; }
    sleep 1
  done
  k logs "deploy/$deploy" -c postfix | grep 'status=sent' | tail -1 | sed 's/^/postfix: /'
fi

step "in-image probe: listmngr status"
k exec "deploy/$deploy" -c listmngr -- /listmngr --config /etc/listmngr/listmngr.toml status

step "first server owner through kubectl exec"
printf 'correct-horse-battery-staple-9\n' > "$work/password"
k exec -i "deploy/$deploy" -c listmngr -- /listmngr --config /etc/listmngr/listmngr.toml \
  user create admin@example.com --display-name Admin --server-owner --password-stdin \
  < "$work/password" > "$work/user.json"
user_id=$(sed -n 's/^ *"id": *"\{0,1\}\([^",]*\)"\{0,1\},\{0,1\}$/\1/p' "$work/user.json" | head -1)
[ -n "$user_id" ] || fail "user create printed no id: $(cat "$work/user.json")"
printf 'server owner %s created\n' "$user_id"

step "web endpoints through a port-forward"
k port-forward "svc/$deploy" 18000:8000 > "$work/port-forward.log" 2>&1 &
pf_pid=$!
i=0
until curl -fsS http://127.0.0.1:18000/healthz > /dev/null 2>&1; do
  i=$((i + 1)); [ "$i" -lt 30 ] || fail "port-forward never answered: $(cat "$work/port-forward.log")"
  sleep 1
done
printf '/healthz: %s\n' "$(curl -fsS http://127.0.0.1:18000/healthz)"
printf '/readyz: %s\n' "$(curl -fsS http://127.0.0.1:18000/readyz)"
code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18000/web/login)
[ "$code" = 200 ] || fail "/web/login answered $code"
printf '/web/login: %s\n' "$code"
{ kill "$pf_pid" && wait "$pf_pid"; } 2>/dev/null || true
pf_pid=

step "helm upgrade with a changed configuration"
h get values "$release" --all -o yaml > "$work/values.yaml"
sed -n '/^config: |/,/^[a-z]/p' "$work/values.yaml" | sed '1d;$d' | sed 's/^  //' > "$work/config.toml"
grep -q '^\[site\]' "$work/config.toml" || fail "could not read the installed config back"
sed -i.bak 's/^name = .*/name = "Example Lists, upgraded"/' "$work/config.toml"
t0=$(date +%s)
# shellcheck disable=SC2086  # $chart_flags is a flag list on purpose (empty, or --version X --plain-http)
h upgrade "$release" "$chart_ref" $chart_flags --reuse-values --set-file "config=$work/config.toml" --wait --timeout 5m
printf 'upgraded and ready in %ss\n' "$(( $(date +%s) - t0 ))"
pod_after=$(k get pod -l "$app" -o jsonpath='{.items[0].metadata.name}')
checksum_after=$(k get deploy "$deploy" -o jsonpath="{.spec.template.metadata.annotations['checksum/config']}")
[ "$checksum_before" != "$checksum_after" ] || fail "checksum/config did not change"
[ "$pod_before" != "$pod_after" ] || fail "the pod was not replaced"
printf 'pod %s -> %s\n' "$pod_before" "$pod_after"
k get configmap "$deploy" -o jsonpath='{.data.listmngr\.toml}' | grep -q 'Example Lists, upgraded' || fail "the ConfigMap did not change"
k exec "deploy/$deploy" -c listmngr -- /listmngr --config /etc/listmngr/listmngr.toml user export "$user_id" | grep -q 'admin@example.com' \
  || fail "the account did not survive the upgrade"
if [ "$sqlite" -eq 0 ]; then
  rows=$(k exec "statefulset/$deploy-postgresql" -- psql -U listmngr -d listmngr -Atc 'select count(*) from users')
  [ "$rows" = 1 ] || fail "PostgreSQL holds $rows accounts, expected 1"
  printf 'account %s survived in PostgreSQL (%s row)\n' "$user_id" "$rows"
else
  printf 'account %s survived on the volume\n' "$user_id"
fi

step "helm uninstall $release"
h uninstall "$release" --wait
k wait --for=delete pod -l "app.kubernetes.io/instance=$release" --timeout=120s 2>/dev/null || true
k get pods,pvc -l "app.kubernetes.io/instance=$release" 2>&1 | tail -2

printf '\nhelm harness: OK in %ss\n' "$(( $(date +%s) - start ))"
