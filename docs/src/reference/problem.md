# Known Issues

Tracked problems and their planned fixes, ordered by priority.

## Critical

### P1 — Nix Expression Injection

`flake_ref` and `attribute` are interpolated into Nix expressions without character validation. A crafted input could execute arbitrary Nix code.

**Fix:** Regex validation + reject special characters before interpolation.

### P2 — No Leader Election

Running multiple operator replicas causes duplicate evaluations and SSA conflicts.

**Fix:** Lease-based leader election via `kube-leader-election`, or single replica for MVP.

## High

### P3 — Revision Field Not Validated

The `revision` field accepts any string. Should be constrained to 6–40 hex characters.

### P4 — No Eval Concurrency Limit

No limit on concurrent eval subprocesses — potential fork-bomb under load.

**Fix:** Semaphore with configurable `max_concurrent_evals`.

### P5 — NAR Buffered in Memory

Large NARs are held in memory 3x during processing (1.5 GB for a 500 MB package like gcc).

**Fix phase 1:** 2 GB decompressed size limit. **Phase 2:** Streaming `NarVisitor`.

### P6 — Readiness Race Condition

Ready flag set before the controller starts watching, causing a brief window where the probe reports ready prematurely.

**Fix:** Set ready flag inside `for_each()` callback.

## Medium

### P7 — Silent Serialization Failures

`unwrap_or_default()` on serialization errors produces null instead of an error.

### P8 — Glob Matching ReDoS

Current glob implementation has potential quadratic complexity.

**Fix:** Use `glob-match` crate with O(n) worst-case.

### P9 — Lock Map Grows Unbounded

Download lock map entries are never cleaned up after completion.

### P10 — Runner Image Hardcoded

The runner image tag is hardcoded to `:latest`.

**Fix:** Make configurable + per-workload override.

### P11 — ownerReference with Empty UID

If the workload UID is `None`, child resources get an empty ownerReference, breaking garbage collection.

## Low

### P12 — Unused Workspace Dependencies

`libp2p`, `rkyv`, `smallvec`, and others are declared but not used.

### P13 — niphas-mesh is a Stub

The mesh crate has no implementation yet.

### P14 — AppError Missing Traits

`AppError` doesn't implement `Display` or `Error`.

### P15 — Manual humantime_serde

Duration serialization is hand-rolled instead of using `humantime_serde`.

### P16 — No Integration Tests

Zero integration tests — only unit tests for parsing and crypto.
