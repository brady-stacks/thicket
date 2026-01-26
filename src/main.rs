use axum::{
    extract::Json,
    http::StatusCode,
    response::Json as ResponseJson,
    routing::{get, post},
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

#[derive(Deserialize)]
struct ContractSourceRequest {
    source: String,
}

#[derive(Deserialize)]
struct SourceKeyRequest {
    key: String,
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

    let app = create_app(cache);

    // Support PORT environment variable (Railway provides this)
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse::<u16>()
        .expect("PORT must be a valid number");
    
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap();
    println!("Server running on http://{}", addr);
    axum::serve(listener, app).await.unwrap();
}

fn create_app(cache: Arc<cache::Cache>) -> Router {
    Router::new()
        .route("/contract", post(handle_contract))
        .route("/contract-source", post(handle_contract_source))
        .route("/recent-urls", get(handle_recent_urls))
        .route("/recent-sources", get(handle_recent_sources))
        .route("/source-by-key", post(handle_source_by_key))
        .nest_service("/", ServeDir::new("static"))
        .layer(
            ServiceBuilder::new()
                .layer(axum::middleware::from_fn(add_cors_headers))
        )
        .with_state(cache)
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

async fn handle_contract_source(
    axum::extract::State(cache): axum::extract::State<Arc<cache::Cache>>,
    Json(payload): Json<ContractSourceRequest>,
) -> Result<ResponseJson<serde_json::Value>, (StatusCode, ResponseJson<ErrorResponse>)> {
    println!("Received source code (length: {} chars)", payload.source.len());
    
    match contract_processor::process_contract_source(&payload.source, cache).await {
        Ok(result) => Ok(ResponseJson(result)),
        Err(e) => {
            eprintln!("Error processing contract source: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                ResponseJson(ErrorResponse {
                    error: e.to_string(),
                }),
            ))
        }
    }
}

async fn handle_recent_urls(
    axum::extract::State(cache): axum::extract::State<Arc<cache::Cache>>,
) -> Result<ResponseJson<serde_json::Value>, (StatusCode, ResponseJson<ErrorResponse>)> {
    match tokio::task::spawn_blocking({
        let cache = cache.clone();
        move || cache.get_recent_urls(10)
    }).await {
        Ok(Ok(urls)) => Ok(ResponseJson(serde_json::json!({ "urls": urls }))),
        Ok(Err(e)) => {
            eprintln!("Error fetching recent URLs: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                ResponseJson(ErrorResponse {
                    error: e.to_string(),
                }),
            ))
        }
        Err(e) => {
            eprintln!("Task error: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                ResponseJson(ErrorResponse {
                    error: e.to_string(),
                }),
            ))
        }
    }
}

async fn handle_recent_sources(
    axum::extract::State(cache): axum::extract::State<Arc<cache::Cache>>,
) -> Result<ResponseJson<serde_json::Value>, (StatusCode, ResponseJson<ErrorResponse>)> {
    match tokio::task::spawn_blocking({
        let cache = cache.clone();
        move || cache.get_recent_sources(10)
    }).await {
        Ok(Ok(sources)) => {
            // Format sources: return key and first 3 lines of source code
            let formatted: Vec<serde_json::Value> = sources
                .into_iter()
                .map(|(key, source_code)| {
                    let first_lines: String = source_code
                        .lines()
                        .take(3)
                        .collect::<Vec<_>>()
                        .join("\n");
                    serde_json::json!({
                        "key": key,
                        "preview": first_lines
                    })
                })
                .collect();
            Ok(ResponseJson(serde_json::json!({ "sources": formatted })))
        }
        Ok(Err(e)) => {
            eprintln!("Error fetching recent sources: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                ResponseJson(ErrorResponse {
                    error: e.to_string(),
                }),
            ))
        }
        Err(e) => {
            eprintln!("Task error: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                ResponseJson(ErrorResponse {
                    error: e.to_string(),
                }),
            ))
        }
    }
}

async fn handle_source_by_key(
    axum::extract::State(cache): axum::extract::State<Arc<cache::Cache>>,
    Json(payload): Json<SourceKeyRequest>,
) -> Result<ResponseJson<serde_json::Value>, (StatusCode, ResponseJson<ErrorResponse>)> {
    match tokio::task::spawn_blocking({
        let cache = cache.clone();
        let key = payload.key.clone();
        move || cache.get(&key)
    }).await {
        Ok(Ok(Some((source_code, _cost_map)))) => {
            Ok(ResponseJson(serde_json::json!({ "source_code": source_code })))
        }
        Ok(Ok(None)) => {
            Err((
                StatusCode::NOT_FOUND,
                ResponseJson(ErrorResponse {
                    error: "Source not found".to_string(),
                }),
            ))
        }
        Ok(Err(e)) => {
            eprintln!("Error fetching source by key: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                ResponseJson(ErrorResponse {
                    error: e.to_string(),
                }),
            ))
        }
        Err(e) => {
            eprintln!("Task error: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                ResponseJson(ErrorResponse {
                    error: e.to_string(),
                }),
            ))
        }
    }
}
