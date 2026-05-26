#![allow(dead_code)]

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
mod cex_dex;
mod chains;
mod config;
mod cross_chain;
mod db;
mod executor;
mod liquidations;
mod mempool;
mod metrics;
mod pool;
mod discovery;
mod scoring;

use arb::router::{LiquidityGraph, RouterConfig};
use chains::evm::{EvmAdapter, EvmConfig};
use config::Config;
use db::postgres::PostgresStore;
use db::redis::RedisCache;
use mempool::listener::MempoolListener;
use metrics::EngineMetrics;
use pool::ChainId;
use discovery::mega_scanner::MegaScanner;
use scoring::mega_scorer::{MegaScorer, WhaleScores};
use std::collections::HashMap;

#[tokio::main]
async fn main() {
    // ── Load configuration ────────────────────────────────────────────────────
    dotenvy::dotenv().ok();
    let config = match Config::from_env() {
        Ok(c) => c,
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
            let base_http = config
                .base_http_url
                .clone()
                .unwrap_or_else(|| base_ws.replace("wss://", "https://"));
            (ChainId::Base, base_ws.clone(), base_http)
        } else {
            warn!("  ⚠ BASE_WS_URL not set — falling back to Ethereum mainnet");
            (
                ChainId::Ethereum,
                config.eth_ws_url.clone(),
                config.eth_http_url.clone(),
            )
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
                info!(
                    "✓ Flashbots Submitter initialized at {}",
                    config.flashbots_url
                );
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
            chain: active_chain,
            ws_url: active_ws_url.clone(),
            http_url: active_http_url.clone(),
            flashbots_url: Some(config.flashbots_url.clone()),
            private_key: config.private_key.clone(),
            contract_address: config.contract_address.clone(),
            flashbots_signing_key: config.flashbots_signing_key.clone(),
            private_rpc_url: config.private_rpc_url.clone(),
        };
        let adapter = EvmAdapter::new(evm_config, flashbots_submitter.clone());
        let ws_preview = if active_ws_url.len() > 40 {
            &active_ws_url[..40]
        } else {
            &active_ws_url
        };
        info!(
            "✓ EVM adapter initialized (chain={}, ws={}...)",
            active_chain.name(),
            ws_preview
        );
        Some(Arc::new(adapter))
    };

    // ── Build LiquidityGraph ──────────────────────────────────────────────────
    let graph = Arc::new(RwLock::new(LiquidityGraph::new()));
    info!("✓ Liquidity graph initialized (empty — will populate from cache/chain)");

    // ── Mega Token Universe (Phase 1-4 dynamic token discovery) ──────────────
    let whale_scores: WhaleScores = Arc::new(RwLock::new(HashMap::new()));
    let (mega_scanner, pool_registry, binance_listed) = MegaScanner::new();
    let (mega_scorer, phase_lists) = MegaScorer::new(
        pool_registry.clone(),
        binance_listed.clone(),
        whale_scores.clone(),
    );

    // Spawn MegaScanner (fetches from DeFiLlama, GeckoTerminal, subgraphs)
    tokio::spawn(async move {
        if let Err(e) = mega_scanner.run().await {
            tracing::error!("MegaScanner crashed: {}", e);
        }
    });

    // Spawn MegaScorer (re-ranks all tokens every 30s)
    tokio::spawn(async move {
        mega_scorer.run().await;
    });

    // Spawn phase list logger (every 60s)
    {
        let pl = phase_lists.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                t.tick().await;
                let lists = pl.read().await;
                tracing::info!(
                    "\u{1F4CA} Token Universe | pools:{} tokens:{} | P1:{} P2:{} P3:{} P4:{}",
                    lists.total_pools_scanned, lists.total_tokens_scored,
                    lists.phase1.len(), lists.phase2.len(),
                    lists.phase3.len(), lists.phase4.len(),
                );
            }
        });
    }
    info!("\u{2705} Mega Token Universe scanner started (10+ data sources, 30s rescore)");

    // ── Pool warm-up from PostgreSQL ──────────────────────────────────────────
    if let Some(ref pg) = pg_store {
        match warm_up_and_sync_pools_from_postgres(
            pg,
            &graph,
            &metrics,
            active_chain,
            evm_adapter.as_deref().unwrap(),
        )
        .await
        {
            Ok(count) => {
                if count > 0 {
                    info!("✓ Warmed up and synced {} pools from PostgreSQL", count);
                } else {
                    info!(
                        "  (no pools in registry for {:?} — run `cargo run --bin seed-base-pools`)",
                        active_chain.name()
                    );
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
            ChainId::Base => Some("0xA238Dd80C259a72e81d7e4664a9801593F98d1c5"),
            ChainId::Arbitrum => Some("0x794a61358D6845594F94dc1DB02A252b5b4814aD"),
            ChainId::Ethereum => Some("0x87870B27f0bf4296857d44E8a96a1B714F24F5C9"),
            _ => None,
        };
        if let Some(pool_addr) = config.aave_pool_address.as_deref().or(aave_pool_for_chain) {
            match evm.get_aave_premium_direct(pool_addr).await {
                Ok(p) => {
                    info!("✓ Aave flash loan fee: {} bps", p);
                    aave_fee_bps = p;
                }
                Err(e) => warn!(
                    "⚠ Aave fee fetch failed: {} — using {} bps default",
                    e, aave_fee_bps
                ),
            }
        }
    }

    // ── Build router config ───────────────────────────────────────────────────
    let router_config = RouterConfig {
        gas_price_gwei: config.gas_price_gwei,
        gas_estimate: 350_000,
        eth_price_usd: config.eth_price_usd,
        btc_price_usd: config.btc_price_usd,
        min_profit_usd: config.min_profit_usd,
        reference_amount: crate::pool::U256::from(1_000_000_000_000_000_000u128),
        max_price_impact_bps: config.max_price_impact_bps,
        max_hops: config.max_hops,
        verbose: false,
        aave_fee_bps,
        backrun_enabled: config.backrun_enabled,
    };
    // ── Start mempool listener ────────────────────────────────────────────────
    info!("═══════════════════════════════════════════════════════════════");
    info!("  🚀 Starting mempool listener...");
    info!("═══════════════════════════════════════════════════════════════");

    // Use the active chain WS URL for mempool streaming (Base if configured)
    let execute_enabled = config.execute_enabled;
    if execute_enabled {
        warn!("🔥 LIVE EXECUTION MODE: Engine will broadcast transactions on-chain!");
    } else {
        info!("🔍 MONITORING MODE: Engine will detect & simulate but NOT execute trades.");
        info!("   Set EXECUTE_ENABLED=true in .env to enable live execution.");
    }

    let listener = MempoolListener::new(
        active_ws_url.clone(),
        config.solana_ws_url.clone(),
        redis_cache.clone(),
        graph.clone(),
        router_config,
        pg_store.clone(),
        evm_adapter.clone(),
        metrics.clone(),
        execute_enabled,
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
    let api_graph = graph.clone();
    tokio::spawn(async move {
        api::start_api_server(metrics.clone(), api_graph, 3000).await;
    });

    // Start Automated Pool Discovery (DeFiLlama, Subgraph, Factory Watcher)
    crate::pool::discovery::start_discovery_services(
        graph.clone(),
        pg_store.clone(),
        evm_adapter.clone().unwrap(),
    );

    // NOTE: listener is spawned AFTER Phase 3a so BackrunDetector can be attached first.
    // See the spawn call below the Phase 3a block.
    let mut listener = listener;  // make mutable for with_backrun_detector() builder

    // ── Phase 2: CEX-DEX Statistical Arbitrage ────────────────────────────────
    if config.cex_dex_enabled {
        use cex_dex::binance_feed::BinancePriceFeeder;
        use cex_dex::spread_engine::SpreadEngine;

        // Get Binance-listed symbols from the live phase 2 list
        let binance_syms: Vec<String> = {
            let lists = phase_lists.read().await;
            lists.phase2.iter()
                .map(|t| format!("{}USDT", t.symbol.to_uppercase()))
                .take(400) // Binance WS supports up to 400 streams
                .collect()
        };
        info!("📡 CEX-DEX: subscribing to {} Binance symbols", binance_syms.len());

        let (feeder, cex_feed) = BinancePriceFeeder::new(binance_syms, 5_000);
        let spread_engine = SpreadEngine::new(
            cex_feed,
            phase_lists.clone(),
            config.cex_dex_min_spread_pct,
            config.cex_dex_loan_size_usd,
        );

        tokio::spawn(async move {
            if let Err(e) = feeder.run().await {
                error!("BinanceFeeder crashed: {}", e);
            }
        });

        let exec = config.execute_enabled;
        tokio::spawn(async move {
            if let Err(e) = spread_engine.run(exec).await {
                error!("SpreadEngine crashed: {}", e);
            }
        });
        info!("✅ CEX-DEX engine started");
    } else {
        info!("  Phase 2: CEX-DEX engine disabled (CEX_DEX_ENABLED=false)");
    }

    // ── Phase 3a: Backrunning ─────────────────────────────────────────────────
    if config.backrun_enabled {
        use liquidations::backrun::BackrunDetector;
        
        let detector = Arc::new(BackrunDetector::new(
            phase_lists.clone(),
            whale_scores.clone(),
        ));

        // Attach detector to outer listener (assignment to outer mut binding)
        listener = listener.with_backrun_detector(detector);

        info!(
            "\u{2713} Phase 3a: Backrunning enabled | min_impact={:.0}bps | min_profit=${:.0}",
            config.backrun_min_impact_bps, config.backrun_min_profit_usd
        );
        if config.bloxroute_api_key.is_some() {
            info!("  \u{2713} Bloxroute private mempool feed configured");
        } else {
            warn!("  \u{26a0} BLOXROUTE_API_KEY not set -- using public mempool only (slower backrun detection)");
        }
    } else {
        info!("  Phase 3a: Backrunning disabled (BACKRUN_ENABLED=false)");
    }

    // ── Spawn listener (after all setup so detectors are attached) ───────────
    let listener_handle = tokio::spawn(async move {
        if let Err(e) = listener.run().await {
            error!("Mempool listener fatal error: {}", e);
            std::process::exit(1);
        }
    });
    // ── Phase 3b: Liquidation Monitor ────────────────────────────────────────
    if config.liquidations_enabled {
        use liquidations::liquidation_monitor::LiquidationMonitor;

        let monitor = LiquidationMonitor::new(
            config.liquidation_min_profit_usd,
            whale_scores.clone(),
        );

        tokio::spawn(async move {
            monitor.run().await;
        });
        info!("✅ Liquidation monitor started (Aave V3 + Moonwell on Base)");
    }

    // ── Phase 4: Cross-Chain Arbitrage (Base <-> Optimism <-> Arbitrum) ───────
    if config.cross_chain_enabled {
        use cross_chain::cross_chain_engine::CrossChainEngine;

        let xc_engine = CrossChainEngine::new(
            phase_lists.clone(),
            config.cross_chain_trade_size_usd,
            config.op_http_url.clone(),
            config.arb_http_url.clone(),
        );

        let exec = config.execute_enabled;
        tokio::spawn(async move {
            if let Err(e) = xc_engine.run(exec).await {
                error!("CrossChainEngine crashed: {}", e);
            }
        });
        info!("✅ Cross-chain engine started (Base↔OP↔ARB)");
    }

    // Block until the mempool listener exits (never under normal operation)
    let _ = listener_handle.await;
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

    if active_chain == crate::pool::ChainId::Base {
        let mut existing_ids: std::collections::HashSet<String> =
            pools.iter().map(|p| p.id.clone()).collect();
        for hp in get_hardcoded_pools() {
            if !existing_ids.contains(&hp.id) {
                pools.push(hp.clone());
                existing_ids.insert(hp.id);
            }
        }
    }

    if pools.is_empty() {
        return Ok(0);
    }

    info!(
        "  ⏳ Fetching live pool states for {} pools via Multicall3...",
        pools.len()
    );

    let mut synced = 0usize;
    let mut failed = 0usize;
    let mut g = graph.write().await;

    // Process in smaller chunks of 5 to avoid RPC timeouts and Alchemy 429 rate limits
    for chunk in pools.chunks(5) {
        match evm.fetch_pool_states_multicall(chunk).await {
            Ok(states) => {
                for (i, state) in states.into_iter().enumerate() {
                    let mut p = chunk[i].clone();
                    let t0 = p.token_a.address.to_lowercase();
                    let t1 = p.token_b.address.to_lowercase();

                    let weth = "0x4200000000000000000000000000000000000006".to_string();
                    let usdc = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913".to_string();
                    let cbbtc = "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf".to_string();

                    let mut is_dust = false;
                    let usdbc = "0xd9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca".to_string();
                    let aero = "0x940181a94a35a4569e4529a3cdfb74e38fd98631".to_string();

                    // Filter out pools with < $500 liquidity to allow structural micro-pools
                    if t0 == weth
                        && state.reserve_a
                            < primitive_types::U256::from(150_000_000_000_000_000u128)
                    {
                        is_dust = true;
                    }
                    if t1 == weth
                        && state.reserve_b
                            < primitive_types::U256::from(150_000_000_000_000_000u128)
                    {
                        is_dust = true;
                    }

                    if t0 == usdc && state.reserve_a < primitive_types::U256::from(500_000_000u128)
                    {
                        is_dust = true;
                    }
                    if t1 == usdc && state.reserve_b < primitive_types::U256::from(500_000_000u128)
                    {
                        is_dust = true;
                    }

                    if t0 == usdbc && state.reserve_a < primitive_types::U256::from(500_000_000u128)
                    {
                        is_dust = true;
                    }
                    if t1 == usdbc && state.reserve_b < primitive_types::U256::from(500_000_000u128)
                    {
                        is_dust = true;
                    }

                    if t0 == cbbtc && state.reserve_a < primitive_types::U256::from(500_000u128) {
                        is_dust = true;
                    }
                    if t1 == cbbtc && state.reserve_b < primitive_types::U256::from(500_000u128) {
                        is_dust = true;
                    }

                    if t0 == aero
                        && state.reserve_a
                            < primitive_types::U256::from(400_000_000_000_000_000_000u128)
                    {
                        is_dust = true;
                    }
                    if t1 == aero
                        && state.reserve_b
                            < primitive_types::U256::from(400_000_000_000_000_000_000u128)
                    {
                        is_dust = true;
                    }

                    let is_empty = match p.pool_type {
                        crate::pool::PoolType::ConcentratedLiquidity => {
                            state.sqrt_price_x96.is_none()
                                || state.liquidity.map_or(true, |l| l < 1_000_000)
                                || is_dust
                        }
                        _ => {
                            state.reserve_a < primitive_types::U256::from(1000u64)
                                || state.reserve_b < primitive_types::U256::from(1000u64)
                                || is_dust
                        }
                    };

                    if is_empty {
                        failed += 1;
                        if let Err(e) = pg.delete_pool(&p.id).await {
                            tracing::warn!(
                                "Failed to delete dust pool {} from registry: {}",
                                p.id,
                                e
                            );
                        } else {
                            tracing::info!(
                                "🗑️ Permanently deleted zero-liquidity/dust pool {} from registry",
                                p.id
                            );
                        }
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
        // Sleep to prevent blowing through Alchemy's Compute Units / sec limit!
        tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
    }

    metrics.set_graph_pools(g.pool_count() as u64);
    metrics.set_graph_tokens(g.token_count() as u64);

    info!(
        "✓ Synced {} pools successfully ({} empty/failed)",
        synced, failed
    );

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

fn get_hardcoded_pools() -> Vec<crate::pool::Pool> {
    use crate::pool::{ChainId, DexProtocol, Pool, PoolState, PoolType, Token};
    vec![
        Pool {
            id: "0xd0b53D9277642d899DF5C87A3966A349A798F224".to_lowercase(),
            chain: ChainId::Base,
            dex: DexProtocol::Aerodrome,
            token_a: Token {
                address: "0x4200000000000000000000000000000000000006".to_string(),
                symbol: "WETH".to_string(),
                decimals: 18,
            },
            token_b: Token {
                address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_lowercase(),
                symbol: "USDC".to_string(),
                decimals: 6,
            },
            pool_type: PoolType::ConcentratedLiquidity,
            state: PoolState::empty(),
            fee_bps: 1, // tick spacing 1 -> Slipstream low fee? Usually 0.01% -> 1bps
            last_updated_block: 0,
            last_updated_ts: 0,
        },
        Pool {
            id: "0xB4885Bc63399BF5518b994c1d0C153334Ee579D0".to_lowercase(),
            chain: ChainId::Base,
            dex: DexProtocol::AerodromeV2,
            token_a: Token {
                address: "0x4200000000000000000000000000000000000006".to_string(),
                symbol: "WETH".to_string(),
                decimals: 18,
            },
            token_b: Token {
                address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_lowercase(),
                symbol: "USDC".to_string(),
                decimals: 6,
            },
            pool_type: PoolType::ConstantProduct,
            state: PoolState::empty(),
            fee_bps: 30, // standard volatile pool
            last_updated_block: 0,
            last_updated_ts: 0,
        },
        Pool {
            id: "0x70aCf3cb9dB69A67B2F9b5A96F36AdD81aC5a54A".to_lowercase(),
            chain: ChainId::Base,
            dex: DexProtocol::Aerodrome,
            token_a: Token {
                address: "0x4200000000000000000000000000000000000006".to_string(),
                symbol: "WETH".to_string(),
                decimals: 18,
            },
            token_b: Token {
                address: "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf".to_lowercase(),
                symbol: "cbBTC".to_string(),
                decimals: 8,
            },
            pool_type: PoolType::ConcentratedLiquidity,
            state: PoolState::empty(),
            fee_bps: 5,
            last_updated_block: 0,
            last_updated_ts: 0,
        },
        Pool {
            id: "0x4e962BB3889Bf030368F56810A9489ab21e3E778".to_lowercase(),
            chain: ChainId::Base,
            dex: DexProtocol::Aerodrome,
            token_a: Token {
                address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_lowercase(),
                symbol: "USDC".to_string(),
                decimals: 6,
            },
            token_b: Token {
                address: "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf".to_lowercase(),
                symbol: "cbBTC".to_string(),
                decimals: 8,
            },
            pool_type: PoolType::ConcentratedLiquidity,
            state: PoolState::empty(),
            fee_bps: 5,
            last_updated_block: 0,
            last_updated_ts: 0,
        },
        Pool {
            id: "0x7f670f78B17dEC44d5Ef68a48740b6f8849cc2e6".to_lowercase(),
            chain: ChainId::Base,
            dex: DexProtocol::Aerodrome,
            token_a: Token {
                address: "0x4200000000000000000000000000000000000006".to_string(),
                symbol: "WETH".to_string(),
                decimals: 18,
            },
            token_b: Token {
                address: "0x940181a94A35A4569E4529A3CDfB74e38FD98631".to_lowercase(),
                symbol: "AERO".to_string(),
                decimals: 18,
            },
            pool_type: PoolType::ConcentratedLiquidity,
            state: PoolState::empty(),
            fee_bps: 30,
            last_updated_block: 0,
            last_updated_ts: 0,
        },
        Pool {
            id: "0xDE4Fd6c86c40e8Be6d34B00e3F1a17F6C29D1f9A".to_lowercase(),
            chain: ChainId::Base,
            dex: DexProtocol::AerodromeV2,
            token_a: Token {
                address: "0x4200000000000000000000000000000000000006".to_string(),
                symbol: "WETH".to_string(),
                decimals: 18,
            },
            token_b: Token {
                address: "0x7Ba6F01772924a82D9626c126347A28299E9d858".to_lowercase(),
                symbol: "msETH".to_string(),
                decimals: 18,
            },
            pool_type: PoolType::ConstantProduct,
            state: PoolState::empty(),
            fee_bps: 1, // stable pool
            last_updated_block: 0,
            last_updated_ts: 0,
        },
    ]
}
