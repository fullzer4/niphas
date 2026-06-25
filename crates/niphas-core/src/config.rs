use figment::{
    Figment,
    providers::{Env, Format, Serialized, Yaml},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Top-level configuration, shared across niphas components.
/// Each binary loads the subset it needs.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NiphasConfig {
    /// Log level (RUST_LOG format). Defaults to "info".
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Binary caches, ordered by priority (first = highest).
    #[serde(default)]
    pub binary_caches: Vec<CacheConfig>,

    /// Flake origins allowed for evaluation (glob patterns).
    #[serde(default)]
    pub allowed_flake_origins: Vec<String>,

    /// URIs the Nix evaluator is allowed to fetch (restrict-eval allowlist).
    #[serde(default)]
    pub allowed_uris: Vec<String>,

    /// Evaluation timeout.
    #[serde(default = "default_eval_timeout", with = "humantime_serde")]
    pub eval_timeout: Duration,

    /// Closure resolution settings.
    #[serde(default)]
    pub closure_resolution: ClosureResolutionConfig,

    /// Node-local NAR cache settings (used by CSI driver).
    #[serde(default)]
    pub cache: CacheStorageConfig,

    /// Eval webhook URL (used by operator).
    #[serde(default = "default_eval_url")]
    pub eval_webhook_url: String,

    /// Operator health/metrics server port.
    #[serde(default = "default_health_port")]
    pub health_port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheConfig {
    /// Binary cache URL.
    pub url: String,

    /// Ed25519 public key for signature verification.
    /// Format: `<name>:<base64-key>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,

    /// Priority (lower = preferred). Defaults to 40.
    #[serde(default = "default_cache_priority")]
    pub priority: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosureResolutionConfig {
    /// Max concurrent .narinfo fetches.
    #[serde(default = "default_closure_concurrency")]
    pub concurrency: usize,

    /// Timeout for resolving the full closure.
    #[serde(default = "default_closure_timeout", with = "humantime_serde")]
    pub timeout: Duration,

    /// Maximum number of store paths in a closure.
    #[serde(default = "default_max_paths")]
    pub max_paths: usize,

    /// Maximum total NAR bytes (uncompressed) for a closure.
    #[serde(default = "default_max_nar_bytes")]
    pub max_nar_bytes: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStorageConfig {
    /// Path to the node-local NAR cache.
    #[serde(default = "default_cache_path")]
    pub path: PathBuf,

    /// High watermark (bytes). GC starts when usage exceeds this.
    #[serde(default = "default_high_watermark")]
    pub high_watermark_bytes: u64,

    /// Low watermark (bytes). GC stops when usage drops below this.
    #[serde(default = "default_low_watermark")]
    pub low_watermark_bytes: u64,

    /// GC scan interval.
    #[serde(default = "default_gc_interval", with = "humantime_serde")]
    pub gc_interval: Duration,
}

fn default_log_level() -> String {
    "info".into()
}

fn default_eval_timeout() -> Duration {
    Duration::from_secs(300)
}

fn default_eval_url() -> String {
    "http://niphas-eval.niphas-system.svc:8443".into()
}

fn default_health_port() -> u16 {
    8080
}

fn default_cache_priority() -> u32 {
    40
}

fn default_closure_concurrency() -> usize {
    32
}

fn default_closure_timeout() -> Duration {
    Duration::from_secs(60)
}

fn default_max_paths() -> usize {
    50_000
}

fn default_max_nar_bytes() -> u64 {
    // 50 GB
    50 * 1024 * 1024 * 1024
}

fn default_cache_path() -> PathBuf {
    PathBuf::from("/var/lib/niphas/cache")
}

fn default_high_watermark() -> u64 {
    // 15 GB
    15 * 1024 * 1024 * 1024
}

fn default_low_watermark() -> u64 {
    // 10 GB
    10 * 1024 * 1024 * 1024
}

fn default_gc_interval() -> Duration {
    Duration::from_secs(300)
}

impl Default for NiphasConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
            binary_caches: vec![],
            allowed_flake_origins: vec![],
            allowed_uris: vec![],
            eval_timeout: default_eval_timeout(),
            closure_resolution: ClosureResolutionConfig::default(),
            cache: CacheStorageConfig::default(),
            eval_webhook_url: default_eval_url(),
            health_port: default_health_port(),
        }
    }
}

impl Default for ClosureResolutionConfig {
    fn default() -> Self {
        Self {
            concurrency: default_closure_concurrency(),
            timeout: default_closure_timeout(),
            max_paths: default_max_paths(),
            max_nar_bytes: default_max_nar_bytes(),
        }
    }
}

impl Default for CacheStorageConfig {
    fn default() -> Self {
        Self {
            path: default_cache_path(),
            high_watermark_bytes: default_high_watermark(),
            low_watermark_bytes: default_low_watermark(),
            gc_interval: default_gc_interval(),
        }
    }
}

impl NiphasConfig {
    /// Load configuration from file + environment variables.
    ///
    /// Priority (highest wins):
    /// 1. Environment variables prefixed with `NIPHAS_`
    /// 2. YAML config file at `path` (if provided)
    /// 3. Built-in defaults
    #[allow(clippy::result_large_err)]
    pub fn load(path: Option<&str>) -> Result<Self, figment::Error> {
        let mut figment = Figment::from(Serialized::defaults(NiphasConfig::default()));

        if let Some(config_path) = path {
            figment = figment.merge(Yaml::file(config_path));
        }

        // Also check the standard location
        figment = figment.merge(Yaml::file("/etc/niphas/config.yaml"));

        figment = figment.merge(Env::prefixed("NIPHAS_").split("_"));

        figment.extract()
    }

