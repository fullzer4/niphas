//! Test utilities for niphas-core consumers.
//!
//! Gated behind `#[cfg(any(test, feature = "testutils"))]` so downstream
//! crates can use `niphas-core = { ..., features = ["testutils"] }` in
//! `[dev-dependencies]` without pulling test code into production builds.

pub mod fake_cache;
pub mod narinfo_builder;
