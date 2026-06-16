# Eval Webhook Design

The niphas-eval service evaluates Nix flake references and resolves closures via the Nix C FFI — no subprocess, no container.

## Architecture

```
  operator ──POST /evaluate──► niphas-eval
                                  │
                          ┌───────┼────────┐
                          │       │        │
                      validate  nix eval  closure
                      allowlist  C FFI   resolution
                                          │
                                    binary cache
                                   GET /<hash>.narinfo
```

## HTTP API

### `POST /evaluate`

**Request:**
```json
{
  "flakeRef": "github:myorg/myapp",
  "attribute": "packages.x86_64-linux.default",
  "revision": "abc123",
  "binaryCache": "https://cache.example.com"
}
```

**Response (200):**
```json
{
  "storePath": "/nix/store/abc...-myapp-1.0",
  "name": "myapp-1.0",
  "mainProgram": "myapp",
  "closurePaths": [
    "/nix/store/abc...-myapp-1.0",
    "/nix/store/def...-glibc-2.38"
  ]
}
```

**Error responses:**

| Code | Error | Meaning |
|------|-------|---------|
| 403 | `FlakeNotAllowed` | Flake origin not in allowlist |
| 404 | `StorePathNotCached` | Path not found in binary cache |
| 408 | `EvalTimeout` | Evaluation exceeded timeout |
| 422 | `EvalFailed` | Nix evaluation error |
| 502 | `ClosureResolutionFailed` | Cannot resolve full closure |

### Health Endpoints

- `GET /healthz` — process running
- `GET /readyz` — evaluator initialized (503 during cold start)

## Nix C FFI

niphas-eval links directly against the Nix C libraries (`libexpr-c`, `libstore-c`, `libflake-c`) via `nix-bindings-rust`. The evaluator runs in-process.

- `EvalState` initialized once, shared across requests (thread-safe C API)
- CPU-bound evaluation dispatched via `spawn_blocking`
- Nix C API serializes evaluations internally (internal mutex)

## Eval Settings

```
sandbox = true
restrict_eval = true
allowed_uris = [github.com, cache.nixos.org]
allow_import_from_derivation = false  # critical
max_jobs = 0
```

## Flake Allowlist

Deny-by-default glob matching. Patterns support `*` wildcards. Validated **before** the evaluator is invoked.

## Closure Resolution

After evaluation, the eval service resolves the full transitive closure via binary cache HTTP:

1. Fetch `.narinfo` for the root store path
2. Parse `References` field
3. Recursively fetch `.narinfo` for each reference (parallel BFS)
4. Return complete closure list

Uses `FuturesUnordered` with bounded concurrency (default 32).

## Eval Cache

The `/nix/store` PVC persists fetched flake inputs and evaluated derivations:

| Scenario | Latency |
|----------|---------|
| Cold eval (first time) | 5–15s |
| Same revision | 1–3s |
| Same flake, cached inputs | less than 100ms |

**PVC sizing:** 10–20 GB typical (nixpkgs alone is ~1 GB).

## Deployment

- 2 replicas recommended
- PVC for `/nix/store` (eval cache must persist)
- Non-root, read-only rootfs, no capabilities
- Resource requests: 500m CPU, 1Gi RAM
