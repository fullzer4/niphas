//! In-memory fake binary cache client for testing.

use crate::error::NiphasError;
use crate::nix::cache_client::BinaryCacheClient;
use crate::nix::narinfo::NarInfo;
use crate::nix::store_path::StorePath;
use std::collections::HashMap;

/// A fake binary cache client that serves pre-loaded narinfos and NARs
/// from memory. Useful for testing closure resolution, CSI cache logic,
/// and eval workflows without network access.
#[derive(Clone, Default)]
pub struct FakeCacheClient {
    /// Narinfos keyed by store path hash (32-char Nix-base32).
    narinfos: HashMap<String, NarInfo>,
    /// NAR data keyed by narinfo URL.
    nars: HashMap<String, bytes::Bytes>,
}

impl FakeCacheClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a narinfo. It will be returned for both `fetch_narinfo`
    /// (matched by store path hash) and `fetch_narinfo_by_hash`.
    pub fn add_narinfo(&mut self, narinfo: NarInfo) {
        let hash = narinfo.store_path.hash_str();
        self.narinfos.insert(hash, narinfo);
    }

    /// Register NAR data for a given narinfo URL.
    pub fn add_nar(&mut self, url: &str, data: bytes::Bytes) {
        self.nars.insert(url.to_owned(), data);
    }
}

impl BinaryCacheClient for FakeCacheClient {
    async fn fetch_narinfo(&self, store_path: &StorePath) -> Result<NarInfo, NiphasError> {
        let hash = store_path.hash_str();
        self.narinfos
            .get(&hash)
            .cloned()
            .ok_or_else(|| NiphasError::StorePathNotCached(store_path.to_string()))
    }

    async fn fetch_nar(&self, narinfo: &NarInfo) -> Result<bytes::Bytes, NiphasError> {
        self.nars
            .get(&narinfo.url)
            .cloned()
            .ok_or_else(|| NiphasError::Cache(format!("NAR not found: {}", narinfo.url)))
    }

    async fn fetch_narinfo_by_hash(&self, hash: &str) -> Result<NarInfo, NiphasError> {
        self.narinfos
            .get(hash)
            .cloned()
            .ok_or_else(|| NiphasError::StorePathNotCached(hash.to_string()))
    }
}
