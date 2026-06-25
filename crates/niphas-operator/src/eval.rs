use crate::error::OperatorError;
use niphas_core::crd::{NiphasWorkload, NiphasWorkloadStatus};
use niphas_core::eval::{EvalRequest, EvalResponse};
use niphas_core::nix::store_path::StorePath;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};

#[derive(Debug, Deserialize)]
pub struct EvalErrorResponse {
    pub error: String,
    pub message: String,
}

pub fn eval_result_from_status(status: &NiphasWorkloadStatus) -> Option<EvalResponse> {
    let store_path = status.store_path.as_ref()?;
    let closure_paths = status.closure_paths.clone().unwrap_or_default();
    let resolved_command = status.resolved_command.clone();

    let name = StorePath::parse(store_path)
        .map(|sp| sp.name.clone())
        .unwrap_or_else(|_| "unknown".to_string());

    let main_program = resolved_command
        .as_ref()
        .and_then(|cmd| cmd.rsplit('/').next().map(|s| s.to_string()));

    Some(EvalResponse {
        store_path: store_path.clone(),
        name,
        main_program,
        closure_paths,
    })
}

pub async fn call_eval_webhook(
    http: &Client,
    eval_url: &str,
    workload: &NiphasWorkload,
    timeout: Duration,
) -> Result<EvalResponse, OperatorError> {
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
        let result: EvalResponse = resp
            .json()
            .await
            .map_err(|e| OperatorError::EvalWebhook(format!("invalid response body: {e}")))?;
        debug!(store_path = %result.store_path, "eval succeeded");
        return Ok(result);
    }

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
