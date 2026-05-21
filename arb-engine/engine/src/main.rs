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
mod executor;
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
    let use_json_log = std::env::var("LOG_FORMAT").unwrap_or_default() == "json";
    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("arb_engine=debug".parse().unwrap());

    if use_json_log {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
            .with_current_span(false)
            .with_span_list(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .compact()
            .init();
    }

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
    // ── Pick the active chain WS URL — prefer Base if configured ────────────
    // Base L2: <$0.001/tx gas fees, Aave V3 deployed, Uniswap V3 + Aerodrome
    let (active_chain, active_ws_url, active_http_url) =
        if let Some(ref base_ws) = config.base_ws_url {
            info!("  ✓ Base L2 WS configured — targeting Base Mainnet (chain 8453)");
            let base_http = config.base_http_url.clone().unwrap_or_else(|| base_ws.replace("wss://", "https://"));
            (ChainId::Base, base_ws.clone(), base_http)
        } else {
            warn!("  ⚠ BASE_WS_URL not set — falling back to Ethereum mainnet");
            (ChainId::Ethereum, config.eth_ws_url.clone(), config.eth_http_url.clone())
        };

    // ── Initialize Flashbots Submitter ────────────────────────────────────────
    let flashbots_submitter = if let Some(ref signing_key) = config.flashbots_signing_key {
        let contract_addr = if let Some(ref addr) = config.contract_address {
            use std::str::FromStr;
            alloy::primitives::Address::from_str(addr).unwrap_or(alloy::primitives::Address::ZERO)
        } else {
            alloy::primitives::Address::ZERO
        };
        match executor::FlashbotsSubmitter::new(
            config.flashbots_url.clone(),
            signing_key,
            contract_addr,
            active_chain.evm_chain_id().unwrap_or(1),
        ) {
            Ok(submitter) => {
                info!("✓ Flashbots Submitter initialized at {}", config.flashbots_url);
                Some(Arc::new(submitter))
            }
            Err(e) => {
                warn!("⚠ Failed to initialize Flashbots Submitter: {}", e);
                None
            }
        }
    } else {
        None
    };

    let evm_adapter = {
        let evm_config = EvmConfig {
            chain:            active_chain,
            ws_url:           active_ws_url.clone(),
            http_url:         active_http_url.clone(),
            flashbots_url:    Some(config.flashbots_url.clone()),
            private_key:      config.private_key.clone(),
            contract_address: config.contract_address.clone(),
            flashbots_signing_key: config.flashbots_signing_key.clone(),
            private_rpc_url:  config.private_rpc_url.clone(),
        };
        let adapter = EvmAdapter::new(evm_config, flashbots_submitter.clone());
        let ws_preview = if active_ws_url.len() > 40 {
            &active_ws_url[..40]
        } else {
            &active_ws_url
        };
        info!("✓ EVM adapter initialized (chain={}, ws={}...)", active_chain.name(), ws_preview);
        Some(Arc::new(adapter))
    };

    // ── Build LiquidityGraph ──────────────────────────────────────────────────
    let graph = Arc::new(RwLock::new(LiquidityGraph::new()));
    info!("✓ Liquidity graph initialized (empty — will populate from cache/chain)");

    // ── Pool warm-up from PostgreSQL ──────────────────────────────────────────
    if let Some(ref pg) = pg_store {
        match warm_up_and_sync_pools_from_postgres(pg, &graph, &metrics, active_chain, evm_adapter.as_deref().unwrap()).await {
            Ok(count) => {
                if count > 0 {
                    info!("✓ Warmed up and synced {} pools from PostgreSQL", count);
                } else {
                    info!("  (no pools in registry for {:?} — run `cargo run --bin seed-base-pools`)", active_chain.name());
                }
            }
            Err(e) => {
                warn!("⚠ Pool warm-up failed: {} — graph starts empty", e);
            }
        }
    }

    // ── Fetch Aave Flash Loan fee dynamically ────────────────────────────────
    let mut aave_fee_bps: u32 = 5;
    if let Some(ref evm) = evm_adapter {
        // Try direct fetch first (works before contract deployment)
        let aave_pool_for_chain = match active_chain {
            ChainId::Base     => Some("0xA238Dd80C259a72e81d7e4664a9801593F98d1c5"),
            ChainId::Arbitrum => Some("0x794a61358D6845594F94dc1DB02A252b5b4814aD"),
            ChainId::Ethereum => Some("0x87870B27f0bf4296857d44E8a96a1B714F24F5C9"),
            _ => None,
        };
        if let Some(pool_addr) = config.aave_pool_address.as_deref().or(aave_pool_for_chain) {
            match evm.get_aave_premium_direct(pool_addr).await {
                Ok(p) => { info!("✓ Aave flash loan fee: {} bps", p); aave_fee_bps = p; }
                Err(e) => warn!("⚠ Aave fee fetch failed: {} — using {} bps default", e, aave_fee_bps),
            }
        }
    }

    // ── Build router config ───────────────────────────────────────────────────
    let router_config = RouterConfig {
        gas_price_gwei:       config.gas_price_gwei,
        gas_estimate:         350_000,
        eth_price_usd:        config.eth_price_usd,
        btc_price_usd:        config.btc_price_usd,
        min_profit_usd:       config.min_profit_usd,
        reference_amount:     crate::pool::U256::from(1_000_000_000_000_000_000u128),
        max_price_impact_bps: config.max_price_impact_bps,
        max_hops:             config.max_hops,
        verbose:              false,
        aave_fee_bps,
    };
    // ── Start mempool listener ────────────────────────────────────────────────
    info!("═══════════════════════════════════════════════════════════════");
    info!("  🚀 Starting mempool listener...");
    info!("═══════════════════════════════════════════════════════════════");

    // Use the active chain WS URL for mempool streaming (Base if configured)
    let listener = MempoolListener::new(
        active_ws_url.clone(),
        config.solana_ws_url.clone(),
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
    tokio::spawn(async move {
        api::start_api_server(metrics.clone(), 3000).await;
    });

    // Run listener (blocks forever)
    if let Err(e) = listener.run().await {
        error!("Mempool listener fatal error: {}", e);
        std::process::exit(1);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Pool warm-up — pre-loads pool registry from Postgres into the LiquidityGraph
// ─────────────────────────────────────────────────────────────────────────────

async fn warm_up_and_sync_pools_from_postgres(
    pg: &Arc<PostgresStore>,
    graph: &Arc<RwLock<LiquidityGraph>>,
    metrics: &Arc<EngineMetrics>,
    active_chain: crate::pool::ChainId,
    evm: &EvmAdapter,
) -> anyhow::Result<usize> {
    let all_pools = pg.list_pools().await?;
    let mut pools = Vec::new();
    for p in all_pools {
        if p.chain == active_chain {
            pools.push(p);
        }
    }

    if pools.is_empty() {
        return Ok(0);
    }

    info!("  ⏳ Fetching live pool states for {} pools via Multicall3...", pools.len());

    let mut synced = 0usize;
    let mut failed = 0usize;
    let mut g = graph.write().await;

    // Process in chunks of 50 to avoid RPC timeouts
    for chunk in pools.chunks(50) {
        match evm.fetch_pool_states_multicall(chunk).await {
            Ok(states) => {
                for (i, state) in states.into_iter().enumerate() {
                    let mut p = chunk[i].clone();
                    
                    let is_empty = match p.pool_type {
                        crate::pool::PoolType::ConcentratedLiquidity => state.sqrt_price_x96.is_none() || state.liquidity.map_or(true, |l| l == 0),
                        _ => state.reserve_a.is_zero() && state.reserve_b.is_zero(),
                    };

                    if is_empty {
                        failed += 1;
                        continue;
                    }

                    p.state = state;
                    p.last_updated_ts = chrono::Utc::now().timestamp();
                    g.upsert_pool(p);
                    synced += 1;
                }
            }
            Err(e) => {
                warn!("⚠ Multicall chunk fetch failed: {}", e);
                failed += chunk.len();
            }
        }
    }

    metrics.set_graph_pools(g.pool_count() as u64);
    metrics.set_graph_tokens(g.token_count() as u64);
    
    info!("✓ Synced {} pools successfully ({} empty/failed)", synced, failed);

    Ok(synced)
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
