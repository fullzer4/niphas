use crate::error::AppError;
use crate::evaluator::Evaluator;
use niphas_core::eval::{EvalRequest, EvalResponse};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;
use tracing::debug;

/// POST /evaluate -- evaluate a flake and resolve its closure.
pub async fn evaluate(
    State(evaluator): State<Arc<Evaluator>>,
    Json(req): Json<EvalRequest>,
) -> Result<Json<EvalResponse>, AppError> {
    debug!(
        flake_ref = %req.flake_ref,
        attribute = %req.attribute,
        "received eval request"
    );

    let result = evaluator.evaluate(&req).await?;
    Ok(Json(result))
}

/// GET /healthz -- liveness probe.
pub async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// GET /readyz -- readiness probe.
/// Returns 200 if at least one eval has succeeded (warm cache).
pub async fn readyz(State(evaluator): State<Arc<Evaluator>>) -> StatusCode {
    if evaluator.is_warm() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_healthz_200() {
        let status = healthz().await;
        assert_eq!(status, StatusCode::OK);
    }
}
