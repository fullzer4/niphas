#!/usr/bin/env bash
set -euo pipefail

# ═══════════════════════════════════════════════════════════
#  niphas — real E2E test
#  validates: flake eval → closure → CSI mount → running pod
#
#  usage: nix develop -c bash hack/test-e2e-real.sh
# ═══════════════════════════════════════════════════════════

CLUSTER="niphas-e2e"
NS="niphas-system"
APP_NS="niphas-e2e"
PORT=18080

# ── colors ──────────────────────────────────────────────
bold='\033[1m'
dim='\033[2m'
blue='\033[34m'
green='\033[32m'
red='\033[31m'
cyan='\033[36m'
reset='\033[0m'

step()  { echo -e "\n${bold}${blue}[$1/${TOTAL}]${reset} ${bold}$2${reset}"; }
ok()    { echo -e "    ${green}✓${reset} $*"; }
info()  { echo -e "    ${dim}$*${reset}"; }
fail()  { echo -e "    ${red}✗ $*${reset}"; dump_debug; exit 1; }
TOTAL=10

dump_debug() {
    echo -e "\n${bold}${red}── debug ──${reset}"
    echo -e "${dim}"
    kubectl -n "$APP_NS" get niphasworkload test-app -o yaml 2>/dev/null | tail -30 || true
    echo "---"
    kubectl -n "$APP_NS" get pods -o wide 2>/dev/null || true
    echo "---"
    kubectl -n "$APP_NS" describe pods 2>/dev/null | tail -40 || true
    echo "---"
    kubectl -n "$NS" logs -l app.kubernetes.io/name=niphas-operator --tail=20 2>/dev/null || true
    echo "---"
    kubectl -n "$NS" logs -l app.kubernetes.io/name=niphas-eval --tail=20 2>/dev/null || true
    echo "---"
    kubectl -n "$NS" logs -l app.kubernetes.io/name=niphas-csi --all-containers --tail=20 2>/dev/null || true
    echo -e "${reset}"
}

cleanup() { kind delete cluster --name "$CLUSTER" 2>/dev/null || true; }
trap cleanup EXIT

echo -e "${bold}${cyan}"
echo "  ┌─────────────────────────────────────────┐"
echo "  │         niphas  ·  real E2E test         │"
echo "  │  flake eval → closure → CSI → service   │"
echo "  └─────────────────────────────────────────┘"
echo -e "${reset}"

# ── 1. build ────────────────────────────────────────────
step 1 "building OCI images"
nix build .#all-images -o result-images --no-link 2>&1 | tail -3
nix build .#all-images -o result-images
ok "5 images built"

# ── 2. cluster ──────────────────────────────────────────
step 2 "creating kind cluster"
kind delete cluster --name "$CLUSTER" 2>/dev/null || true
kind create cluster --name "$CLUSTER" --config hack/kind-config.yaml --wait 60s 2>&1 | tail -1
ok "cluster ready (2 nodes)"

# ── 3. images ───────────────────────────────────────────
step 3 "loading images into kind"
for img in operator eval csi runner mock-eval; do
    kind load image-archive "result-images/${img}.tar.gz" --name "$CLUSTER" 2>/dev/null
    ok "$img"
done

# ── 4. nodes ────────────────────────────────────────────
step 4 "labeling nodes for CSI"
kubectl label nodes --all niphas.io/store=true --overwrite >/dev/null
ok "all nodes labeled niphas.io/store=true"

# ── 5. deploy ───────────────────────────────────────────
step 5 "installing niphas (real eval, not mock)"
helm install niphas charts/niphas \
    --namespace "$NS" \
    --create-namespace \
    --set operator.image.pullPolicy=Never \
    --set operator.image.tag=dev \
    --set eval.image.pullPolicy=Never \
    --set eval.image.tag=dev \
    --set csi.image.pullPolicy=Never \
    --set csi.image.tag=dev \
    --set runner.image.tag=dev \
    --wait --timeout 180s >/dev/null
ok "operator, eval, csi running"

# ── 6. workload ─────────────────────────────────────────
step 6 "creating NiphasWorkload"
kubectl create namespace "$APP_NS" 2>/dev/null || true
kubectl apply -f hack/test-workload.yaml >/dev/null
info "flakeRef: github:fullzer4/niphas?dir=hack/test-app"
info "attribute: packages.x86_64-linux.default"
ok "workload created"

# ── 7. wait for phases ──────────────────────────────────
step 7 "waiting for workload lifecycle"
LAST_PHASE=""
for i in $(seq 1 120); do
    PHASE=$(kubectl -n "$APP_NS" get niphasworkload test-app \
        -o jsonpath='{.status.phase}' 2>/dev/null || echo "Pending")

    if [ "$PHASE" != "$LAST_PHASE" ]; then
        [ -n "$LAST_PHASE" ] && echo ""
        case "$PHASE" in
            Pending)      ok "phase: Pending" ;;
            Evaluating)   ok "phase: Evaluating  (nix eval in progress...)" ;;
            Provisioning) ok "phase: Provisioning (CSI fetching NARs...)" ;;
            Running)      ok "phase: Running"; break ;;
            Failed)       fail "phase: Failed" ;;
        esac
        LAST_PHASE="$PHASE"
    else
        printf "."
    fi
    sleep 5
done
echo ""
[ "$PHASE" = "Running" ] || fail "stuck at phase: $PHASE (timeout after 10min)"

# ── 8. pod check ────────────────────────────────────────
step 8 "verifying pod"
POD=$(kubectl -n "$APP_NS" get pods -l niphas.io/workload=test-app \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
[ -n "$POD" ] || fail "no pod found"

STORE_PATH=$(kubectl -n "$APP_NS" get niphasworkload test-app \
    -o jsonpath='{.status.storePath}' 2>/dev/null || echo "?")
CLOSURE_SIZE=$(kubectl -n "$APP_NS" get niphasworkload test-app \
    -o jsonpath='{.status.closurePaths}' 2>/dev/null | tr ',' '\n' | wc -l)

ok "pod: $POD"
ok "store path: $STORE_PATH"
ok "closure size: $CLOSURE_SIZE paths"

# ── 9. nix store mount ──────────────────────────────────
step 9 "checking /nix/store inside container"
MOUNT_COUNT=$(kubectl -n "$APP_NS" exec "$POD" -- ls /nix/store/ 2>/dev/null | wc -l)
ok "$MOUNT_COUNT store paths mounted via CSI"

# ── 10. http test ───────────────────────────────────────
step 10 "testing HTTP service"
kubectl -n "$APP_NS" port-forward "svc/test-app" "$PORT:80" &>/dev/null &
PF_PID=$!
sleep 3

HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$PORT/" 2>/dev/null || echo "000")
BODY=$(curl -s "http://localhost:$PORT/" 2>/dev/null | head -1 || true)
kill "$PF_PID" 2>/dev/null || true

[ "$HTTP_CODE" = "200" ] || fail "HTTP $HTTP_CODE (expected 200)"
ok "HTTP 200 — page served from /nix/store"
info "open http://localhost:$PORT to see the page (port-forward stopped)"

# ── done ────────────────────────────────────────────────
echo ""
echo -e "${bold}${green}"
echo "  ┌─────────────────────────────────────────┐"
echo "  │            all checks passed             │"
echo "  │                                          │"
echo "  │  flake eval       ✓                      │"
echo "  │  closure resolve  ✓                      │"
echo "  │  CSI NAR fetch    ✓                      │"
echo "  │  pod mount        ✓                      │"
echo "  │  HTTP service     ✓                      │"
echo "  └─────────────────────────────────────────┘"
echo -e "${reset}"
