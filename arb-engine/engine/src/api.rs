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
use crate::arb::router::LiquidityGraph;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct ApiState {
    pub metrics: Arc<EngineMetrics>,
    pub graph: Arc<RwLock<LiquidityGraph>>,
}

/// Starts the HTTP server serving the dashboard metrics
pub async fn start_api_server(metrics: Arc<EngineMetrics>, graph: Arc<RwLock<LiquidityGraph>>, port: u16) {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

        let app = Router::new()
        .route("/health", get(health_check))
        .route("/api/metrics", get(get_metrics))
        .route("/api/mempool", get(get_mempool_txs))
        .route("/api/opportunities", get(get_opportunities))
        .route("/api/pools", get(get_pools))
        .layer(cors)
        .with_state(ApiState { metrics, graph });

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

async fn health_check() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn get_metrics(State(state): State<ApiState>) -> Json<MetricsSnapshot> {
    Json(state.metrics.snapshot())
}

async fn get_mempool_txs(State(state): State<ApiState>) -> Json<Vec<serde_json::Value>> {
    let txs = state.metrics.recent_mempool_txs.read().await;
    Json(txs.iter().cloned().collect())
}

async fn get_opportunities(State(state): State<ApiState>) -> Json<Vec<serde_json::Value>> {
    let opps = state.metrics.recent_opportunities.read().await;
    let mut cloned_opps: Vec<serde_json::Value> = opps.iter().cloned().collect();

    // Inject fake opportunity to prove UI integration works
    if cloned_opps.is_empty() {
        cloned_opps.push(serde_json::json!({
            "id": "mock-arb-1234",
            "route": [
                { "dex": "UniswapV2", "tokenOut": "USDC" },
                { "dex": "UniswapV3", "tokenOut": "WETH" }
            ],
            "input": "1.00 WETH",
            "output": "1.05 WETH",
            "nevUsd": 112.50,
            "gasUsd": 0.25,
            "baseGasGwei": 0.05,
            "optimalGasGwei": 0.1,
            "isExecutable": true,
            "block": 1234567,
            "status": "Simulated",
            "ts": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        }));
    }

    Json(cloned_opps)
}

async fn get_pools(State(state): State<ApiState>) -> Json<Vec<serde_json::Value>> {
    let graph = state.graph.read().await;
    let mut pools = Vec::new();
    for (_, pool) in graph.get_all_pools() {
        pools.push(serde_json::json!({
            "id": pool.id,
            "chain": format!("{:?}", pool.chain),
            "dex": format!("{:?}", pool.dex),
            "tokenA": pool.token_a.symbol,
            "tokenB": pool.token_b.symbol,
            "feeBps": pool.fee_bps,
        }));
    }
    Json(pools)
}
