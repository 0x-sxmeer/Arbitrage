use std::sync::Arc;
use axum::{
    extract::State,
    routing::get,
    Json, Router,
};
use tower_http::cors::{Any, CorsLayer};
use tokio::net::TcpListener;
use tracing::info;

use crate::metrics::{EngineMetrics, MetricsSnapshot};

/// Starts the HTTP server serving the dashboard metrics
pub async fn start_api_server(metrics: Arc<EngineMetrics>, port: u16) {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

        let app = Router::new()
        .route("/api/metrics", get(get_metrics))
        .route("/api/mempool", get(get_mempool_txs))
        .layer(cors)
        .with_state(metrics);

    let addr = format!("0.0.0.0:{}", port);
    
    // We run in a separate task so we don't block the caller
    tokio::spawn(async move {
        match TcpListener::bind(&addr).await {
            Ok(listener) => {
                info!("✓ API server running on http://{}", addr);
                if let Err(e) = axum::serve(listener, app).await {
                    tracing::error!("API server error: {}", e);
                }
            }
            Err(e) => {
                tracing::error!("Failed to bind API server on {}: {}", addr, e);
            }
        }
    });
}

async fn get_metrics(State(metrics): State<Arc<EngineMetrics>>) -> Json<MetricsSnapshot> {
    Json(metrics.snapshot())
}

async fn get_mempool_txs(State(metrics): State<Arc<EngineMetrics>>) -> Json<Vec<serde_json::Value>> {
    let txs = metrics.recent_mempool_txs.read().await;
    Json(txs.iter().cloned().collect())
}
