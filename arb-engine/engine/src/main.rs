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

    let evm_adapter = {
        let evm_config = EvmConfig {
            chain:            active_chain,
            ws_url:           active_ws_url.clone(),
            http_url:         active_http_url.clone(),
            flashbots_url:    Some(config.flashbots_url.clone()),
            private_key:      config.private_key.clone(),
            contract_address: config.contract_address.clone(),
            flashbots_signing_key: config.flashbots_signing_key.clone(),
        };
        let adapter = EvmAdapter::new(evm_config);
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
        // ── Base L2 token addresses (checksummed) ─────────────────────────────
        // Base canonical bridged tokens — identical ERC-20 interfaces, L2 addresses
        let weth = Token { address: "0x4200000000000000000000000000000000000006".to_lowercase(), symbol: "WETH".into(), decimals: 18 };
        let usdc = Token { address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_lowercase(), symbol: "USDC".into(), decimals: 6 };
        let wbtc = Token { address: "0x0555E30da8f98308EdB960aa94C0Db47230d2B9c".to_lowercase(), symbol: "WBTC".into(), decimals: 8 };
        let dai  = Token { address: "0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb".to_lowercase(), symbol: "DAI".into(),  decimals: 18 };
        let usdt = Token { address: "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2".to_lowercase(), symbol: "USDT".into(), decimals: 6 };

        // ── Base L2 Pool Addresses ─────────────────────────────────────────────
        // Highest TVL pools on Base — primary arbitrage battleground.
        // Uniswap V3 (CLMM) + Aerodrome Finance V2 (constant product, the
        // dominant AMM on Base with >$500M TVL) give us rich cross-DEX gaps.
        struct PoolDef<'a> {
            address:  &'a str,
            token_a:  &'a Token,
            token_b:  &'a Token,
            fee_bps:  u32,
            label:    &'a str,
        }

        let pools_to_sync = [
            // Uniswap V3 on Base — top-volume CLMM pools
            PoolDef { address: "0xd0b53D9277642d899DF5C87A3966A349A798F224", token_a: &weth, token_b: &usdc, fee_bps: 5,    label: "UniV3 USDC/WETH 0.05% (Base)" },
            PoolDef { address: "0x4C36388bE6F416A29C8d8Eee81C771cE6bE14B5", token_a: &wbtc, token_b: &weth, fee_bps: 30,   label: "UniV3 WBTC/WETH 0.3% (Base)"  },
            PoolDef { address: "0x6c561B446416E1A00E8E93E221854d6eA4171372", token_a: &dai,  token_b: &usdc, fee_bps: 1,    label: "UniV3 DAI/USDC 0.01% (Base)"  },
            PoolDef { address: "0xfBB6Eed8e7aa03B138556eeDaF5D271A5E1e43ef", token_a: &weth, token_b: &usdt, fee_bps: 5,    label: "UniV3 USDT/WETH 0.05% (Base)" },
        ];

        let evm = evm_adapter.as_ref().unwrap();
        let mut synced = 0u32;
        let mut failed = 0u32;

        info!("  ⏳ Fetching live pool states from Alchemy ({} V3 pools)...", pools_to_sync.len());

        let mut g = graph.write().await;

        for def in &pools_to_sync {
            match evm.get_v3_pool_state(def.address).await {
                Ok((sqrt_price, tick, liq)) => {
                    g.upsert_pool(Pool {
                        id: def.address.to_lowercase(),
                        chain: active_chain,
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
                        id: def.address.to_lowercase(),
                        chain: active_chain,
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

        // ── Phase C: V2 SushiSwap pool warm-up ────────────────────────────────
        // These are the highest-volume V2 pairs and the primary cross-DEX
        // arbitrage targets against the Uniswap V3 pools above.
        struct V2PoolDef<'a> {
            address:  &'a str,
            token_a:  &'a Token,
            token_b:  &'a Token,
            fee_bps:  u32,
            dex:      DexProtocol,
            label:    &'a str,
        }

        // Aerodrome Finance V2 on Base — the dominant V2 AMM on Base (>$500M TVL).
        // These are the primary cross-DEX arb targets against Uniswap V3 above.
        // Aerodrome uses a 0.3% fee (30 bps) identical to Uniswap V2 math.
        let v2_pools = [
            V2PoolDef { address: "0xcDAC0d6c6C59727a65F871236188350531885C43", token_a: &weth, token_b: &usdc, fee_bps: 30, dex: DexProtocol::UniswapV2, label: "Aero USDC/WETH (Base)" },
            V2PoolDef { address: "0x27Be19afF47d30d3CEDC098E36844a657a8953AE", token_a: &wbtc, token_b: &weth, fee_bps: 30, dex: DexProtocol::UniswapV2, label: "Aero WBTC/WETH (Base)" },
            V2PoolDef { address: "0x67b00B46FA4f4F24c03855c5C8013C0B938B3eEc", token_a: &dai,  token_b: &usdc, fee_bps: 5,  dex: DexProtocol::UniswapV2, label: "Aero DAI/USDC (Base)"  },
            V2PoolDef { address: "0xFFD4Ec4BD2211cBFD58C209FdEcC65F63f2b9e4c", token_a: &weth, token_b: &usdt, fee_bps: 30, dex: DexProtocol::UniswapV2, label: "Aero USDT/WETH (Base)" },
        ];

        info!("  ⏳ Fetching live V2 pool states ({} pools)...", v2_pools.len());

        for def in &v2_pools {
            match evm.get_v2_pool_state(def.address).await {
                Ok((reserve0, reserve1)) => {
                    let (reserve_a, reserve_b) = if def.token_a.address.to_lowercase() < def.token_b.address.to_lowercase() {
                        (reserve0, reserve1)
                    } else {
                        (reserve1, reserve0)
                    };
                    g.upsert_pool(Pool {
                        id: def.address.to_lowercase(),
                        chain: active_chain,
                        dex: def.dex.clone(),
                        token_a: def.token_a.clone(),
                        token_b: def.token_b.clone(),
                        pool_type: PoolType::ConstantProduct,
                        fee_bps: def.fee_bps,
                        last_updated_block: 0,
                        last_updated_ts: chrono::Utc::now().timestamp(),
                        state: PoolState {
                            reserve_a,
                            reserve_b,
                            sqrt_price_x96: None,
                            tick:       None,
                            liquidity:  None,
                            amp_coeff:  None,
                        },
                    });
                    synced += 1;
                    info!(
                        pool     = %def.label,
                        reserve0 = %reserve0,
                        reserve1 = %reserve1,
                        "  ✓ {} synced (V2)",
                        def.label
                    );
                }
                Err(e) => {
                    failed += 1;
                    warn!(
                        pool  = %def.label,
                        error = %e,
                        "  ⚠ {} V2 fetch failed — using simulated reserves",
                        def.label
                    );
                    g.upsert_pool(Pool {
                        id: def.address.to_lowercase(),
                        chain: active_chain,
                        dex: def.dex.clone(),
                        token_a: def.token_a.clone(),
                        token_b: def.token_b.clone(),
                        pool_type: PoolType::ConstantProduct,
                        fee_bps: def.fee_bps,
                        last_updated_block: 0,
                        last_updated_ts: 0,
                        state: PoolState {
                            reserve_a: if def.token_a.symbol == "WETH" {
                                U256::from(1_000_000_000_000_000_000_000u128) // ~1000 WETH (18 dec)
                            } else if def.token_a.symbol == "WBTC" {
                                U256::from(10_000_000_000u128) // ~100 WBTC (8 dec)
                            } else if def.token_a.symbol == "DAI" {
                                U256::from(3_000_000_000_000_000_000_000_000u128) // ~3M DAI (18 dec)
                            } else {
                                U256::from(3_000_000_000_000u128) // ~3M stables (USDC/USDT, 6 dec)
                            },
                            reserve_b: if def.token_b.symbol == "WETH" {
                                U256::from(1_000_000_000_000_000_000_000u128) // ~1000 WETH (18 dec)
                            } else {
                                U256::from(3_000_000_000_000u128) // ~3M stables (6 dec)
                            },
                            sqrt_price_x96: None,
                            tick:       None,
                            liquidity:  None,
                            amp_coeff:  None,
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

    // ── Fetch Aave Flash Loan fee dynamically ────────────────────────────────
    let mut aave_fee_bps = 5; // Default fallback
    if let Some(ref evm) = evm_adapter {
        match evm.get_aave_premium().await {
            Ok(premium) => {
                info!("✓ Dynamically fetched Aave flash loan fee: {} bps", premium);
                aave_fee_bps = premium;
            }
            Err(e) => {
                warn!("⚠ Failed to fetch Aave flash loan fee: {} — falling back to {} bps", e, aave_fee_bps);
            }
        }
    }

    // ── Build router config ───────────────────────────────────────────────────
    let router_config = RouterConfig {
        max_hops:         config.max_hops,
        min_profit_usd:   1.0, // $1.0 baseline
        reference_amount: crate::pool::U256::from(1_000_000_000_000_000_000u128), // 1 ETH
        eth_price_usd:    config.eth_price_usd,
        gas_price_gwei:   config.gas_price_gwei,
        gas_estimate:     350_000,
        max_price_impact_bps: 200,
        verbose:          false,
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
