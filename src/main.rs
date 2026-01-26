use axum::{
    extract::Json,
    http::StatusCode,
    response::Json as ResponseJson,
    routing::post,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::services::ServeDir;

mod contract_processor;
mod cache;

#[derive(Deserialize)]
struct ContractRequest {
    url: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[tokio::main]
async fn main() {
    // Initialize cache
    let cache = Arc::new(cache::Cache::new().expect("Failed to initialize cache"));
    println!("Cache initialized");

    let app = Router::new()
        .route("/contract", post(handle_contract))
        .nest_service("/", ServeDir::new("static"))
        .layer(
            ServiceBuilder::new()
                .layer(axum::middleware::from_fn(add_cors_headers))
        )
        .with_state(cache);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .unwrap();
    println!("Server running on http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}

async fn add_cors_headers(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    let mut res = next.run(req).await;
    res.headers_mut().insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        axum::http::HeaderValue::from_static("*"),
    );
    Ok(res)
}

async fn handle_contract(
    axum::extract::State(cache): axum::extract::State<Arc<cache::Cache>>,
    Json(payload): Json<ContractRequest>,
) -> Result<ResponseJson<serde_json::Value>, (StatusCode, ResponseJson<ErrorResponse>)> {
    println!("Received URL: {}", payload.url);
    
    match contract_processor::process_contract_url(&payload.url, cache).await {
        Ok(result) => Ok(ResponseJson(result)),
        Err(e) => {
            eprintln!("Error processing contract: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                ResponseJson(ErrorResponse {
                    error: e.to_string(),
                }),
            ))
        }
    }
}
