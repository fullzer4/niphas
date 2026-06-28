use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use niphas_core::error::NiphasError;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("flake not allowed: {0}")]
    FlakeNotAllowed(String),

    #[error("evaluation failed: {0}")]
    EvalFailed(String),

    #[error("evaluation exceeded timeout")]
    EvalTimeout,

    #[error("store path not cached: {0}")]
    StorePathNotCached(String),

    #[error("closure resolution failed: {0}")]
    ClosureResolutionFailed(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    /// HTTP status code for this error.
    fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidInput(_) => StatusCode::BAD_REQUEST,
            Self::FlakeNotAllowed(_) => StatusCode::FORBIDDEN,
            Self::EvalFailed(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::EvalTimeout => StatusCode::REQUEST_TIMEOUT,
            Self::StorePathNotCached(_) => StatusCode::NOT_FOUND,
            Self::ClosureResolutionFailed(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Machine-readable error code for the JSON response.
    fn error_code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "InvalidInput",
            Self::FlakeNotAllowed(_) => "FlakeNotAllowed",
            Self::EvalFailed(_) => "EvalFailed",
            Self::EvalTimeout => "EvalTimeout",
            Self::StorePathNotCached(_) => "StorePathNotCached",
            Self::ClosureResolutionFailed(_) => "ClosureResolutionFailed",
            Self::Internal(_) => "InternalError",
        }
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorResponse {
            error: self.error_code().into(),
            message: self.to_string(),
        };
        (status, axum::Json(body)).into_response()
    }
}

impl From<NiphasError> for AppError {
    fn from(e: NiphasError) -> Self {
        match e {
            NiphasError::InvalidInput(msg) => Self::InvalidInput(msg),
            NiphasError::FlakeNotAllowed(msg) => Self::FlakeNotAllowed(msg),
            NiphasError::EvalTimeout(_) => Self::EvalTimeout,
            NiphasError::StorePathNotCached(msg) => Self::StorePathNotCached(msg),
            NiphasError::ClosureResolution(msg) => Self::ClosureResolutionFailed(msg),
            other => Self::Internal(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn status_of(err: AppError) -> StatusCode {
        err.into_response().status()
    }

    async fn body_of(err: AppError) -> serde_json::Value {
        let resp = err.into_response();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn test_invalid_input_400() {
        assert_eq!(
            status_of(AppError::InvalidInput("bad flake_ref".into())),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn test_flake_not_allowed_403() {
        assert_eq!(
            status_of(AppError::FlakeNotAllowed("bad".into())),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn test_eval_failed_422() {
        assert_eq!(
            status_of(AppError::EvalFailed("nix failed".into())),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn test_eval_timeout_408() {
        assert_eq!(
            status_of(AppError::EvalTimeout),
            StatusCode::REQUEST_TIMEOUT
        );
    }

    #[test]
    fn test_closure_resolution_502() {
        assert_eq!(
            status_of(AppError::ClosureResolutionFailed("fail".into())),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn test_internal_500() {
        assert_eq!(
            status_of(AppError::Internal("oops".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn test_error_body_shape() {
        let body = body_of(AppError::FlakeNotAllowed("github:evil/repo".into())).await;
        assert_eq!(body["error"], "FlakeNotAllowed");
        assert_eq!(body["message"], "flake not allowed: github:evil/repo");
    }

    #[tokio::test]
    async fn test_invalid_input_body_shape() {
        let body = body_of(AppError::InvalidInput("bad field".into())).await;
        assert_eq!(body["error"], "InvalidInput");
        assert_eq!(body["message"], "invalid input: bad field");
    }

    #[test]
    fn test_from_niphas_error() {
        let err: AppError = NiphasError::InvalidInput("bad".into()).into();
        assert!(matches!(err, AppError::InvalidInput(_)));

        let err: AppError = NiphasError::FlakeNotAllowed("x".into()).into();
        assert!(matches!(err, AppError::FlakeNotAllowed(_)));

        let err: AppError = NiphasError::EvalTimeout(300).into();
        assert!(matches!(err, AppError::EvalTimeout));

        let err: AppError = NiphasError::ClosureResolution("fail".into()).into();
        assert!(matches!(err, AppError::ClosureResolutionFailed(_)));

        let err: AppError = NiphasError::Io(std::io::Error::other("x")).into();
        assert!(matches!(err, AppError::Internal(_)));
    }
}
