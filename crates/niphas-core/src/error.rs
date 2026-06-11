use thiserror::Error;

#[derive(Error, Debug)]
pub enum NiphasError {
    // -- Kubernetes --
    #[error("kubernetes api error: {0}")]
    Kube(#[from] kube::Error),

    // -- Input validation --
    #[error("invalid input: {0}")]
    InvalidInput(String),

    // -- Nix evaluation --
    #[error("nix evaluation failed: {0}")]
    NixEval(String),

    #[error("evaluation timed out after {0}s")]
    EvalTimeout(u64),

    #[error("flake not allowed: {0}")]
    FlakeNotAllowed(String),

    // -- Store / NAR --
    #[error("store path not found: {0}")]
    StorePathNotFound(String),

    #[error("invalid store path: {0}")]
    InvalidStorePath(String),

    #[error("NAR parse error: {0}")]
    NarParse(String),

    #[error(".narinfo parse error: {0}")]
    NarInfoParse(String),

    // -- Integrity verification --
    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },

    #[error("signature verification failed: {0}")]
    SignatureVerification(String),

    // -- Binary cache --
    #[error("cache error: {0}")]
    Cache(String),

    #[error("store path not cached: {0}")]
    StorePathNotCached(String),

    #[error("closure resolution failed: {0}")]
    ClosureResolution(String),

    // -- Mount / CSI --
    #[error("mount failed: {0}")]
    Mount(String),

    // -- Infrastructure --
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("config error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, NiphasError>;
