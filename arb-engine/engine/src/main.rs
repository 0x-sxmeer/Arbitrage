// ─────────────────────────────────────────────────────────────────────────────
//  main.rs — Cross-Chain Arbitrage Engine Entry Point
//
//  Wires up all subsystems:
//    1. Configuration (from .env + env vars)
//    2. PostgreSQL persistent store (pool registry + opportunity log)
//    3. Redis hot cache (pool state TTL cache + deduplication)
//    4. EVM Manager (chain adapters for on-chain state fetching)
//    5. LiquidityGraph (shared, lock-protected)
//    6. EngineMetrics (atomic counters for monitoring)
//    7. MempoolListener (WebSocket subscription + evaluation pipeline)
//    8. Pool warm-up (pre-load pool registry from Postgres into graph)
//
//  The engine runs continuously until killed (Ctrl+C or SIGTERM).
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

mod api;
mod arb;
mod chains;
mod config;
mod db;
mod mempool;
mod metrics;
mod pool;

use arb::router::{LiquidityGraph, RouterConfig};
use chains::evm::{EvmAdapter, EvmConfig};
use config::Config;
use db::postgres::PostgresStore;
use db::redis::RedisCache;
use mempool::listener::MempoolListener;
use metrics::EngineMetrics;
use pool::ChainId;

