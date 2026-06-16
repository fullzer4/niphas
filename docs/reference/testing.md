# Testing Strategy

Testability patterns and test architecture for the niphas codebase.

## Current State

~4,500 lines of Rust, 32 tests covering only parsing and crypto. Zero tests for operator, CSI, and eval pipeline. Root cause: logic mixed with IO, making it untestable without real infrastructure.

## Approach: Sans-IO + Trait Injection

Separate pure logic from side effects. Extract traits for IO boundaries, use fakes (not mocks) in tests.

### Three Core Traits

```rust
// Binary cache interaction
trait BinaryCacheClient {
    async fn fetch_narinfo(&self, hash: &str) -> Result<NarInfo>;
    async fn fetch_nar(&self, url: &str) -> Result<Bytes>;
}

// CSI mount operations
trait MountOps {
    fn bind_mount(&self, source: &Path, target: &Path) -> Result<()>;
    fn unmount(&self, target: &Path) -> Result<()>;
}

// Nix evaluation
trait NixEval {
    async fn evaluate(&self, req: &EvalRequest) -> Result<EvalResult>;
}
```

Fakes implement simplified logic — they work rather than just verify calls. More robust to refactoring.

## Test Tiers

### Tier 1 — Fast (every commit)

- No network, no K8s, no root, no nix
- Fakes, builders, snapshots, property tests
- Target: less than 5s, ~100 tests
- Run: `cargo test --workspace`

### Tier 2 — Integration (CI only)

- Requires kind cluster, nix binary, root
- Real CSI mounts, actual eval calls
- Run: `cargo test --workspace -- --ignored`

## Patterns

### Test Fixtures — Builder Pattern

```rust
WorkloadBuilder::new("test-app")
    .flake_ref("github:test/app")
    .attribute("packages.x86_64-linux.default")
    .replicas(3)
    .build()
```

Sensible defaults, chainable methods, feature-gated `testutils` module.

### Snapshot Testing (insta)

Capture complex outputs (JSON Kubernetes resources) as snapshots. First run creates the snapshot, subsequent runs compare. Use redactions for non-deterministic fields (UIDs, timestamps).

### Property Testing (proptest)

Generate valid inputs and test invariants:
- Roundtrip: `parse(display(x)) == x`
- No-panic: random inputs don't crash parsers
- Forbidden patterns: outputs never contain injection vectors

### kube-rs Operator Testing

Use `tower_test` mock client. Inject `mock::Handle` into `kube::Client`. Verify API calls in an async task.

### Axum Handler Testing

Use `tower::ServiceExt::oneshot` to call handlers directly without HTTP transport.

## Expected Results

| Metric | Before | After |
|--------|--------|-------|
| Tier 1 tests | 32 | ~100 |
| Crates with tests | 2/5 | 4/5 |
| Tier 1 runtime | less than 1s | less than 5s |
| Traits extracted | 0 | 3 |
| Snapshot tests | 0 | ~15 |
| Property tests | 0 | ~8 |
