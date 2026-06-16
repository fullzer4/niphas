# Niphas -- Eval Webhook Design

niphas-eval is an HTTP webhook that evaluates Nix flakes via the Nix C API
(in-process FFI) and resolves closures via binary cache HTTP. It is called
by the operator during reconciliation.

## Architecture

```
operator                  niphas-eval                    binary cache
   |                          |                              |
   | POST /evaluate           |                              |
   |  {flakeRef, attribute}   |                              |
   |------------------------->|                              |
   |                          |                              |
   |                          | 1. validate flakeRef         |
   |                          |    against allowlist          |
   |                          |                              |
   |                          | 2. nix eval via C FFI        |
   |                          |    (in-process, no Jobs)     |
   |                          |    -> outPath, name,         |
   |                          |       mainProgram            |
   |                          |                              |
   |                          | 3. resolve closure           |
   |                          |    GET /<hash>.narinfo  ---->|
   |                          |    parse References     <----|
   |                          |    (recursive, parallel)     |
   |                          |                              |
   |  {storePath, closure}    |                              |
   |<-------------------------|                              |
```

niphas-eval runs as a Deployment (not DaemonSet). Typically 2 replicas
for availability. Stateless except for the Nix eval cache (`/nix/store`
on a PVC or hostPath).

## HTTP API

### POST /evaluate

Request:

