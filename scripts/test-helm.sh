#!/bin/sh
# Install the chart on a disposable kind cluster and exercise it as an
# operator would (P10-HELM-TEST): build the image from deploy/Dockerfile and
# load it into the cluster, `helm install --wait`, `helm test`, the in-image
# `listmngr status` probe, the first server owner through `kubectl exec`,
# `/healthz`, `/readyz` and `/web/login` through a port-forward, a
# configuration change through `helm upgrade` (the pod is replaced because
# `checksum/config` changed; the account on the volume survives), then
# `helm uninstall`. The cluster is deleted on exit unless --keep is given.
# Needs docker, kind, kubectl and helm; the only pull is the pinned kind node
# image. SQLite on the cluster's default storage class stands in for the
# database until P10-HELM-DB.
#
#   scripts/test-helm.sh [--keep] [--image NAME:TAG]    (--image skips the build)
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
image=
while [ $# -gt 0 ]; do
  case "$1" in
    --keep) keep=1 ;;
    --image) shift; image=${1:?--image needs NAME:TAG} ;;
    *) printf 'usage: %s [--keep] [--image NAME:TAG]\n' "$0" >&2; exit 2 ;;
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

step "kind create cluster $cluster"
kind delete cluster --name "$cluster" >/dev/null 2>&1 || true
kind create cluster --name "$cluster" --image "$node_image" --wait 120s
k get nodes -o wide

step "kind load docker-image $image"
kind load docker-image "$image" --name "$cluster"

step "helm install $release --wait"
t0=$(date +%s)
h install "$release" "$chart" --wait --timeout 5m \
  --set "image.repository=$repository" --set "image.tag=$tag" --set image.pullPolicy=Never \
  --set-string 'secrets.LISTMNGR__DATABASE__URL=sqlite:///var/lib/listmngr/listmngr.db?mode=rwc'
printf 'installed and ready in %ss\n' "$(( $(date +%s) - t0 ))"
k get pods,pvc,svc,sa -l "app.kubernetes.io/instance=$release"
pod_before=$(k get pod -l "app.kubernetes.io/instance=$release" -o jsonpath='{.items[0].metadata.name}')
checksum_before=$(k get deploy "$deploy" -o jsonpath="{.spec.template.metadata.annotations['checksum/config']}")

step "helm test $release"
h test "$release" --logs

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
h upgrade "$release" "$chart" --reuse-values --set-file "config=$work/config.toml" --wait --timeout 5m
printf 'upgraded and ready in %ss\n' "$(( $(date +%s) - t0 ))"
pod_after=$(k get pod -l "app.kubernetes.io/instance=$release" -o jsonpath='{.items[0].metadata.name}')
checksum_after=$(k get deploy "$deploy" -o jsonpath="{.spec.template.metadata.annotations['checksum/config']}")
[ "$checksum_before" != "$checksum_after" ] || fail "checksum/config did not change"
[ "$pod_before" != "$pod_after" ] || fail "the pod was not replaced"
printf 'pod %s -> %s\n' "$pod_before" "$pod_after"
k get configmap "$deploy" -o jsonpath='{.data.listmngr\.toml}' | grep -q 'Example Lists, upgraded' || fail "the ConfigMap did not change"
k exec "deploy/$deploy" -c listmngr -- /listmngr --config /etc/listmngr/listmngr.toml user export "$user_id" | grep -q 'admin@example.com' \
  || fail "the account did not survive the upgrade"
printf 'account %s survived on the volume\n' "$user_id"

step "helm uninstall $release"
h uninstall "$release" --wait
k wait --for=delete pod -l "app.kubernetes.io/instance=$release" --timeout=120s 2>/dev/null || true
k get pods,pvc -l "app.kubernetes.io/instance=$release" 2>&1 | tail -2

printf '\nhelm harness: OK in %ss\n' "$(( $(date +%s) - start ))"
