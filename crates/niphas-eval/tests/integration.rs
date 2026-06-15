use niphas_core::config::NiphasConfig;
use niphas_eval::evaluator::Evaluator;
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Spawn the eval server on a random port and return the base URL.
async fn spawn_app() -> String {
    let config = NiphasConfig {
        allowed_flake_origins: vec!["github:myorg/*".into()],
        ..Default::default()
    };

    let evaluator = Arc::new(Evaluator::new(config).expect("evaluator init"));
    let app = niphas_eval::app(evaluator);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn healthz_returns_200() {
    let base = spawn_app().await;
    let resp = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn readyz_returns_503_when_cold() {
    let base = spawn_app().await;
    let resp = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn evaluate_invalid_body_returns_400() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/evaluate"))
        .header("content-type", "application/json")
        .body(r#"{"not": "valid"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);
}

#[tokio::test]
async fn evaluate_flake_not_in_allowlist_returns_403() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/evaluate"))
        .header("content-type", "application/json")
        .body(
            json!({
                "flakeRef": "github:evil/repo",
                "attribute": "packages.x86_64-linux.default"
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "FlakeNotAllowed");
}

#[tokio::test]
async fn evaluate_empty_body_returns_422() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/evaluate"))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    // Missing required fields triggers Axum deserialization error (422)
    assert_eq!(resp.status(), 422);
}

#[tokio::test]
async fn evaluate_injection_chars_returns_400() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/evaluate"))
        .header("content-type", "application/json")
        .body(
            json!({
                "flakeRef": r#"github:myorg/app" ++ builtins.readFile /etc/shadow ++ ""#,
                "attribute": "packages.x86_64-linux.default"
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "InvalidInput");
}

#[tokio::test]
async fn evaluate_allowed_flake_without_nix_returns_422() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();
    // Flake passes allowlist but nix eval subprocess will fail (no nix binary in test env)
    let resp = client
        .post(format!("{base}/evaluate"))
        .header("content-type", "application/json")
        .body(
            json!({
                "flakeRef": "github:myorg/app",
                "attribute": "packages.x86_64-linux.default"
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "EvalFailed");
}

#[tokio::test]
async fn evaluate_wrong_content_type_returns_415() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/evaluate"))
        .header("content-type", "text/plain")
        .body("not json")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 415);
}
