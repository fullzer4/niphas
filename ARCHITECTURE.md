# Niphas – Architecture & Project Structure

## Directory Layout

```
niphas/
├── Cargo.toml                  # Workspace root
├── Cargo.lock                  # Shared lockfile
├── flake.nix                   # Nix flake (crane-based)
├── flake.lock
├── rust-toolchain.toml         # Pin Rust version
├── .cargo/
│   └── config.toml             # Cargo build settings
├── deny.toml                   # cargo-deny (licenses, advisories)
├── proto/
│   └── csi/
│       └── v1/
│           └── csi.proto       # CSI spec protobuf (vendored)
├── crates/
│   ├── niphas-core/            # Shared library
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── crd.rs          # NiphasWorkload CRD (kube-derive)
│   │       ├── nix/
│   │       │   ├── mod.rs
│   │       │   ├── nar.rs          # NAR archive parser + extractor
│   │       │   ├── narinfo.rs      # .narinfo text parser
│   │       │   ├── hash.rs         # Nix hashing (SHA-256, Nix-base32)
│   │       │   ├── signature.rs    # Ed25519 signature verification
│   │       │   ├── store_path.rs   # Store path parsing + validation
│   │       │   ├── cache_client.rs # Binary cache HTTP client
│   │       │   └── closure.rs      # Recursive closure resolution
│   │       ├── config.rs       # Shared config loading
│   │       ├── eval.rs         # Eval request/response types + input validation
│   │       ├── error.rs        # Error types (thiserror)
│   │       ├── telemetry.rs    # Tracing init with opt-in OTEL (TelemetryGuard)
│   │       └── testutils.rs    # Test fixtures (feature-gated)
│   ├── niphas-eval/            # Webhook binary (Nix eval via subprocess)
│   │   └── src/
│   │       ├── main.rs
│   │       ├── handlers.rs     # Axum route handlers
│   │       ├── evaluator.rs    # Nix eval subprocess + closure resolution
│   │       ├── allowlist.rs    # Flake origin validation
│   │       └── error.rs        # Eval-specific error types
│   ├── niphas-operator/        # Operator binary
│   │   └── src/
│   │       ├── main.rs
│   │       ├── reconciler.rs   # Reconciler for NiphasWorkload
│   │       ├── resources.rs    # K8s resource construction (Deployment, Service, etc.)
│   │       ├── context.rs      # Shared operator context (kube client, config)
│   │       ├── eval.rs         # Eval webhook client
│   │       ├── health.rs       # Health/readiness probes (Axum)
│   │       └── error.rs        # Operator-specific error types
│   ├── niphas-csi/             # CSI driver binary
│   │   ├── build.rs            # tonic-build for CSI proto
│   │   └── src/
│   │       ├── main.rs
│   │       ├── identity.rs     # CSI Identity service
│   │       ├── node.rs         # CSI Node service (mount/unmount)
│   │       ├── mount.rs        # Nix closure mount logic
│   │       └── cache.rs        # NAR cache management
│   └── niphas-mesh/            # P2P binary (early stage)
│       └── src/
│           └── main.rs
└── manifests/                  # K8s manifests
    ├── crd.yaml                # Generated from niphas-core
    ├── operator.yaml
    ├── csi-driver.yaml
    └── eval-webhook.yaml
```

## Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members = [
    "crates/niphas-core",
    "crates/niphas-eval",
    "crates/niphas-operator",
    "crates/niphas-csi",
    "crates/niphas-mesh",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"
repository = "https://github.com/fullzer4/niphas"
rust-version = "1.85"

[workspace.dependencies]
# Kubernetes
kube = { version = "3.1", features = ["runtime", "derive", "client"] }
k8s-openapi = { version = "0.27", features = ["latest", "schemars"] }
kube-runtime = "3.1"
kube-leader-election = "0.43"

# Async
tokio = { version = "1.52", features = ["rt-multi-thread", "net", "time", "signal", "macros", "process"] }

# Web
axum = "0.8"
tower = "0.5"
tower-http = { version = "0.6", features = ["trace"] }

# gRPC (CSI)
tonic = "0.14"
tonic-build = "0.14"
tonic-prost-build = "0.14"
tonic-prost = "0.14"
prost = "0.14"
prost-types = "0.14"

