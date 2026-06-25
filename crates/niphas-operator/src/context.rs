use kube::Client;
use kube_runtime::events::{Recorder, Reporter};
use niphas_core::config::NiphasConfig;
use reqwest::Client as HttpClient;

/// Shared context for the reconciler.
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,
    /// Reusable HTTP client for eval webhook calls.
    pub http: HttpClient,
    /// Event recorder for emitting K8s Events.
    pub recorder: Recorder,
    /// Operator configuration.
    pub config: NiphasConfig,
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
        }
    }
}
