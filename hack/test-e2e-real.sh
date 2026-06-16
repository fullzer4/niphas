#!/usr/bin/env bash
set -euo pipefail

# Full E2E test with real Nix eval — validates the complete flow:
#   flake eval → closure resolution → CSI NAR fetch → pod mount → running service
#
# Prerequisites: nix develop shell (provides kind, kubectl, helm)
# Usage: nix develop -c bash hack/test-e2e-real.sh

CLUSTER_NAME="niphas-real-test"
NAMESPACE="niphas-system"
E2E_NS="niphas-e2e"

info()  { echo -e "\033[1;34m▸ $*\033[0m"; }
ok()    { echo -e "\033[1;32m✓ $*\033[0m"; }
fail()  { echo -e "\033[1;31m✗ $*\033[0m"; exit 1; }

cleanup() {
    info "Cleaning up kind cluster..."
    kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null || true
}
trap cleanup EXIT

# ── Step 1: Build images ────────────────────────────────
info "Building OCI images..."
nix build .#all-images -o result-images

# ── Step 2: Create kind cluster ─────────────────────────
info "Creating kind cluster..."
kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null || true
kind create cluster --name "$CLUSTER_NAME" --config hack/kind-config.yaml --wait 60s

# ── Step 3: Load images into kind ───────────────────────
info "Loading images into kind..."
for img in operator eval csi runner mock-eval; do
    kind load image-archive "result-images/${img}.tar.gz" --name "$CLUSTER_NAME"
done

# ── Step 4: Label nodes ─────────────────────────────────
info "Labeling nodes for CSI..."
kubectl label nodes --all niphas.io/store=true --overwrite

# ── Step 5: Install Helm chart (real eval, not mock) ────
info "Installing niphas with REAL eval..."
helm install niphas charts/niphas \
    --namespace "$NAMESPACE" \
    --create-namespace \
    --set operator.image.pullPolicy=Never \
    --set operator.image.tag=dev \
    --set eval.image.pullPolicy=Never \
    --set eval.image.tag=dev \
    --set csi.image.pullPolicy=Never \
    --set csi.image.tag=dev \
    --set runner.image.tag=dev \
    --wait --timeout 180s

ok "All deployments ready"

# ── Step 6: Check component health ─────────────────────
info "Checking component health..."
kubectl -n "$NAMESPACE" get pods -o wide

# ── Step 7: Create test namespace ───────────────────────
info "Creating test namespace..."
kubectl create namespace "$E2E_NS" 2>/dev/null || true

# ── Step 8: Deploy test workload ────────────────────────
info "Creating NiphasWorkload (darkhttpd from nixpkgs)..."
kubectl apply -f hack/test-workload.yaml

# ── Step 9: Wait for evaluation ─────────────────────────
info "Waiting for evaluation (this fetches nixpkgs — may be slow first time)..."
for i in $(seq 1 90); do
    PHASE=$(kubectl -n "$E2E_NS" get niphasworkload test-darkhttpd -o jsonpath='{.status.phase}' 2>/dev/null || echo "Pending")
    case "$PHASE" in
        Running)
            ok "Workload phase: Running"
            break
            ;;
        Failed)
            echo ""
            fail "Workload phase: Failed"
            ;;
        *)
            printf "\r  phase: %-20s (${i}/90, poll every 10s)" "$PHASE"
            sleep 10
            ;;
    esac
done
echo ""

if [ "$PHASE" != "Running" ]; then
    echo ""
    info "Workload status:"
    kubectl -n "$E2E_NS" get niphasworkload test-darkhttpd -o yaml | grep -A 30 "^status:"
    info "Pod status:"
    kubectl -n "$E2E_NS" get pods -o wide
    info "Pod describe:"
    kubectl -n "$E2E_NS" describe pods
    info "Operator logs:"
    kubectl -n "$NAMESPACE" logs -l app.kubernetes.io/name=niphas-operator --tail=30
    info "Eval logs:"
    kubectl -n "$NAMESPACE" logs -l app.kubernetes.io/name=niphas-eval --tail=30
    info "CSI logs:"
    kubectl -n "$NAMESPACE" logs -l app.kubernetes.io/name=niphas-csi --all-containers --tail=30
    fail "Workload did not reach Running phase (stuck at: $PHASE)"
fi

# ── Step 10: Verify the pod is actually running ─────────
info "Checking pod status..."
kubectl -n "$E2E_NS" get pods -o wide
POD=$(kubectl -n "$E2E_NS" get pods -l niphas.io/workload=test-darkhttpd -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
if [ -z "$POD" ]; then
    fail "No pod found for workload test-darkhttpd"
fi
ok "Pod running: $POD"

# ── Step 11: Verify nix store mount ─────────────────────
info "Checking /nix/store mount inside pod..."
kubectl -n "$E2E_NS" exec "$POD" -- ls /nix/store/ | head -5
ok "Nix store is mounted"

# ── Step 12: Test the service ───────────────────────────
info "Port-forwarding to test service..."
kubectl -n "$E2E_NS" port-forward "svc/test-darkhttpd" 18080:80 &
PF_PID=$!
sleep 3

HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' http://localhost:18080/ 2>/dev/null || echo "000")
kill $PF_PID 2>/dev/null || true

if [ "$HTTP_CODE" = "200" ] || [ "$HTTP_CODE" = "403" ]; then
    # darkhttpd returns 403 for empty dir listing (no index.html), which is fine
    ok "HTTP service responding (status: $HTTP_CODE)"
else
    fail "HTTP service not responding (status: $HTTP_CODE)"
fi

# ── Step 13: Show workload status ───────────────────────
info "Final workload status:"
kubectl -n "$E2E_NS" get niphasworkload test-darkhttpd -o jsonpath='{.status}' | python3 -m json.tool 2>/dev/null || \
    kubectl -n "$E2E_NS" get niphasworkload test-darkhttpd -o yaml | grep -A 30 "^status:"

echo ""
ok "═══════════════════════════════════════════════"
ok " FULL E2E TEST PASSED"
ok " flake eval → closure → CSI mount → service"
ok "═══════════════════════════════════════════════"
