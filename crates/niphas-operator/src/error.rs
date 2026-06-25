use thiserror::Error;

#[derive(Error, Debug)]
pub enum OperatorError {
    #[error("kubernetes api error: {0}")]
    Kube(#[from] kube::Error),

    #[error("eval webhook error: {0}")]
    EvalWebhook(String),

    #[error("eval timeout after {0}s")]
    EvalTimeout(u64),

    #[error("eval failed: {code}: {message}")]
    EvalFailed { code: String, message: String },

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Map OperatorError to a kube-runtime Action for the error_policy.
impl OperatorError {
    /// Whether this error is transient and should be retried.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Kube(_) | Self::EvalWebhook(_) | Self::EvalTimeout(_)
        )
    }

    /// Extract a machine-readable reason and human-readable message for K8s conditions/events.
    pub fn reason_and_message(&self) -> (String, String) {
        match self {
            Self::EvalTimeout(secs) => (
                "EvalTimeout".into(),
                format!("Evaluation timed out after {secs}s"),
            ),
            Self::EvalFailed { code, message } => (code.clone(), message.clone()),
            other => ("EvalFailed".into(), other.to_string()),
        }
    }
}
