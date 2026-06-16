use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get, routing::post};
use niphas_core::eval::{EvalRequest, EvalResponse};
use std::net::SocketAddr;

async fn evaluate(Json(req): Json<EvalRequest>) -> impl IntoResponse {
    // Simulate failure for flake refs containing "nonexistent" or "does-not-exist"
    if req.flake_ref.contains("nonexistent") || req.flake_ref.contains("does-not-exist") {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("evaluation failed: flake '{}' not found", req.flake_ref),
        ));
    }

    let store_path = format!(
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-{}",
        req.flake_ref.split(':').next_back().unwrap_or("mock")
    );
    let name = req
        .flake_ref
        .split('/')
        .next_back()
        .unwrap_or("mock-app")
        .to_string();

    Ok(Json(EvalResponse {
        store_path: store_path.clone(),
        name: name.clone(),
        main_program: Some(name),
        closure_paths: vec![store_path],
    }))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() {
    let addr: SocketAddr = std::env::var("LISTEN_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()
        .expect("invalid LISTEN_ADDR");

    let app = Router::new()
        .route("/evaluate", post(evaluate))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz));

    println!("mock-eval listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