```json
{
  "flakeRef": "github:myorg/myapp",
  "attribute": "packages.x86_64-linux.default",
  "revision": "a1b2c3d4e5f6",
  "binaryCache": "https://cache.company.com"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `flakeRef` | string | yes | Flake reference |
| `attribute` | string | yes | Output attribute path |
| `revision` | string | no | Git revision to pin |
| `binaryCache` | string | no | Override binary cache URL |

Response (success, 200):

```json
{
  "storePath": "/nix/store/abc123-myapp-1.0.0",
  "name": "myapp-1.0.0",
  "mainProgram": "myapp",
  "closurePaths": [
    "/nix/store/abc123-myapp-1.0.0",
    "/nix/store/def456-glibc-2.38",
    "/nix/store/ghi789-openssl-3.1"
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `storePath` | string | Resolved output path |
| `name` | string | Derivation name (from `drv.name`) |
| `mainProgram` | string or null | From `meta.mainProgram`. Used for command auto-detection |
| `closurePaths` | Vec<string> | Full transitive closure (root + all references) |

Response (error, 4xx/5xx):

```json
{
  "error": "FlakeNotAllowed",
  "message": "github:evil/repo not in allowlist"
}
```

| HTTP Status | Error code | Meaning |
|-------------|-----------|---------|
| 403 | `FlakeNotAllowed` | flakeRef not in allowlist |
| 422 | `EvalFailed` | Nix evaluation error |
| 408 | `EvalTimeout` | Evaluation exceeded timeout |
| 404 | `StorePathNotCached` | NAR not found in any binary cache |
| 502 | `ClosureResolutionFailed` | Could not resolve all closure paths |
| 500 | `InternalError` | Unexpected error |

### GET /healthz

Returns 200 if the process is running and the Nix evaluator is initialized.

### GET /readyz

Returns 200 if the evaluator has completed at least one successful eval
(warm cache). Returns 503 during cold start.

## Nix C FFI integration

niphas-eval links against the Nix C libraries (`libexpr-c`, `libstore-c`,
`libflake-c`) at build time via `nix-bindings-rust`. The evaluator runs
in-process, not in a separate process or container.

### Lifecycle

```rust
// On startup (once)
let nix_ctx = nix::Context::new()?;
let store = nix::Store::open(&nix_ctx, None)?;

// Configure evaluator settings
let mut settings = nix::EvalSettings::default();
settings.sandbox = true;
settings.restrict_eval = true;
settings.allowed_uris = config.allowed_uris.clone();
settings.allow_import_from_derivation = false;
settings.max_jobs = 0;

let state = nix::EvalStateBuilder::new(&store)?
    .with_settings(&settings)
    .build()?;

// Per-request (shared state, concurrent via Arc)
let state = Arc::new(state);
```

The `EvalState` is initialized once and shared across requests. The Nix
C API is internally thread-safe for evaluation (each eval call acquires
internal locks). Concurrent evaluations of different flakes are safe.

### Evaluation call

```rust
fn evaluate(
    state: &EvalState,
    flake_ref: &str,
    attribute: &str,
    revision: Option<&str>,
) -> Result<EvalResult, EvalError> {
    // Construct flake reference with optional revision
    let pinned_ref = match revision {
        Some(rev) => format!("{}/{}", flake_ref, rev),
        None => flake_ref.to_string(),
    };

    // Build Nix expression that extracts what we need
    let expr = format!(
        r#"let drv = (builtins.getFlake "{}").{}; in {{
            outPath = drv.outPath;
            name = drv.name;
            mainProgram = drv.meta.mainProgram or null;
        }}"#,
        pinned_ref, attribute
    );

    // Evaluate with timeout
    let value = tokio::time::timeout(
        Duration::from_secs(300),
        tokio::task::spawn_blocking(move || {
            state.eval_from_string(&expr, "<niphas-eval>")
        }),
    ).await
    .map_err(|_| EvalError::Timeout)??;

    let attrs = value.require_attrs()?;

    Ok(EvalResult {
        store_path: attrs.get("outPath")?.require_string()?,
        name: attrs.get("name")?.require_string()?,
        main_program: attrs.get("mainProgram")?.as_string().ok(),
    })
}
```

### Why spawn_blocking

Nix evaluation is CPU-bound and blocks the thread. Running it on
tokio's blocking thread pool prevents stalling the async runtime.
The eval call can take 1-15 seconds for cold evaluations.

### Eval settings

| Setting | Value | Rationale |
|---------|-------|-----------|
| `sandbox` | `true` | Restrict filesystem access during eval |
| `restrict_eval` | `true` | Only allow access to explicitly allowed URIs |
| `allowed_uris` | `["https://github.com", "https://cache.nixos.org"]` | Configurable via ConfigMap |
| `allow_import_from_derivation` | `false` | **Critical**: blocks IFD (arbitrary code execution during eval) |
| `max_jobs` | `0` | No builds, eval only. Even if IFD is somehow bypassed, no builder is available |

See [`SECURITY_DESIGN.md`](SECURITY_DESIGN.md) for the full 4-layer
defense model.

## Flake allowlist

Before evaluation, the webhook validates the flakeRef against a
deny-by-default allowlist.

```rust
fn validate_flake_ref(flake_ref: &str, allowlist: &[String]) -> Result<(), EvalError> {
    for pattern in allowlist {
        if matches_glob(flake_ref, pattern) {
            return Ok(());
        }
    }
    Err(EvalError::FlakeNotAllowed(flake_ref.into()))
}
```

### Glob matching rules

| Pattern | Matches | Does not match |
|---------|---------|----------------|
| `github:myorg/*` | `github:myorg/myapp`, `github:myorg/lib` | `github:other/repo` |
| `github:nixos/nixpkgs` | `github:nixos/nixpkgs` | `github:nixos/nix` |
| `github:*/*` | any GitHub flake | `path:/local/flake` |

### Config

```yaml
# niphas-eval ConfigMap
apiVersion: v1
kind: ConfigMap
metadata:
  name: niphas-eval-config
  namespace: niphas-system
data:
  config.yaml: |
    allowedFlakeOrigins:
      - "github:myorg/*"
      - "github:nixos/nixpkgs"
    evalTimeout: 300s
    allowedUris:
      - "https://github.com"
      - "https://cache.nixos.org"
    binaryCaches:
      - url: "https://cache.company.com"
        publicKey: "cache.company.com-1:..."
      - url: "https://cache.nixos.org"
        publicKey: "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
    closureResolution:
      concurrency: 32
      timeout: 60s
```

## Closure resolution

After evaluation, niphas-eval resolves the full transitive closure by
walking `.narinfo` files from the binary cache. This is pure HTTP --
no Nix needed.

### Algorithm

```rust
async fn resolve_closure(
    cache: &CacheClient,
    root_path: &str,
) -> Result<Vec<String>, EvalError> {
    let mut closure = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    queue.push_back(root_path.to_string());
    visited.insert(root_path.to_string());

    while let Some(path) = queue.pop_front() {
        // Fetch .narinfo for this path
        let hash = store_path_hash(&path)?;
        let narinfo = cache.fetch_narinfo(&hash).await?;

        closure.push(path);

        // Add unvisited references to queue
        for reference in &narinfo.references {
            let ref_path = format!("/nix/store/{}", reference);
            if visited.insert(ref_path.clone()) {
                queue.push_back(ref_path);
            }
        }
    }

    Ok(closure)
}
```

### Parallel resolution

The BFS is parallelized: multiple `.narinfo` fetches happen concurrently
(up to `closureResolution.concurrency`, default 32).

```rust
async fn resolve_closure_parallel(
    cache: &CacheClient,
    root_path: &str,
    concurrency: usize,
) -> Result<Vec<String>, EvalError> {
    let mut closure = Vec::new();
    let mut visited = HashSet::new();
    let mut pending = FuturesUnordered::new();

    // Seed with root
    visited.insert(root_path.to_string());
    pending.push(fetch_and_parse(cache, root_path));

    while let Some(result) = pending.next().await {
        let (path, narinfo) = result?;
        closure.push(path);

        for reference in &narinfo.references {
            let ref_path = format!("/nix/store/{}", reference);
            if visited.insert(ref_path.clone()) {
                if pending.len() < concurrency {
                    pending.push(fetch_and_parse(cache, &ref_path));
                }
            }
        }
    }

    Ok(closure)
}
```

### What if root .narinfo returns 404?

The store path was evaluated but never built (or not pushed to the
binary cache). This is the `StorePathNotCached` error:

```
The workload cannot be deployed. Build in CI and push to binary cache first.

  nix build .#packages.x86_64-linux.default
  nix copy --to https://cache.company.com ./result
```

The operator sets:
```yaml
status:
  phase: Failed
  conditions:
    - type: Evaluated
      status: "False"
      reason: StorePathNotCached
      message: "/nix/store/abc123-myapp-1.0.0 not found in any binary cache"
```

## Eval cache

The Nix evaluator maintains its own internal cache:

| Cache | Location | Contents |
|-------|----------|----------|
| Eval cache | `/nix/store` (PVC) | Fetched flake inputs, evaluated derivations |
| Git cache | `~/.cache/nix/` | Cloned git repos (flake sources) |

### Performance from cache

| Scenario | Time | Why |
|----------|------|-----|
| First eval of a new flake | 5-15s | Fetches flake source + inputs from git |
| Same flake, new revision | 1-3s | Inputs cached, re-evaluates expression |
| Same flake, same revision | <100ms | Eval cache hit |
| Redeploy (no spec change) | skipped | Operator checks `observedGeneration == generation` |

The eval cache persists across pod restarts via PVC. Cold start (first
eval ever in a fresh pod) fetches nixpkgs and inputs -- this is expected
and documented in startup logs.

### Cache sizing

The eval cache grows as new flakes are evaluated. nixpkgs alone is ~1 GB
in the eval cache. For most deployments, 10-20 GB PVC is sufficient.
The Nix evaluator handles its own GC internally.

## Concurrency model

niphas-eval uses Axum with tokio multi-thread runtime:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    niphas_core::telemetry::init_tracing();

    // Load config
    let config = Config::load()?;

    // Initialize Nix evaluator (once)
    let evaluator = Arc::new(Evaluator::new(&config)?);

    // Build router
    let app = Router::new()
        .route("/evaluate", post(handlers::evaluate))
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .layer(Extension(evaluator))
        .layer(Extension(config));

    // Serve
    let addr = SocketAddr::from(([0, 0, 0, 0], 8443));
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
```

### Concurrent eval requests

Multiple eval requests can arrive simultaneously (e.g. operator creates
multiple NiphasWorkloads). Each request runs on its own tokio task:

1. Allowlist check: instant, no blocking
2. Nix eval: `spawn_blocking` (CPU-bound, runs on blocking thread pool)
3. Closure resolution: async HTTP calls (concurrent `.narinfo` fetches)

The Nix C API's `EvalState` is behind an `Arc`. The C API internally
serializes evaluation (one eval at a time via internal mutex). This
means concurrent eval requests are queued at the C level. For true
parallelism, niphas-eval would need multiple `EvalState` instances --
this is a future optimization.

### Request timeout

Each handler wraps the evaluation in a tokio timeout:

```rust
async fn evaluate(
    Extension(evaluator): Extension<Arc<Evaluator>>,
    Json(req): Json<EvalRequest>,
) -> Result<Json<EvalResponse>, AppError> {
    let result = tokio::time::timeout(
        evaluator.config.eval_timeout,
        evaluator.evaluate(&req),
    ).await
    .map_err(|_| AppError::EvalTimeout)?
    .map_err(AppError::from)?;

    Ok(Json(result))
}
```

## Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: niphas-eval
  namespace: niphas-system
spec:
  replicas: 2
  selector:
    matchLabels:
      app: niphas-eval
  template:
    metadata:
      labels:
        app: niphas-eval
    spec:
      serviceAccountName: niphas-eval
      securityContext:
        runAsNonRoot: true
        runAsUser: 65534
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: niphas-eval
          image: ghcr.io/fullzer4/niphas-eval:latest
          ports:
            - containerPort: 8443
              name: webhook
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop: ["ALL"]
          resources:
            requests:
              cpu: "500m"
              memory: "1Gi"
            limits:
              cpu: "2"
              memory: "4Gi"
          volumeMounts:
            - name: nix-store
              mountPath: /nix/store
            - name: eval-config
              mountPath: /etc/niphas
              readOnly: true
          livenessProbe:
            httpGet:
              path: /healthz
              port: webhook
            initialDelaySeconds: 10
          readinessProbe:
            httpGet:
              path: /readyz
              port: webhook
            initialDelaySeconds: 30    # cold start may take time
      volumes:
        - name: nix-store
          persistentVolumeClaim:
            claimName: niphas-eval-store
        - name: eval-config
          configMap:
            name: niphas-eval-config
---
apiVersion: v1
kind: Service
metadata:
  name: niphas-eval
  namespace: niphas-system
spec:
  selector:
    app: niphas-eval
  ports:
    - port: 8443
      targetPort: webhook
```

### Why PVC for /nix/store

The Nix eval cache (fetched flake inputs, evaluated derivations) must
persist across pod restarts. Without PVC, every restart triggers a cold
eval (fetch nixpkgs from git, ~5-15 seconds). With PVC, warm evals
take <100ms.

The PVC can be `ReadWriteOnce` -- only one pod writes at a time (the
Nix evaluator serializes writes internally). If running 2 replicas,
each gets its own PVC (`volumeClaimTemplates` in a StatefulSet) or
they share via `ReadWriteMany` if the storage class supports it.

Alternative: hostPath at `/var/lib/niphas/eval-cache/`. Simpler but
ties the eval pod to a specific node. PVC is preferred for portability.
