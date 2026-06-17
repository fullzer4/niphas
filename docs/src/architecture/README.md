# Architecture & Project Structure

Niphas is a Nix-native platform for Kubernetes, organized as a Rust workspace with five crates.

## Directory Layout

```
niphas/
├── crates/
│   ├── niphas-core/      # shared library (CRDs, Nix formats, cache client)
│   ├── niphas-eval/      # Nix evaluator webhook (C FFI)
│   ├── niphas-operator/  # Kubernetes operator
│   ├── niphas-csi/       # CSI node driver
│   └── niphas-mesh/      # P2P NAR distribution (libp2p)
├── proto/                # vendored CSI gRPC proto
├── nix/                  # flake-parts modules
├── chart/                # Helm chart
└── flake.nix             # Nix build with Crane
```

## Core Crate

`niphas-core` is the shared library used by all other crates:

| Module | Purpose |
|--------|---------|
| `crd.rs` | `NiphasWorkload` CRD definition |
| `hash.rs` | Nix-base32 encoding, SHA-256, `NixHash` |
| `nar.rs` | NAR streaming parser and extractor |
| `narinfo.rs` | `.narinfo` parser |
| `store_path.rs` | Store path validation |
| `cache_client.rs` | Binary cache HTTP client |
| `signature.rs` | Ed25519 signature verification |
| `closure.rs` | Parallel closure resolution (BFS) |

## Build System

The Nix build uses [Crane](https://crane.dev) with a two-phase strategy:

1. **`buildDepsOnly`** — cacheable dependency layer (only `Cargo.lock` and `Cargo.toml` files)
2. **`buildPackage`** — per-crate build on top of cached deps

OCI images are produced via `dockerTools.buildLayeredImage` with minimal contents (static musl binaries).

## Key Design Decisions

- **Axum** over actix-web — native tower/hyper ecosystem, async-first
- **Vendored CSI proto** — sandboxed Nix builds can't fetch at build time
- **Selective libp2p** — only TCP, QUIC, Noise, Yamux, Gossipsub features enabled
- **Workspace dependency inheritance** — single source of truth in root `Cargo.toml`

## Observability

- Structured JSON logging via `tracing-subscriber`
- Optional OpenTelemetry integration (zero overhead without `OTEL_*` env vars)
- Distributed tracing via W3C TraceContext propagation

## Input Validation

Three-layer defense against Nix injection:

1. **Regex validation** — `flake_ref`, `attribute`, `revision` validated before use
2. **Flake allowlist** — deny-by-default glob matching
3. **Nix sandbox** — `restrict_eval=true`, `allow_import_from_derivation=false`, `max_jobs=0`
