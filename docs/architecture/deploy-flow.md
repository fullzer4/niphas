# Deploy Flow

From a single CRD to a running workload — no Dockerfile, no registry, no image tags.

## User Experience

```yaml
apiVersion: niphas.io/v1
kind: NiphasWorkload
metadata:
  name: hello
spec:
  flakeRef: "github:NixOS/nixpkgs"
  attribute: "hello"
```

That's it. The operator evaluates the flake reference, resolves the closure, creates a Deployment with CSI volumes, and the workload runs.

## Production Example

```yaml
apiVersion: niphas.io/v1
kind: NiphasWorkload
metadata:
  name: my-app
spec:
  flakeRef: "github:myorg/myapp"
  attribute: "packages.x86_64-linux.default"
  revision: "abc123"
  replicas: 3
  args: ["--port", "8080"]
  env:
    - name: DATABASE_URL
      valueFrom:
        secretKeyRef:
          name: db-credentials
          key: url
  ports:
    - containerPort: 8080
  resources:
    requests:
      cpu: "100m"
      memory: "128Mi"
    limits:
      cpu: "500m"
      memory: "512Mi"
  livenessProbe:
    httpGet:
      path: /healthz
      port: 8080
  service:
    type: ClusterIP
    ports:
      - port: 80
        targetPort: 8080
  ingress:
    enabled: true
    className: nginx
    hosts:
      - host: my-app.example.com
        paths:
          - path: /
            pathType: Prefix
```

## Build Model

**CI builds, the cluster evaluates and fetches** (never builds).

1. CI runs `nix build` and pushes to a binary cache
2. `niphas-eval` runs `nix eval` to resolve the store path deterministically
3. If the path isn't cached, the workload fails with `StorePathNotCached`

## End-to-End Sequence

1. User creates `NiphasWorkload` CR
2. Operator detects new resource, sets phase `Evaluating`
3. Operator calls eval webhook with `flakeRef`, `attribute`, `revision`
4. Eval validates against flake allowlist (deny-by-default)
5. Eval resolves store path via Nix C FFI
6. Eval resolves full closure via binary cache `.narinfo` (parallel BFS)
7. Operator receives `storePath`, `closurePaths`, `mainProgram`
8. Operator creates Deployment, Service, Ingress (server-side apply)
9. Kubelet schedules pods on nodes with `niphas.io/store=true` label
10. CSI driver fetches NARs (cache → mesh → binary cache)
11. Pod starts with Nix binary as entrypoint
12. Operator sets phase `Running`

## Command Resolution

If `spec.command` is omitted:

1. Eval checks `meta.mainProgram` in the derivation
2. If not set, lists `$out/bin/`
3. If exactly one binary found, uses it
4. Otherwise, fails with an error

## Updates and Rollouts

Changing the spec increments `metadata.generation`:

1. Operator detects `observedGeneration < generation`
2. Re-evaluates if `flakeRef`, `attribute`, or `revision` changed
3. Patches Deployment with new CSI `volumeAttributes` and command
4. Kubernetes performs a rolling update

## Multi-Architecture

The `architectures` field triggers parallel evaluation:

- `attribute` must contain `{arch}` placeholder (e.g., `packages.{arch}.default`)
- One Deployment created per architecture
- A single Service selects all pods via shared labels
