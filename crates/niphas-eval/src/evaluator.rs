use crate::allowlist;
use crate::error::AppError;
use niphas_core::config::NiphasConfig;
use niphas_core::eval::{EvalRequest, EvalResponse};
use niphas_core::nix::cache_client::CacheClient;
use niphas_core::nix::closure;
use niphas_core::nix::store_path::StorePath;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info};

/// The core evaluator, shared across requests via Arc.
pub struct Evaluator {
    config: NiphasConfig,
    cache_client: CacheClient,
    /// Set to true after at least one successful evaluation.
    warm: AtomicBool,
}

impl Evaluator {
    pub fn new(config: NiphasConfig) -> Result<Self, niphas_core::error::NiphasError> {
        let cache_client = CacheClient::new(config.binary_caches.clone())?;

        Ok(Evaluator {
            config,
            cache_client,
            warm: AtomicBool::new(false),
        })
    }

    /// Whether at least one eval has succeeded (for readiness probe).
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
        //
        // TODO: Phase 3B -- integrate nix-bindings-rust for in-process eval.
        // For now, we use `nix eval` as a subprocess fallback.
        let eval_result = self.nix_eval(&pinned_ref, &req.attribute).await?;

        // 5. Resolve closure via binary cache
        let root = StorePath::parse(&eval_result.store_path)
            .map_err(|e| AppError::Internal(format!("invalid store path from eval: {e}")))?;

        let resolved = closure::resolve_closure(
            &self.cache_client,
            &root,
            self.config.closure_resolution.concurrency,
            self.config.closure_resolution.timeout,
        )
        .await
        .map_err(|e| AppError::ClosureResolutionFailed(e.to_string()))?;

        self.warm.store(true, Ordering::Relaxed);

        info!(
            store_path = %eval_result.store_path,
            closure_size = resolved.paths.len(),
            "evaluation complete"
        );

        Ok(EvalResponse {
            store_path: eval_result.store_path,
            name: eval_result.name,
            main_program: eval_result.main_program,
            closure_paths: resolved.paths,
        })
    }

    /// Evaluate a flake using `nix eval` subprocess.
    ///
    /// This is the Phase 3A fallback. Phase 3B will replace this with
    /// in-process Nix C FFI via nix-bindings-rust.
    async fn nix_eval(&self, pinned_ref: &str, attribute: &str) -> Result<NixEvalResult, AppError> {
        let expr = format!(
            r#"let drv = (builtins.getFlake "{pinned_ref}").{attribute}; in builtins.toJSON {{
                outPath = drv.outPath;
                name = drv.name;
                mainProgram = drv.meta.mainProgram or null;
            }}"#
        );

        debug!(expr = %expr, "running nix eval");

        let timeout = self.config.eval_timeout;
        let output = tokio::time::timeout(timeout, async {
            tokio::process::Command::new("nix")
                .args(["eval", "--raw", "--expr", &expr])
                .arg("--extra-experimental-features")
                .arg("nix-command flakes")
                .output()
                .await
        })
        .await
        .map_err(|_| AppError::EvalTimeout)?
        .map_err(|e| AppError::EvalFailed(format!("failed to run nix eval: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::EvalFailed(format!("nix eval failed: {stderr}")));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: NixEvalOutput = serde_json::from_str(&stdout)
            .map_err(|e| AppError::EvalFailed(format!("failed to parse nix eval output: {e}")))?;

        Ok(NixEvalResult {
            store_path: parsed.out_path,
            name: parsed.name,
            main_program: parsed.main_program,
        })
    }
}

/// Raw output from nix eval.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NixEvalOutput {
    out_path: String,
    name: String,
    main_program: Option<String>,
}

/// Intermediate eval result (before closure resolution).
struct NixEvalResult {
    store_path: String,
    name: String,
    main_program: Option<String>,
}