# P2P
libp2p = { version = "0.56", features = [
    "tcp", "quic", "noise", "yamux",
    "dns", "identify", "gossipsub", "request-response",
    "tokio", "macros",
] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"

# Zero-copy (P2P protocol)
rkyv = "0.8"

# Cryptography / hashing
sha2 = "0.10"
ed25519-dalek = { version = "2", features = ["pkcs8"] }
base64 = "0.22"

# HTTP client
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "gzip"] }

# Error handling
thiserror = "2"
anyhow = "1"

# Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json", "registry"] }
tracing-opentelemetry = "0.30"
opentelemetry = "0.29"
opentelemetry_sdk = { version = "0.29", features = ["rt-tokio"] }
opentelemetry-otlp = { version = "0.29", features = ["grpc-tonic", "trace", "logs"] }
opentelemetry-appender-tracing = "0.29"

# Schema / CRD
schemars = "1"
garde = { version = "0.23", features = ["derive", "serde"] }

# Config
figment = { version = "0.10", features = ["env", "toml", "yaml"] }

# Allocator
tikv-jemallocator = { version = "0.7", features = ["profiling", "stats"] }

# Performance primitives
smallvec = "1"
compact_str = "0.9"
bumpalo = { version = "3", features = ["collections"] }
memmap2 = "0.9"
bytes = "1"

# Object pooling
lockfree-object-pool = "0.1"

# Async utilities
futures = "0.3"
async-compression = { version = "0.4", features = ["tokio", "zstd", "xz", "bzip2"] }

# Time
time = { version = "0.3", features = ["formatting"] }

# Internal
niphas-core = { path = "crates/niphas-core" }
```

## niphas-core (shared crate)

Contains everything that two or more binaries need:

| Module | Contents | Consumers |
|---|---|---|
| `crd.rs` | `NiphasWorkload` CRD struct (`#[derive(CustomResource)]`) | operator, eval, CSI |
| `nix/nar.rs` | NAR archive streaming parser + extractor | CSI, mesh |
| `nix/narinfo.rs` | `.narinfo` text parser | CSI, mesh, eval |
| `nix/hash.rs` | Nix hashing (SHA-256, Nix-base32 encoding) | CSI, mesh, eval |
| `nix/signature.rs` | Ed25519 NAR signature verification | CSI, mesh |
| `nix/store_path.rs` | `StorePath` parsing + validation | all |
| `nix/cache_client.rs` | Binary cache HTTP client | CSI, eval |
| `nix/closure.rs` | Recursive closure resolution via HTTP | eval |
| `eval.rs` | Eval request/response types, input validation (flake_ref, attribute, revision) | operator, eval |
| `error.rs` | `NiphasError` enum (thiserror) | all |
| `config.rs` | Shared config loading (figment) | all |
| `telemetry.rs` | `init_tracing()` with opt-in OTEL (traces + logs), `TelemetryGuard` | all |
| `testutils.rs` | Test fixtures: `WorkloadBuilder`, `FakeCacheClient` (feature-gated) | all (dev) |

Uses a `runtime` feature flag so lightweight consumers (CSI, mesh) can depend on it without pulling in `kube-runtime`:

```toml
[features]
default = []
runtime = ["kube/runtime"]
```

## CRD Definition (crd.rs)

```rust
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "niphas.io",
    version = "v1alpha1",
    kind = "NiphasWorkload",
    namespaced,
    status = "NiphasWorkloadStatus",
)]
pub struct NiphasWorkloadSpec {
    pub flake_ref: String,
    pub attribute: String,
    pub replicas: Option<i32>,
}

pub struct NiphasWorkloadStatus {
    pub observed_generation: Option<i64>,  // detect desync
    pub phase: String,
    pub store_path: Option<String>,
    pub closure_paths: Option<Vec<String>>,  // full closure for rescheduling
    pub ready_replicas: Option<i32>,
    pub conditions: Option<Vec<NiphasCondition>>,
}

pub struct NiphasCondition {
    pub type_: String,      // Evaluated, ClosureCached, Available, Progressing, Degraded
    pub status: String,     // True, False, Unknown
    pub reason: String,     // EvalSucceeded, NarFetchFailed, ReplicasReady
    pub message: String,
    pub last_transition_time: String,
    pub observed_generation: Option<i64>,
}
```

`closure_paths` is critical for resilience: when a pod is rescheduled to a new
node, the CSI driver knows exactly which store paths to fetch without depending
on `.narinfo` from an external cache. The CSI driver checks local cache first,
then mesh peers (if available), then binary cache HTTP. See `docs/RESILIENCE.md`.

