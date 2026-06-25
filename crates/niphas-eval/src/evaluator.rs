use crate::allowlist;
use crate::error::AppError;
use niphas_core::config::NiphasConfig;
use niphas_core::eval::{EvalRequest, EvalResponse};
use niphas_core::nix::cache_client::CacheClient;
use niphas_core::nix::closure;
use niphas_core::nix::signature::TrustedKey;
use niphas_core::nix::store_path::StorePath;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info};

/// The core evaluator, shared across requests via Arc.
pub struct Evaluator {
    config: NiphasConfig,
    cache_client: CacheClient,
    trusted_keys: Vec<TrustedKey>,
    /// Set to true after at least one successful evaluation.
    warm: AtomicBool,
}

impl Evaluator {
    pub fn new(config: NiphasConfig) -> Result<Self, niphas_core::error::NiphasError> {
        let cache_client = CacheClient::new(config.binary_caches.clone())?;

        let trusted_keys: Vec<TrustedKey> = config
            .binary_caches
            .iter()
            .filter_map(|c| c.public_key.as_deref())
            .map(TrustedKey::parse)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Evaluator {
            config,
            cache_client,
            trusted_keys,
            warm: AtomicBool::new(false),
        })
    }

    pub fn config(&self) -> &NiphasConfig {
        &self.config
    }

    pub fn is_warm(&self) -> bool {
        self.warm.load(Ordering::Relaxed)
    }

    /// Evaluate a flake and resolve its closure.
    pub async fn evaluate(&self, req: &EvalRequest) -> Result<EvalResponse, AppError> {
        // 1. Validate input characters (injection prevention)
        niphas_core::eval::validate_eval_request(req)?;

        // 2. Validate flake ref against allowlist
        allowlist::validate_flake_ref(&req.flake_ref, &self.config.allowed_flake_origins)?;

        // 3. Construct pinned flake reference
        let pinned_ref = match &req.revision {
            Some(rev) => format!("{}/{}", req.flake_ref, rev),
            None => req.flake_ref.clone(),
        };

        // 4. Nix evaluation via subprocess
        let nix_output = self.nix_eval(&pinned_ref, &req.attribute).await?;

        // 5. Resolve closure via binary cache
        let root = StorePath::parse(&nix_output.store_path)
            .map_err(|e| AppError::Internal(format!("invalid store path from eval: {e}")))?;

        let resolved = closure::resolve_closure(
            &self.cache_client,
            &root,
            self.config.closure_resolution.concurrency,
            self.config.closure_resolution.timeout,
            &self.trusted_keys,
            self.config.closure_resolution.max_paths,
            self.config.closure_resolution.max_nar_bytes,
        )
        .await
        .map_err(|e| AppError::ClosureResolutionFailed(e.to_string()))?;

        self.warm.store(true, Ordering::Relaxed);

        info!(
            store_path = %nix_output.store_path,
            closure_size = resolved.paths.len(),
            "evaluation complete"
        );

        Ok(EvalResponse {
            store_path: nix_output.store_path,
            name: nix_output.name,
            main_program: nix_output.main_program,
            closure_paths: resolved.paths,
        })
    }

    /// Evaluate a flake using `nix eval` subprocess.
    ///
    /// This is the Phase 3A fallback. Phase 3B will replace this with
    /// in-process Nix C FFI via nix-bindings-rust.
    async fn nix_eval(&self, pinned_ref: &str, attribute: &str) -> Result<NixEvalOutput, AppError> {
        let expr = format!(
            r#"let drv = (builtins.getFlake "{pinned_ref}").{attribute}; in builtins.toJSON {{
                storePath = drv.outPath;
                name = drv.name;
                mainProgram = drv.meta.mainProgram or null;
            }}"#
        );

        debug!(expr = %expr, "running nix eval");

        let timeout = self.config.eval_timeout;
        let child = tokio::process::Command::new("nix")
            .args(["eval", "--raw", "--impure", "--expr", &expr])
            .arg("--extra-experimental-features")
            .arg("nix-command flakes")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| AppError::EvalFailed(format!("failed to spawn nix eval: {e}")))?;

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(result) => {
                result.map_err(|e| AppError::EvalFailed(format!("failed to run nix eval: {e}")))?
            }
            Err(_) => {
                // child is moved into wait_with_output, but on timeout the future
                // is dropped — kill_on_drop(true) ensures the process is killed.
                return Err(AppError::EvalTimeout);
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::EvalFailed(format!("nix eval failed: {stderr}")));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(&stdout)
            .map_err(|e| AppError::EvalFailed(format!("failed to parse nix eval output: {e}")))
    }
}

/// Intermediate eval result from `nix eval` (before closure resolution).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NixEvalOutput {
    store_path: String,
    name: String,
    main_program: Option<String>,
}
