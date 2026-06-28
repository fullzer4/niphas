use crate::error::NiphasError;
use crate::nix::cache_client::BinaryCacheClient;
use crate::nix::narinfo::NarInfo;
use crate::nix::signature::{self, TrustedKey};
use crate::nix::store_path::StorePath;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::debug;

/// Result of resolving a full closure.
#[derive(Debug)]
pub struct ResolvedClosure {
    /// All narinfos in the closure, keyed by store path string.
    pub narinfos: HashMap<String, NarInfo>,
    /// All store paths in the closure (topologically sorted, root first).
    pub paths: Vec<String>,
}

impl ResolvedClosure {
    /// All store path strings in the closure.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Total NAR size (uncompressed) for the entire closure.
    pub fn nar_size(&self) -> u64 {
        self.narinfos.values().map(|ni| ni.nar_size).sum()
    }

    /// Total file size (compressed) for the entire closure.
    pub fn file_size(&self) -> u64 {
        self.narinfos.values().map(|ni| ni.file_size).sum()
    }
}

/// Resolve the full transitive closure of a store path.
///
/// Performs BFS over `.narinfo` References using parallel HTTP fetches
/// (bounded by `concurrency`). Returns all narinfos needed to materialize
/// the store path.
///
/// If `trusted_keys` is non-empty, each narinfo's signature is verified
/// against the trusted keys. If empty, no signature verification is performed
/// (backwards compatible).
pub async fn resolve_closure<C: BinaryCacheClient + Clone>(
    client: &C,
    root: &StorePath,
    concurrency: usize,
    timeout: Duration,
    trusted_keys: &[TrustedKey],
    max_paths: usize,
    max_nar_bytes: u64,
) -> Result<ResolvedClosure, NiphasError> {
    let deadline = tokio::time::Instant::now() + timeout;

    let mut visited: HashSet<String> = HashSet::new();
    let mut narinfos: HashMap<String, NarInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut total_nar_bytes: u64 = 0;

    // BFS queue: store path strings to resolve
    let mut queue: Vec<StorePath> = vec![root.clone()];

    while !queue.is_empty() {
        if tokio::time::Instant::now() > deadline {
            return Err(NiphasError::Timeout(format!(
                "closure resolution timed out after {}s",
                timeout.as_secs()
            )));
        }

        // Take up to `concurrency` items from queue
        let batch: Vec<StorePath> = queue.drain(..queue.len().min(concurrency)).collect();

        let mut futures = FuturesUnordered::new();

        for sp in &batch {
            let sp_str = sp.to_string();
            if visited.contains(&sp_str) {
                continue;
            }
            visited.insert(sp_str.clone());

            let client = client.clone();
            let sp = sp.clone();
            futures.push(async move {
                let result = client.fetch_narinfo(&sp).await;
                (sp, result)
            });
        }

        while let Some((sp, result)) = futures.next().await {
            let sp_str = sp.to_string();
            match result {
                Ok(narinfo) => {
                    debug!(store_path = %sp_str, refs = narinfo.references.len(), "resolved narinfo");

                    // Verify signature if trusted keys are configured
                    if !trusted_keys.is_empty() {
                        let fp = narinfo.fingerprint();
                        signature::verify_narinfo(&fp, &narinfo.signatures, trusted_keys)?;
                    }

                    // Enqueue unvisited references
                    for ref_basename in &narinfo.references {
                        let ref_path_str = ref_basename.to_store_path_string();
                        if !visited.contains(&ref_path_str) {
                            match StorePath::parse(&ref_path_str) {
                                Ok(ref_sp) => queue.push(ref_sp),
                                Err(e) => {
                                    return Err(NiphasError::ClosureResolution(format!(
                                        "invalid reference '{}' in {}: {}",
                                        ref_path_str, sp_str, e
                                    )));
                                }
                            }
                        }
                    }

                    order.push(sp_str.clone());
                    total_nar_bytes += narinfo.nar_size;
                    narinfos.insert(sp_str, narinfo);

                    // Check closure size limits
                    if narinfos.len() > max_paths {
                        return Err(NiphasError::ClosureResolution(format!(
                            "closure exceeds max path count ({})",
                            max_paths
                        )));
                    }
                    if total_nar_bytes > max_nar_bytes {
                        return Err(NiphasError::ClosureResolution(format!(
                            "closure exceeds max NAR size ({} bytes)",
                            max_nar_bytes
                        )));
                    }
                }
                Err(e) => {
                    return Err(NiphasError::ClosureResolution(format!(
                        "failed to resolve {sp_str}: {e}"
                    )));
                }
            }
        }
    }

    debug!(
        root = %root,
        total_paths = order.len(),
        "closure resolution complete"
    );

    Ok(ResolvedClosure {
        narinfos,
        paths: order,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nix::signature::{NarSignature, TrustedKey};
    use crate::testutils::fake_cache::FakeCacheClient;
    use crate::testutils::narinfo_builder::NarInfoBuilder;

    // Real Nix store path hashes (32-char Nix-base32) for test fixtures.
    const HASH_A: &str = "00bgd045z0d4icpbc2yyz4gx48ak44la";
    const HASH_B: &str = "3n58xw4373jp0ljirf06d8077j15pc4j";
    const HASH_C: &str = "aw2fw9ag10wr9pf0qk4nk5sxi0q0bn56";
    const HASH_D: &str = "b6vsz4mzc9ql0a0rdiw5mmfqajj39lqg";

    fn sp(hash: &str, name: &str) -> String {
        format!("/nix/store/{hash}-{name}")
    }

    fn basename(hash: &str, name: &str) -> String {
        format!("{hash}-{name}")
    }

    #[tokio::test]
    async fn test_resolve_closure_single_path_no_refs() {
        let mut client = FakeCacheClient::new();
        let path = sp(HASH_A, "hello-2.12.1");
        client.add_narinfo(NarInfoBuilder::new(&path).nar_size(5000).build());

        let root = StorePath::parse(&path).unwrap();
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[],
            50_000,
            50 * 1024 * 1024 * 1024,
        )
        .await
        .unwrap();

        assert_eq!(result.paths.len(), 1);
        assert_eq!(result.paths[0], path);
        assert_eq!(result.nar_size(), 5000);
    }

    #[tokio::test]
    async fn test_resolve_closure_transitive() {
        // A -> B -> C
        let mut client = FakeCacheClient::new();

        let path_a = sp(HASH_A, "hello-2.12.1");
        let path_b = sp(HASH_B, "glibc-2.37-8");
        let path_c = sp(HASH_C, "gcc-12.3.0-lib");

        client.add_narinfo(
            NarInfoBuilder::new(&path_a)
                .references(vec![&basename(HASH_B, "glibc-2.37-8")])
                .build(),
        );
        client.add_narinfo(
            NarInfoBuilder::new(&path_b)
                .references(vec![&basename(HASH_C, "gcc-12.3.0-lib")])
                .build(),
        );
        client.add_narinfo(NarInfoBuilder::new(&path_c).build());

        let root = StorePath::parse(&path_a).unwrap();
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[],
            50_000,
            50 * 1024 * 1024 * 1024,
        )
        .await
        .unwrap();

        assert_eq!(result.paths.len(), 3);
        assert!(result.paths.contains(&path_a));
        assert!(result.paths.contains(&path_b));
        assert!(result.paths.contains(&path_c));
    }

    #[tokio::test]
    async fn test_resolve_closure_diamond_dedup() {
        // A -> B, A -> C, B -> D, C -> D
        let mut client = FakeCacheClient::new();

        let path_a = sp(HASH_A, "hello-2.12.1");
        let path_b = sp(HASH_B, "glibc-2.37-8");
        let path_c = sp(HASH_C, "gcc-12.3.0-lib");
        let path_d = sp(HASH_D, "linux-headers-6.5");

        client.add_narinfo(
            NarInfoBuilder::new(&path_a)
                .references(vec![
                    &basename(HASH_B, "glibc-2.37-8"),
                    &basename(HASH_C, "gcc-12.3.0-lib"),
                ])
                .build(),
        );
        client.add_narinfo(
            NarInfoBuilder::new(&path_b)
                .references(vec![&basename(HASH_D, "linux-headers-6.5")])
                .build(),
        );
        client.add_narinfo(
            NarInfoBuilder::new(&path_c)
                .references(vec![&basename(HASH_D, "linux-headers-6.5")])
                .build(),
        );
        client.add_narinfo(NarInfoBuilder::new(&path_d).build());

        let root = StorePath::parse(&path_a).unwrap();
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[],
            50_000,
            50 * 1024 * 1024 * 1024,
        )
        .await
        .unwrap();

        // D should appear exactly once despite being referenced by both B and C.
        assert_eq!(result.paths.len(), 4);
        let d_count = result.paths.iter().filter(|p| **p == path_d).count();
        assert_eq!(d_count, 1, "D should be deduplicated");
    }

    #[tokio::test]
    async fn test_resolve_closure_missing_ref_errors() {
        let mut client = FakeCacheClient::new();

        let path_a = sp(HASH_A, "hello-2.12.1");
        // A references B, but B is not in the cache.
        client.add_narinfo(
            NarInfoBuilder::new(&path_a)
                .references(vec![&basename(HASH_B, "glibc-2.37-8")])
                .build(),
        );

        let root = StorePath::parse(&path_a).unwrap();
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[],
            50_000,
            50 * 1024 * 1024 * 1024,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to resolve"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn test_closure_helpers() {
        let mut client = FakeCacheClient::new();

        let path_a = sp(HASH_A, "hello-2.12.1");
        let path_b = sp(HASH_B, "glibc-2.37-8");

        client.add_narinfo(
            NarInfoBuilder::new(&path_a)
                .nar_size(1000)
                .file_size(500)
                .references(vec![&basename(HASH_B, "glibc-2.37-8")])
                .build(),
        );
        client.add_narinfo(
            NarInfoBuilder::new(&path_b)
                .nar_size(3000)
                .file_size(1500)
                .build(),
        );

        let root = StorePath::parse(&path_a).unwrap();
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[],
            50_000,
            50 * 1024 * 1024 * 1024,
        )
        .await
        .unwrap();

        assert_eq!(result.paths().len(), 2);
        assert_eq!(result.nar_size(), 4000);
        assert_eq!(result.file_size(), 2000);
    }

    #[tokio::test]
    async fn test_resolve_closure_self_reference() {
        // A references itself (common for glibc).
        let mut client = FakeCacheClient::new();

        let path_a = sp(HASH_A, "glibc-2.37-8");
        client.add_narinfo(
            NarInfoBuilder::new(&path_a)
                .references(vec![&basename(HASH_A, "glibc-2.37-8")])
                .build(),
        );

        let root = StorePath::parse(&path_a).unwrap();
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[],
            50_000,
            50 * 1024 * 1024 * 1024,
        )
        .await
        .unwrap();

        assert_eq!(result.paths.len(), 1);
    }

    /// Helper: generate a valid ed25519 keypair and sign a narinfo fingerprint.
    fn sign_narinfo(narinfo: &NarInfo, key_name: &str) -> (TrustedKey, NarSignature) {
        use base64::prelude::*;
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let verifying_key = signing_key.verifying_key();

        let fp = narinfo.fingerprint();
        let sig = signing_key.sign(fp.as_bytes());

        let sig_b64 = BASE64_STANDARD.encode(sig.to_bytes());
        let sig_str = format!("{key_name}:{sig_b64}");
        let nar_sig = NarSignature::parse(&sig_str).unwrap();

        let key_b64 = BASE64_STANDARD.encode(verifying_key.to_bytes());
        let key_str = format!("{key_name}:{key_b64}");
        let trusted_key = TrustedKey::parse(&key_str).unwrap();

        (trusted_key, nar_sig)
    }

    #[tokio::test]
    async fn test_signature_verification_valid() {
        let mut client = FakeCacheClient::new();
        let path = sp(HASH_A, "hello-2.12.1");

        let narinfo_unsigned = NarInfoBuilder::new(&path).nar_size(5000).build();
        let (trusted_key, nar_sig) = sign_narinfo(&narinfo_unsigned, "test-key-1");

        client.add_narinfo(
            NarInfoBuilder::new(&path)
                .nar_size(5000)
                .signatures(vec![nar_sig])
                .build(),
        );

        let root = StorePath::parse(&path).unwrap();
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[trusted_key],
            50_000,
            50 * 1024 * 1024 * 1024,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_signature_verification_no_valid_sig() {
        let mut client = FakeCacheClient::new();
        let path = sp(HASH_A, "hello-2.12.1");

        // Narinfo with no signatures, but trusted keys are configured
        client.add_narinfo(NarInfoBuilder::new(&path).nar_size(5000).build());

        let key_str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
        let trusted_key = TrustedKey::parse(key_str).unwrap();

        let root = StorePath::parse(&path).unwrap();
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[trusted_key],
            50_000,
            50 * 1024 * 1024 * 1024,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("signature"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn test_signature_verification_empty_keys_skips() {
        let mut client = FakeCacheClient::new();
        let path = sp(HASH_A, "hello-2.12.1");

        // Narinfo with no signatures, but no trusted keys configured → should pass
        client.add_narinfo(NarInfoBuilder::new(&path).nar_size(5000).build());

        let root = StorePath::parse(&path).unwrap();
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[],
            50_000,
            50 * 1024 * 1024 * 1024,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_closure_exceeds_max_paths() {
        let mut client = FakeCacheClient::new();
        let path = sp(HASH_A, "hello-2.12.1");
        client.add_narinfo(NarInfoBuilder::new(&path).nar_size(5000).build());

        let root = StorePath::parse(&path).unwrap();
        // max_paths = 0 means any closure with at least 1 path should fail
        let result = resolve_closure(
            &client,
            &root,
            4,
            Duration::from_secs(10),
            &[],
            0,
            50 * 1024 * 1024 * 1024,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max path count"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn test_closure_exceeds_max_nar_bytes() {
        let mut client = FakeCacheClient::new();
        let path = sp(HASH_A, "hello-2.12.1");
        client.add_narinfo(NarInfoBuilder::new(&path).nar_size(5000).build());

        let root = StorePath::parse(&path).unwrap();
        // max_nar_bytes = 100 is less than the 5000 nar_size
        let result =
            resolve_closure(&client, &root, 4, Duration::from_secs(10), &[], 50_000, 100).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max NAR size"), "unexpected error: {err}");
    }
}
