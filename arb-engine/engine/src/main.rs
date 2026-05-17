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
            private_key:   config.private_key.clone(),
            contract_address: config.contract_address.clone(),
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

    // ─────────────────────────────────────────────────────────────────────────
    //  🔥 LIVE MAINNET WARMUP: Fetch real-time pool states from Alchemy
    //  This is the critical bridge between "Brain" and "Blockchain"
    // ─────────────────────────────────────────────────────────────────────────
    {
        use crate::pool::{Pool, PoolType, PoolState, Token, DexProtocol};
        use crate::pool::U256;
        
        // ── Token definitions (checksummed addresses) ─────────────────────────
        let weth = Token { address: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".into(), symbol: "WETH".into(), decimals: 18 };
        let usdc = Token { address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".into(), symbol: "USDC".into(), decimals: 6 };
        let wbtc = Token { address: "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599".into(), symbol: "WBTC".into(), decimals: 8 };
        let dai  = Token { address: "0x6B175474E89094C44Da98b954EedeAC495271d0F".into(), symbol: "DAI".into(),  decimals: 18 };
        let usdt = Token { address: "0xdAC17F958D2ee523a2206206994597C13D831ec7".into(), symbol: "USDT".into(), decimals: 6 };

        // ── Real Uniswap V3 Pool Addresses (Ethereum mainnet) ─────────────────
        // These are the highest-TVL pools — the primary arbitrage battleground.
        struct PoolDef<'a> {
            address:  &'a str,
            token_a:  &'a Token,
            token_b:  &'a Token,
            fee_bps:  u32,
            label:    &'a str,
        }

        let pools_to_sync = [
            PoolDef { address: "0x88e6a0c2ddd26feeb64f039a2c412e6eb18a3014", token_a: &usdc, token_b: &weth, fee_bps: 500,  label: "USDC/WETH 0.05%" },
            PoolDef { address: "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed", token_a: &wbtc, token_b: &weth, fee_bps: 3000, label: "WBTC/WETH 0.3%"  },
            PoolDef { address: "0x5777d92f208679db4b9778590fa3cab3ac9e2168", token_a: &dai,  token_b: &usdc, fee_bps: 100,  label: "DAI/USDC 0.01%"  },
            PoolDef { address: "0x11b815efb8f581194ae5486326431ce0c3c65f48", token_a: &usdt, token_b: &weth, fee_bps: 500,  label: "USDT/WETH 0.05%" },
        ];

        let evm = evm_adapter.as_ref().unwrap();
        let mut synced = 0u32;
        let mut failed = 0u32;

        info!("  ⏳ Fetching live pool states from Alchemy ({} pools)...", pools_to_sync.len());

        let mut g = graph.write().await;

        for def in &pools_to_sync {
            match evm.get_v3_pool_state(def.address).await {
                Ok((sqrt_price, tick, liq)) => {
                    g.upsert_pool(Pool {
                        id: def.address.into(),
                        chain: ChainId::Ethereum,
                        dex: DexProtocol::UniswapV3,
                        token_a: def.token_a.clone(),
                        token_b: def.token_b.clone(),
                        pool_type: PoolType::ConcentratedLiquidity,
                        fee_bps: def.fee_bps,
                        last_updated_block: 0,
                        last_updated_ts: chrono::Utc::now().timestamp(),
                        state: PoolState {
                            reserve_a:      U256::zero(),
                            reserve_b:      U256::zero(),
                            sqrt_price_x96: Some(sqrt_price),
                            tick:           Some(tick),
                            liquidity:      Some(liq),
                            amp_coeff:      None,
                        },
                    });
                    synced += 1;
                    info!(
                        pool  = %def.label,
                        tick  = tick,
                        liq   = liq,
                        "  ✓ {} synced",
                        def.label
                    );
                }
                Err(e) => {
                    failed += 1;
                    warn!(
                        pool  = %def.label,
                        error = %e,
                        "  ⚠ {} fetch failed — using simulated state",
                        def.label
                    );
                    // Insert with simulated defaults so the graph still has connectivity
                    g.upsert_pool(Pool {
                        id: def.address.into(),
                        chain: ChainId::Ethereum,
                        dex: DexProtocol::UniswapV3,
                        token_a: def.token_a.clone(),
                        token_b: def.token_b.clone(),
                        pool_type: PoolType::ConcentratedLiquidity,
                        fee_bps: def.fee_bps,
                        last_updated_block: 0,
                        last_updated_ts: 0,
                        state: PoolState {
                            reserve_a:      U256::zero(),
                            reserve_b:      U256::zero(),
                            sqrt_price_x96: Some(U256::from(1_936_540_681_085_355_540_000_000_000_000u128)),
                            tick:           Some(201_210),
                            liquidity:      Some(12_345_678_901_234_567_890),
                            amp_coeff:      None,
                        },
                    });
                }
            }
        }
        
        metrics.set_graph_pools(g.pool_count() as u64);
        metrics.set_graph_tokens(g.token_count() as u64);

        if failed == 0 {
            info!("✓ All {} pools synchronized with LIVE mainnet prices via Alchemy!", synced);
        } else {
            warn!("⚠ {}/{} pools synced ({} failed — using simulated fallbacks)", synced, synced + failed, failed);
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
        max_price_impact_bps: 200,
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
