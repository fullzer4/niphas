use crate::error::OperatorError;
use niphas_core::crd::NiphasWorkload;
use niphas_core::eval::{EvalRequest, EvalResponse};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};

/// Re-export as EvalResult for backward compatibility within the operator.
pub type EvalResult = EvalResponse;

/// Error response from the eval webhook.
#[derive(Debug, Deserialize)]
pub struct EvalErrorResponse {
    pub error: String,
    pub message: String,
}

/// Extension methods for EvalResult within the operator context.
pub trait EvalResultExt {
    fn from_status(status: &niphas_core::crd::NiphasWorkloadStatus) -> Option<EvalResult>;
}

impl EvalResultExt for EvalResult {
    /// Reconstruct an EvalResult from the workload status (for skipping re-eval).
    fn from_status(status: &niphas_core::crd::NiphasWorkloadStatus) -> Option<EvalResult> {
        use niphas_core::nix::store_path::StorePath;

        let store_path = status.store_path.as_ref()?;
        let closure_paths = status.closure_paths.clone().unwrap_or_default();
        let resolved_command = status.resolved_command.clone();

        // Extract name from store path using the validated parser.
        let name = StorePath::parse(store_path)
            .map(|sp| sp.name.clone())
            .unwrap_or_else(|_| "unknown".to_string());

        // Extract mainProgram from resolved command.
        let main_program = resolved_command
            .as_ref()
            .and_then(|cmd| cmd.rsplit('/').next().map(|s| s.to_string()));

        Some(EvalResult {
            store_path: store_path.clone(),
            name,
            main_program,
            closure_paths,
        })
    }
}

/// Call the eval webhook to evaluate a flake.
pub async fn call_eval_webhook(
    http: &Client,
    eval_url: &str,
    workload: &NiphasWorkload,
    timeout: Duration,
) -> Result<EvalResult, OperatorError> {
    let req = EvalRequest {
        flake_ref: workload.spec.flake_ref.clone(),
        attribute: workload.spec.attribute.clone(),
        revision: workload.spec.revision.clone(),
        binary_cache: workload.spec.binary_cache.clone(),
    };

    debug!(
        flake_ref = %req.flake_ref,
        attribute = %req.attribute,
        "calling eval webhook"
    );

    let url = format!("{}/evaluate", eval_url.trim_end_matches('/'));

    let resp = http
        .post(&url)
        .json(&req)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                OperatorError::EvalTimeout(timeout.as_secs())
            } else {
                OperatorError::EvalWebhook(format!("request failed: {e}"))
            }
        })?;

    if resp.status().is_success() {
        let result: EvalResult = resp
            .json()
            .await
            .map_err(|e| OperatorError::EvalWebhook(format!("invalid response body: {e}")))?;
        debug!(store_path = %result.store_path, "eval succeeded");
        return Ok(result);
    }

    // Try to parse error response
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if let Ok(err_resp) = serde_json::from_str::<EvalErrorResponse>(&body) {
        warn!(
            error_code = %err_resp.error,
            message = %err_resp.message,
            "eval webhook returned error"
        );
        Err(OperatorError::EvalFailed {
            code: err_resp.error,
            message: err_resp.message,
        })
    } else {
        Err(OperatorError::EvalWebhook(format!("HTTP {status}: {body}")))
    }
}