#[tokio::main]
async fn main() {
    // ── Load configuration ────────────────────────────────────────────────────
    dotenvy::dotenv().ok();
    let config = match Config::from_env() {
        Ok(c)  => c,
        Err(e) => {
            eprintln!("✗ Failed to load configuration: {}", e);
            eprintln!("  Copy .env.example to .env and fill in required values.");
            std::process::exit(1);
        }
    };

    // ── Initialize tracing (structured logging) ──────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("arb_engine=info".parse().unwrap()),
        )
        .with_target(false)
        .compact()
        .init();

    info!("═══════════════════════════════════════════════════════════════");
    info!("  ⚡ Cross-Chain Arbitrage Engine — Phase 1");
    info!("═══════════════════════════════════════════════════════════════");
    config.log_summary();

    // ── Initialize metrics ────────────────────────────────────────────────────
    let metrics = Arc::new(EngineMetrics::new());

    // ── Connect Redis ─────────────────────────────────────────────────────────
    let redis_cache = match RedisCache::connect(&config.redis_url).await {
        Ok(cache) => {
            info!("✓ Redis connected at {}", config.redis_url);
            Arc::new(cache)
        }
        Err(e) => {
            error!("✗ Redis connection failed: {}", e);
            error!("  Start Redis with: docker-compose up -d cache");
            error!("  (engine requires Redis for pool state caching)");
            std::process::exit(1);
        }
    };

    // ── Connect PostgreSQL (optional) ─────────────────────────────────────────
    let pg_store: Option<Arc<PostgresStore>> = match config.database_url.as_deref() {
        Some(db_url) => {
            match PostgresStore::connect(db_url).await {
                Ok(store) => {
                    info!("✓ PostgreSQL connected at {}", redact_url(db_url));
                    let pg = Arc::new(store);

                    // Run migrations (create tables if they don't exist)
                    if let Err(e) = pg.run_migrations().await {
                        warn!("⚠ PostgreSQL migration warning: {}", e);
                        warn!("  Tables may already exist — continuing");
                    } else {
                        info!("  ✓ Migrations applied");
                    }

                    Some(pg)
                }
                Err(e) => {
                    warn!("⚠ PostgreSQL connection failed: {}", e);
                    warn!("  (engine will run without persistence)");
                    None
                }
            }
        }
        None => {
            warn!("⚠ DATABASE_URL not set — running without PostgreSQL persistence");
            warn!("  Set DATABASE_URL in .env to enable opportunity logging");
            None
        }
    };

    // ── Initialize EVM Adapter ────────────────────────────────────────────────
    let evm_adapter = {
        let evm_config = EvmConfig {
            chain:         ChainId::Ethereum,
            ws_url:        config.eth_ws_url.clone(),
            http_url:      config.eth_http_url.clone(),
            flashbots_url: Some(config.flashbots_url.clone()),
        };
        let adapter = EvmAdapter::new(evm_config);
        let ws_preview = if config.eth_ws_url.len() > 40 {
            &config.eth_ws_url[..40]
        } else {
            &config.eth_ws_url
        };
        info!("✓ EVM adapter initialized (chain=ethereum, ws={}...)", ws_preview);
        Some(Arc::new(adapter))
    };

    // ── Build LiquidityGraph ──────────────────────────────────────────────────
    let graph = Arc::new(RwLock::new(LiquidityGraph::new()));
    info!("✓ Liquidity graph initialized (empty — will populate from cache/chain)");

    // ── Pool warm-up from PostgreSQL ──────────────────────────────────────────
    if let Some(ref pg) = pg_store {
        match warm_up_graph_from_postgres(pg, &graph, &metrics).await {
            Ok(count) => {
                if count > 0 {
                    info!("✓ Graph warmed up with {} pools from PostgreSQL", count);
                } else {
                    info!("  (no pools in registry — graph starts empty)");
                }
            }
            Err(e) => {
                warn!("⚠ Pool warm-up failed: {} — graph starts empty", e);
            }
        }
    }

    // ── Build router config ───────────────────────────────────────────────────
    let router_config = RouterConfig {
        max_hops:         config.max_hops,
        min_profit_rate:  0.001, // 0.1% baseline
        reference_amount: crate::pool::U256::from(1_000_000_000_000_000_000u128), // 1 ETH
        eth_price_usd:    config.eth_price_usd,
        gas_price_gwei:   config.gas_price_gwei,
        gas_per_hop:      Config::GAS_PER_HOP,
    };

    // ── Start mempool listener ────────────────────────────────────────────────
    info!("═══════════════════════════════════════════════════════════════");
    info!("  🚀 Starting mempool listener...");
    info!("═══════════════════════════════════════════════════════════════");

    let listener = MempoolListener::new(
        config.eth_ws_url.clone(),
        config.solana_rpc_url.clone(),
        redis_cache.clone(),
        graph.clone(),
        router_config,
        pg_store,
        evm_adapter,
        metrics.clone(),
    );

    // Spawn metrics dashboard logger (every 60 seconds)
    let metrics_handle = metrics.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            metrics_handle.log_summary();
        }
    });

    // Start local API server for the React dashboard
    api::start_api_server(metrics.clone(), 3000).await;

    // Run listener (blocks forever)
    if let Err(e) = listener.run().await {
        error!("Mempool listener fatal error: {}", e);
        std::process::exit(1);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Pool warm-up — pre-loads pool registry from Postgres into the LiquidityGraph
// ─────────────────────────────────────────────────────────────────────────────

async fn warm_up_graph_from_postgres(
    pg: &Arc<PostgresStore>,
    graph: &Arc<RwLock<LiquidityGraph>>,
    metrics: &Arc<EngineMetrics>,
) -> anyhow::Result<usize> {
    let pools = pg.list_pools().await?;
    let count = pools.len();

    if count == 0 {
        return Ok(0);
    }

    let mut graph = graph.write().await;
    for pool in pools {
        graph.upsert_pool(pool);
    }

    metrics.set_graph_pools(count as u64);
    metrics.set_graph_tokens(graph.token_count() as u64);

    Ok(count)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Utilities
// ─────────────────────────────────────────────────────────────────────────────

/// Redact passwords in database URLs for safe logging.
fn redact_url(url: &str) -> String {
    if let Some(at_pos) = url.find('@') {
        if let Some(colon_pos) = url[..at_pos].rfind(':') {
            return format!("{}:***@{}", &url[..colon_pos], &url[at_pos + 1..]);
        }
    }
    url.to_string()
}
