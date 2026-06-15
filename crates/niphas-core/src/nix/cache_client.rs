use crate::config::CacheConfig;
use crate::error::NiphasError;
use crate::nix::narinfo::NarInfo;
use crate::nix::store_path::StorePath;
use reqwest::Client;
use std::future::Future;
use std::time::Duration;
use tracing::{debug, warn};

/// Trait abstracting binary cache operations.
///
/// Enables testing closure resolution, CSI cache, and evaluator
/// without hitting real HTTP endpoints.
pub trait BinaryCacheClient: Send + Sync {
    /// Fetch `.narinfo` for a store path, trying caches in priority order.
    fn fetch_narinfo(
        &self,
        store_path: &StorePath,
    ) -> impl Future<Output = Result<NarInfo, NiphasError>> + Send;

    /// Fetch the compressed NAR bytes for a narinfo.
    fn fetch_nar(
        &self,
        narinfo: &NarInfo,
    ) -> impl Future<Output = Result<bytes::Bytes, NiphasError>> + Send;

    /// Fetch narinfo by hash string directly (32-char Nix-base32).
    fn fetch_narinfo_by_hash(
        &self,
        hash: &str,
    ) -> impl Future<Output = Result<NarInfo, NiphasError>> + Send;
}

/// HTTP client for interacting with Nix binary caches.
///
/// Supports multiple caches with priority-based fallback.
#[derive(Clone)]
pub struct CacheClient {
    http: Client,
    caches: Vec<CacheConfig>,
}

impl CacheClient {
    /// Create a new cache client with the given caches (ordered by priority).
    pub fn new(caches: Vec<CacheConfig>) -> Result<Self, NiphasError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(8)
            .build()
            .map_err(|e| NiphasError::Cache(format!("failed to create HTTP client: {e}")))?;

        // Sort by priority (lower = preferred)
        let mut caches = caches;
        caches.sort_by_key(|c| c.priority);

        Ok(CacheClient { http, caches })
    }

    /// Fetch `.narinfo` for a store path, trying caches in priority order.
    async fn fetch_narinfo_impl(&self, store_path: &StorePath) -> Result<NarInfo, NiphasError> {
        let hash = store_path.hash_str();
        let narinfo_path = format!("{hash}.narinfo");

        let mut last_err = None;

        for cache in &self.caches {
            let url = format!("{}/{}", cache.url.trim_end_matches('/'), narinfo_path);
            debug!(cache_url = %cache.url, store_path = %store_path, "fetching narinfo");

            match self.http.get(&url).send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let text = resp.text().await.map_err(|e| {
                            NiphasError::Cache(format!("failed to read narinfo body: {e}"))
                        })?;
                        return NarInfo::parse(&text);
                    }

                    if resp.status().as_u16() == 404 {
                        debug!(cache_url = %cache.url, "narinfo not found, trying next cache");
                        last_err = Some(NiphasError::StorePathNotCached(store_path.to_string()));
                        continue;
                    }

                    let status = resp.status();
                    warn!(cache_url = %cache.url, %status, "unexpected HTTP status for narinfo");
                    last_err = Some(NiphasError::Cache(format!("HTTP {status} from {url}")));
                }
                Err(e) => {
                    warn!(cache_url = %cache.url, error = %e, "failed to fetch narinfo");
                    last_err = Some(NiphasError::Cache(format!("request failed: {e}")));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| NiphasError::Cache("no binary caches configured".into())))
    }

    /// Fetch the compressed NAR bytes for a narinfo.
    ///
    /// Returns the raw compressed bytes. Caller is responsible for
    /// decompression and hash verification.
    async fn fetch_nar_impl(&self, narinfo: &NarInfo) -> Result<bytes::Bytes, NiphasError> {
        // The URL in narinfo is relative to the cache base URL.
        // Try each cache in case one has the NAR.
        let mut last_err = None;

        for cache in &self.caches {
            let url = format!("{}/{}", cache.url.trim_end_matches('/'), narinfo.url);
            debug!(url = %url, "fetching NAR");

            match self.http.get(&url).send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let data = resp.bytes().await.map_err(|e| {
                            NiphasError::Cache(format!("failed to read NAR body: {e}"))
                        })?;
                        return Ok(data);
                    }

                    let status = resp.status();
                    warn!(url = %url, %status, "failed to fetch NAR");
                    last_err = Some(NiphasError::Cache(format!(
                        "HTTP {status} fetching NAR from {url}"
                    )));
                }
                Err(e) => {
                    warn!(url = %url, error = %e, "failed to fetch NAR");
                    last_err = Some(NiphasError::Cache(format!("NAR request failed: {e}")));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| NiphasError::Cache("no binary caches configured".into())))
    }

    /// Fetch narinfo by hash string directly (32-char Nix-base32).
    async fn fetch_narinfo_by_hash_impl(&self, hash: &str) -> Result<NarInfo, NiphasError> {
        let narinfo_path = format!("{hash}.narinfo");
        let mut last_err = None;

        for cache in &self.caches {
            let url = format!("{}/{}", cache.url.trim_end_matches('/'), narinfo_path);

            match self.http.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let text = resp.text().await.map_err(|e| {
                        NiphasError::Cache(format!("failed to read narinfo body: {e}"))
                    })?;
                    return NarInfo::parse(&text);
                }
                Ok(resp) if resp.status().as_u16() == 404 => {
                    last_err = Some(NiphasError::StorePathNotCached(hash.to_string()));
                    continue;
                }
                Ok(resp) => {
                    last_err = Some(NiphasError::Cache(format!(
                        "HTTP {} from {}",
                        resp.status(),
                        url
                    )));
                }
                Err(e) => {
                    last_err = Some(NiphasError::Cache(format!("request failed: {e}")));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| NiphasError::Cache("no binary caches configured".into())))
    }
}

impl BinaryCacheClient for CacheClient {
    async fn fetch_narinfo(&self, store_path: &StorePath) -> Result<NarInfo, NiphasError> {
        self.fetch_narinfo_impl(store_path).await
    }

    async fn fetch_nar(&self, narinfo: &NarInfo) -> Result<bytes::Bytes, NiphasError> {
        self.fetch_nar_impl(narinfo).await
    }

    async fn fetch_narinfo_by_hash(&self, hash: &str) -> Result<NarInfo, NiphasError> {
        self.fetch_narinfo_by_hash_impl(hash).await
    }
}