## Per-Crate Key Dependencies

### niphas-eval
`niphas-core` (runtime), `kube`, `kube-runtime`, `k8s-openapi`, `axum`, `tower`, `tokio`, `serde`, `tracing`, `anyhow`, `reqwest` (closure resolution via binary cache HTTP). Nix evaluation runs via `tokio::process::Command("nix")` subprocess with sandbox flags. Future: in-process FFI via Nix C API.

### niphas-operator
`niphas-core` (runtime), `kube`, `kube-runtime`, `k8s-openapi`, `kube-leader-election` (Lease-based leader election), `axum` (health probes), `tokio`, `serde`, `tracing`, `anyhow`, `reqwest` (eval webhook client).

### niphas-csi
`niphas-core`, `tonic`, `prost`, `prost-types`, `tokio`, `tracing`, `anyhow`, `reqwest` (binary cache fallback), `sha2`, `ed25519-dalek`, `async-compression` (zstd/xz/bzip2 decompression).
Build dep: `tonic-build`, `tonic-prost-build` (compiles vendored CSI proto).

### niphas-mesh
`niphas-core`, `libp2p`, `tokio`, `tracing`, `anyhow`, `serde`, `async-compression` (zstd, NAR streaming), `futures`. Early stage -- only `main.rs` exists currently.

## Nix Build (flake.nix with Crane)

Stack: **rust-overlay + crane + flake-utils + nix-direnv**

Crane's two-phase strategy:
1. `buildDepsOnly` – builds all workspace dependencies (cacheable via Cachix/Attic)
2. `buildPackage` per crate – builds only source code against cached deps

Each crate produces a binary and an OCI image (`dockerTools.buildLayeredImage`).

The devShell includes: `kubectl`, `helm`, `cargo-deny`, `cargo-nextest`, `cargo-watch`, `protobuf`.

## Key Design Decisions

**Axum over actix-web** – tower middleware is composable and shared with kube-rs's own tower-based client. Axum is the framework used in kube-rs examples.

**Proto vendoring** – CSI protobuf lives in `proto/csi/` rather than being fetched at build time. Nix builds are sandboxed (no network access). Alternative: use the `k8s-csi` crate for pre-generated bindings.

**libp2p feature selection** – only `tcp`, `quic`, `noise`, `yamux`, `gossipsub`, `request-response`, `stream`. The full feature set pulls a massive dep tree. `stream` gives raw `AsyncRead`/`AsyncWrite` substreams for bulk NAR transfers.

**Workspace dependency inheritance** – every dep used by 2+ crates goes in `[workspace.dependencies]`. Members use `dep.workspace = true`. Prevents version drift.

**Single Cargo.lock** – all crates build against identical dependency versions.

## Observability

### Structured logging

All binaries emit JSON-structured logs on stdout via `tracing-subscriber`. Log level
is controlled by `RUST_LOG` env var (default: `info`). Every log line includes
timestamp, level, target, span context, and structured fields.

### OpenTelemetry (opt-in)

Telemetry is initialized in `niphas-core/src/telemetry.rs` via `init_tracing()`.
OTEL support is **opt-in** -- controlled entirely by standard OTEL SDK env vars.
Without them, behavior is identical to plain JSON logging with zero OTEL overhead.

| Env var | Effect |
|---------|--------|
| (none) | JSON logs on stdout only |
| `OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4317` | Adds OTLP trace exporter (spans) + OTLP log exporter |
| `OTEL_SERVICE_NAME=custom-name` | Overrides the default service name in the OTEL resource |
| `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf` | Uses HTTP instead of gRPC for OTLP export |

Any env var documented in the [OTEL SDK specification](https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/) is supported.

### Architecture

```
tracing-subscriber::registry
  |
  +-- fmt::layer().json()              (always: JSON logs on stdout)
  +-- OpenTelemetryLayer               (opt-in: OTLP trace spans)
  +-- OpenTelemetryTracingBridge       (opt-in: OTLP log export)
```

`init_tracing()` returns a `TelemetryGuard` that each binary holds until shutdown.
The guard flushes pending spans and logs on drop via `SdkTracerProvider::shutdown()`
and `SdkLoggerProvider::shutdown()`, preventing data loss on graceful exit.

```rust
// Every main.rs:
let _telemetry = niphas_core::telemetry::init_tracing("niphas-operator");
// guard lives until end of main, ensuring flush
```

### Distributed tracing