    /// Validate configuration invariants. Returns a list of problems found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.cache.high_watermark_bytes <= self.cache.low_watermark_bytes {
            errors.push(format!(
                "cache.highWatermarkBytes ({}) must be greater than cache.lowWatermarkBytes ({})",
                self.cache.high_watermark_bytes, self.cache.low_watermark_bytes
            ));
        }

        if self.eval_timeout.is_zero() {
            errors.push("evalTimeout must be greater than 0".into());
        }

        if self.closure_resolution.concurrency == 0 {
            errors.push("closureResolution.concurrency must be greater than 0".into());
        }

        if self.closure_resolution.timeout.is_zero() {
            errors.push("closureResolution.timeout must be greater than 0".into());
        }

        if self.binary_caches.is_empty() {
            errors.push(
                "binaryCaches must not be empty (at least one binary cache is required)".into(),
            );
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Load configuration, falling back to defaults with a warning on error.
    pub fn load_or_default(path: Option<&str>) -> Self {
        match Self::load(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load config, using defaults");
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let config = NiphasConfig::default();
        assert_eq!(config.log_level, "info");
        assert!(config.binary_caches.is_empty());
        assert!(config.allowed_flake_origins.is_empty());
        assert_eq!(config.eval_timeout, Duration::from_secs(300));
        assert_eq!(config.closure_resolution.concurrency, 32);
        assert_eq!(config.closure_resolution.timeout, Duration::from_secs(60));
        assert_eq!(config.cache.path, PathBuf::from("/var/lib/niphas/cache"));
        assert_eq!(config.health_port, 8080);
        assert_eq!(
            config.eval_webhook_url,
            "http://niphas-eval.niphas-system.svc:8443"
        );
    }

    #[test]
    fn test_default_cache_storage() {
        let cs = CacheStorageConfig::default();
        assert_eq!(cs.high_watermark_bytes, 15 * 1024 * 1024 * 1024);
        assert_eq!(cs.low_watermark_bytes, 10 * 1024 * 1024 * 1024);
        assert_eq!(cs.gc_interval, Duration::from_secs(300));
    }

    fn valid_config() -> NiphasConfig {
        NiphasConfig {
            binary_caches: vec![CacheConfig {
                url: "https://cache.nixos.org".into(),
                public_key: None,
                priority: 40,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn test_validate_high_watermark_less_than_low() {
        let config = NiphasConfig {
            cache: CacheStorageConfig {
                high_watermark_bytes: 100,
                low_watermark_bytes: 200,
                ..Default::default()
            },
            ..valid_config()
        };
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("highWatermarkBytes")));
    }

    #[test]
    fn test_validate_zero_eval_timeout() {
        let config = NiphasConfig {
            eval_timeout: Duration::from_secs(0),
            ..valid_config()
        };
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("evalTimeout")));
    }

    #[test]
    fn test_validate_empty_binary_caches() {
        let config = NiphasConfig::default();
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("binaryCaches")));
    }

    #[test]
    fn test_validate_valid_config() {
        assert!(valid_config().validate().is_ok());
    }

    #[test]
    fn test_humantime_parse_seconds() {
        assert_eq!(
            humantime_serde::parse_duration("300s").unwrap(),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn test_humantime_parse_minutes() {
        assert_eq!(
            humantime_serde::parse_duration("5m").unwrap(),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn test_humantime_parse_hours() {
        assert_eq!(
            humantime_serde::parse_duration("1h").unwrap(),
            Duration::from_secs(3600)
        );
    }

    #[test]
    fn test_humantime_parse_raw_number() {
        assert_eq!(
            humantime_serde::parse_duration("60").unwrap(),
            Duration::from_secs(60)
        );
    }
}

/// Serde support for `Duration` as human-readable strings (e.g. "300s", "5m").
mod humantime_serde {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = format!("{}s", duration.as_secs());
        serializer.serialize_str(&s)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_duration(&s).map_err(serde::de::Error::custom)
    }

    pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
        let s = s.trim();
        if let Some(secs) = s.strip_suffix('s') {
            secs.trim()
                .parse::<u64>()
                .map(Duration::from_secs)
                .map_err(|e| format!("invalid duration '{}': {}", s, e))
        } else if let Some(mins) = s.strip_suffix('m') {
            mins.trim()
                .parse::<u64>()
                .map(|m| Duration::from_secs(m * 60))
                .map_err(|e| format!("invalid duration '{}': {}", s, e))
        } else if let Some(hours) = s.strip_suffix('h') {
            hours
                .trim()
                .parse::<u64>()
                .map(|h| Duration::from_secs(h * 3600))
                .map_err(|e| format!("invalid duration '{}': {}", s, e))
        } else {
            // Try parsing as raw seconds
            s.parse::<u64>().map(Duration::from_secs).map_err(|_| {
                format!(
                    "invalid duration '{}': expected format like '300s', '5m', '1h'",
                    s
                )
            })
        }
    }
}
