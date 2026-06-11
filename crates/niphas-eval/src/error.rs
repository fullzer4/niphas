use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug)]
pub enum AppError {
    InvalidInput(String),
    FlakeNotAllowed(String),
    EvalFailed(String),
    EvalTimeout,
    StorePathNotCached(String),
    ClosureResolutionFailed(String),
    Internal(String),
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            AppError::InvalidInput(msg) => (StatusCode::BAD_REQUEST, "InvalidInput", msg),
            AppError::FlakeNotAllowed(msg) => (StatusCode::FORBIDDEN, "FlakeNotAllowed", msg),
            AppError::EvalFailed(msg) => (StatusCode::UNPROCESSABLE_ENTITY, "EvalFailed", msg),
            AppError::EvalTimeout => (
                StatusCode::REQUEST_TIMEOUT,
                "EvalTimeout",
                "Evaluation exceeded timeout".into(),
            ),
            AppError::StorePathNotCached(msg) => (StatusCode::NOT_FOUND, "StorePathNotCached", msg),
            AppError::ClosureResolutionFailed(msg) => {
                (StatusCode::BAD_GATEWAY, "ClosureResolutionFailed", msg)
            }
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, "InternalError", msg),
        };

        let body = ErrorResponse {
            error: code.into(),
            message,
        };

        (status, axum::Json(body)).into_response()
    }
}

impl From<niphas_core::error::NiphasError> for AppError {
    fn from(e: niphas_core::error::NiphasError) -> Self {
        match e {
            niphas_core::error::NiphasError::InvalidInput(msg) => AppError::InvalidInput(msg),
            niphas_core::error::NiphasError::FlakeNotAllowed(msg) => AppError::FlakeNotAllowed(msg),
            niphas_core::error::NiphasError::EvalTimeout(_) => AppError::EvalTimeout,
            niphas_core::error::NiphasError::StorePathNotCached(msg) => {
                AppError::StorePathNotCached(msg)
            }
            niphas_core::error::NiphasError::ClosureResolution(msg) => {
                AppError::ClosureResolutionFailed(msg)
            }
            other => AppError::Internal(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn status_of(err: AppError) -> StatusCode {
        let resp = err.into_response();
        resp.status()
    }

    async fn body_of(err: AppError) -> serde_json::Value {
        let resp = err.into_response();
        let body = resp.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_invalid_input_400() {
        assert_eq!(
            status_of(AppError::InvalidInput("bad flake_ref".into())).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn test_flake_not_allowed_403() {
        assert_eq!(
            status_of(AppError::FlakeNotAllowed("bad".into())).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn test_eval_failed_422() {
        assert_eq!(
            status_of(AppError::EvalFailed("nix failed".into())).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn test_eval_timeout_408() {
        assert_eq!(
            status_of(AppError::EvalTimeout).await,
            StatusCode::REQUEST_TIMEOUT
        );
    }

    #[tokio::test]
    async fn test_closure_resolution_502() {
        assert_eq!(
            status_of(AppError::ClosureResolutionFailed("fail".into())).await,
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn test_internal_500() {
        assert_eq!(
            status_of(AppError::Internal("oops".into())).await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn test_error_body_shape() {
        let body = body_of(AppError::FlakeNotAllowed("github:evil/repo".into())).await;
        assert_eq!(body["error"], "FlakeNotAllowed");
        assert_eq!(body["message"], "github:evil/repo");
    }

    #[tokio::test]
    #[tokio::test]
    async fn test_invalid_input_body_shape() {
        let body = body_of(AppError::InvalidInput("bad field".into())).await;
        assert_eq!(body["error"], "InvalidInput");
        assert_eq!(body["message"], "bad field");
    }

    #[tokio::test]
    async fn test_from_niphas_error() {
        use niphas_core::error::NiphasError;

        let err: AppError = NiphasError::InvalidInput("bad".into()).into();
        assert!(matches!(err, AppError::InvalidInput(_)));

        let err: AppError = NiphasError::FlakeNotAllowed("x".into()).into();
        assert!(matches!(err, AppError::FlakeNotAllowed(_)));

        let err: AppError = NiphasError::EvalTimeout(300).into();
        assert!(matches!(err, AppError::EvalTimeout));

        let err: AppError = NiphasError::ClosureResolution("fail".into()).into();
        assert!(matches!(err, AppError::ClosureResolutionFailed(_)));

        let err: AppError =
            NiphasError::Io(std::io::Error::new(std::io::ErrorKind::Other, "x")).into();
        assert!(matches!(err, AppError::Internal(_)));
    }
}