With OTEL enabled, every reconciliation, eval request, and CSI mount operation
produces trace spans that propagate across component boundaries. This enables
end-to-end visibility: operator -> eval webhook -> closure resolution -> CSI mount.

W3C TraceContext propagation between operator and eval can be added via
`tower-http` tracing layer + `opentelemetry-http` (future enhancement).

### Metrics (future)

The health server in each binary can expose a `/metrics` endpoint for Prometheus
scraping via `opentelemetry-prometheus`. The OTEL layer architecture supports
adding a metrics layer without changing the existing setup.

## Input Validation

### Threat: Nix expression injection

The eval service constructs Nix expressions by interpolating user-provided fields
(`flake_ref`, `attribute`, `revision`) into a `format!()` string that is passed
to `nix eval`. Without validation, characters like `"`, `\`, `$`, `;`, `(`, `)`
can break out of the string literal and inject arbitrary Nix code.

```rust
// evaluator.rs -- the interpolation point:
let expr = format!(
    r#"let drv = (builtins.getFlake "{pinned_ref}").{attribute}; in ..."#
);
```

The flake allowlist (glob matching) validates the **pattern** of the flake ref
but does not reject dangerous characters. A ref like
`github:myorg/app" ++ builtins.readFile /etc/shadow ++ "` could pass a
`github:myorg/*` glob and still inject code.

### Defense: strict input validation

Validation functions in `niphas-core/src/eval.rs` enforce character-level
restrictions before any interpolation happens:

| Field | Regex | Rejects |
|-------|-------|---------|
| `flake_ref` | `^[a-zA-Z][a-zA-Z0-9+\-\.]*:[a-zA-Z0-9/_\-\.]+$` | `"`, `\`, `$`, `;`, `(`, `)`, spaces, unicode |
| `attribute` | `^[a-zA-Z_][a-zA-Z0-9_\-]*(\.[a-zA-Z_][a-zA-Z0-9_\-]*)*$` | Anything that isn't a valid Nix attribute path |
| `revision` | `^[a-fA-F0-9]{6,40}$` | Non-hex characters, wrong length |

These run **before** the allowlist check and **before** any string interpolation.
Rejection at this layer is a 400 Bad Request, not a security event.

### Defense layers (eval pipeline)

```
1. Input validation    -- reject malformed flake_ref / attribute / revision
2. Flake allowlist     -- reject refs not matching configured patterns
3. Nix sandbox         -- restrict_eval=true, IFD=false, max_jobs=0
4. Container isolation -- non-root, no capabilities, read-only rootfs
5. Network policy      -- egress limited to git HTTPS + DNS only
```

See [`docs/SECURITY_DESIGN.md`](docs/SECURITY_DESIGN.md) for the full 4-layer
defense model on eval sandboxing.

## Leader Election

The operator must run with a single active reconciler to avoid duplicate eval
calls, SSA conflicts, and race conditions when multiple replicas process the
same `NiphasWorkload` simultaneously.

### Mechanism

Lease-based leader election via `coordination.k8s.io/v1` Lease object. Only the
leader runs the `Controller` reconciliation loop; standby replicas wait and
monitor the lease.

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Lease TTL | 15s | Time before a dead leader is considered gone |
| Renew interval | 5s | Leader heartbeat frequency |
| Retry interval | 5s | Non-leader polling frequency |

Failover takes at most ~15 seconds. Reconciliation is idempotent (SSA-based),
so brief dual-leader windows during transition are safe.

### RBAC

The operator ServiceAccount requires a namespace-scoped Role for Lease
management in `niphas-system`:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: niphas-operator-leader-election
  namespace: niphas-system
rules:
  - apiGroups: ["coordination.k8s.io"]
    resources: ["leases"]
    verbs: ["get", "list", "watch", "create", "update", "patch", "delete"]
```

### Readiness integration

The `/readyz` endpoint returns 200 only after leader election is resolved and
the controller is actively watching. During standby, the pod reports not-ready
so the health Service does not route traffic to it.

See [`docs/OPERATOR.md`](docs/OPERATOR.md) for the full operator design
including reconciliation state machine, error handling, and watches.
See [`docs/RESILIENCE.md`](docs/RESILIENCE.md) for failure scenarios and
recovery timelines.

## CRD Generation

A small binary or example in `niphas-core` that outputs `serde_yaml::to_string(&NiphasWorkload::crd())` keeps `manifests/crd.yaml` in sync with the Rust types. Wire into CI.
