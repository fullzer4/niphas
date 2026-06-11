use kube::Client;
use kube_runtime::events::{Recorder, Reporter};
use niphas_core::config::NiphasConfig;
use reqwest::Client as HttpClient;
use std::sync::Arc;

/// Shared context for the reconciler and health server.
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,
    /// Reusable HTTP client for eval webhook calls.
    pub http: HttpClient,
    /// Event recorder for emitting K8s Events.
    pub recorder: Recorder,
    /// Operator configuration.
    pub config: NiphasConfig,
    /// Whether the controller is ready (leader elected + watching).
    pub ready: Arc<std::sync::atomic::AtomicBool>,
}

impl Context {
    pub fn new(client: Client, config: NiphasConfig) -> Self {
        let recorder = Recorder::new(
            client.clone(),
            Reporter {
                controller: "niphas-operator".into(),
                instance: std::env::var("POD_NAME").ok(),
            },
        );

        Self {
            client,
            http: HttpClient::new(),
            recorder,
            config,
            ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}
