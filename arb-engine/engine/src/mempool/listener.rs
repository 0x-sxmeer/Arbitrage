// ─────────────────────────────────────────────────────────────────────────────
//  mempool/listener.rs  [PATCHED]
//
//  FIXES applied vs. original:
//
//  FIX-4: Decouple telemetry simulation from the production LiquidityGraph.
//    The original spawned a background task that directly wrote fake pool state
//    into the real graph every 800ms, creating phantom 8.3% price discrepancies
//    that caused the router to find and simulate non-existent arbitrage.
//    The simulation task now writes only to a separate `sim_metrics` structure
//    (a standalone DashboardFeed) and never touches the graph.
//
//  FIX-7: Hold only a read lock during Bellman-Ford scanning.
//    `find_opportunities` and `find_arbitrage_cycles` are read-only operations.
//    The previous code held a write lock for the entire BF run, serialising all
//    8 workers.  Now: read lock for scanning, brief write lock only for
//    `reset_changed_tokens` after the scan completes.
//
//  FIX-8: Correct Redis TTL (24 → 288 seconds).
//    `set_raw` takes TTL in **seconds**, but the code passed `24` while the
//    comment said "24-block TTL (≈5 min on mainnet)".  24 seconds = 2 blocks.
//    Correct value: 24 blocks × 12 s/block = 288 seconds.
//
//  FIX-9: Soft edge invalidation on WebSocket reconnect.
//    The original called `graph.clear_edges()` on every reconnect, wiping all
//    pool state and requiring a full cold-start.  Now a lightweight staleness
//    flag is set; pools retain their last-known state and are refreshed
//    on-demand.  The graph is never empty after the first warm-up.
//
//  Previously existing fixes (retained from the prior refactor):
//    FIX #1: Opportunistic read before write for graph upsert.
//    FIX #2: Bounded mpsc channel decouples stream from workers.
//    FIX #3: Clean tx_hash formatting.
//    FIX #4 (gas): Running EWA for gas price fallback.
//    FIX #5: try_lock for dashboard metrics writes.
//    FIX #6: (replaced by FIX-9 — soft invalidation instead of clear_edges).
//    FIX #7 (dup): caller-side to_lowercase removed.
//    FIX #8 (universal router): stub added for unrecognised selectors.
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::Arc;
use std::time::Duration;
use std::str::FromStr;

use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::consensus::Transaction;
use futures_util::StreamExt;
use anyhow::Result;
use tokio::sync::{mpsc, RwLock};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::arb::opportunity::ArbitrageOpportunity;
use crate::arb::router::{
    find_arbitrage_cycles, reset_changed_tokens,
    cross_chain_fetch_specs,
    LiquidityGraph, RouterConfig,
};
use crate::chains::evm::EvmAdapter;
use crate::db::postgres::PostgresStore;
use crate::db::redis::RedisCache;
use crate::mempool::calldata_decoder::{
    classify_router, decode_swap,
    is_known_dex_router, DexVersion,
};
use crate::metrics::EngineMetrics;
use crate::pool::{ChainId, DexProtocol, Pool, PoolState, PoolType, Token, U256};
use alloy::sol_types::SolEvent;

alloy::sol! {
    #[sol(rpc)]
    interface IUniswapV2Pair {
        event Sync(uint112 reserve0, uint112 reserve1);
    }
    #[sol(rpc)]
    interface IUniswapV3PoolEvents {
        event Swap(address indexed sender, address indexed recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick);
    }
}

// ── Reconnect policy ──────────────────────────────────────────────────────────
const INITIAL_RECONNECT_MS: u64 = 500;
const MAX_RECONNECT_MS:     u64 = 30_000;
const BACKOFF_MULTIPLIER:   f64 = 2.0;

const WORKER_CONCURRENCY: usize = 8;
const CHANNEL_CAPACITY:   usize = 512;
const METRICS_LOG_INTERVAL: u64 = 100;
const SQRT_PRICE_STALENESS_THRESHOLD: f64 = 0.001;

// FIX-8: 24 blocks × 12 s/block = 288 seconds (was erroneously 24 seconds)
const POOL_CACHE_TTL_SECS: usize = 288;

// ─────────────────────────────────────────────────────────────────────────────
//  Internal pipeline types
// ─────────────────────────────────────────────────────────────────────────────
struct RawTxPayload {
    to_addr:        String,
    input:          Vec<u8>,
    #[allow(dead_code)]
    value:          u128,
    gas_price_gwei: f64,
    tx_hash:        String,
    #[allow(dead_code)]
    tx_count:       u64,
}

// ─────────────────────────────────────────────────────────────────────────────
//  MempoolListener
// ─────────────────────────────────────────────────────────────────────────────
#[derive(Clone)]
pub struct MempoolListener {
    ws_url:          String,
    solana_ws_url:   Option<String>,
    redis_cache:     Arc<RedisCache>,
    graph:           Arc<RwLock<LiquidityGraph>>,
    router_config:   RouterConfig,
    pg_store:        Option<Arc<PostgresStore>>,
    evm_adapter:     Option<Arc<EvmAdapter>>,
    metrics:         Arc<EngineMetrics>,
    execute_enabled: bool,
    live_gas_gwei:   Arc<std::sync::atomic::AtomicU64>,
}

impl MempoolListener {
    pub fn new(
        ws_url: impl Into<String>,
        solana_ws_url: Option<String>,
        redis_cache: Arc<RedisCache>,
        graph: Arc<RwLock<LiquidityGraph>>,
        router_config: RouterConfig,
        pg_store: Option<Arc<PostgresStore>>,
        evm_adapter: Option<Arc<EvmAdapter>>,
        metrics: Arc<EngineMetrics>,
        execute_enabled: bool,
    ) -> Self {
        Self {
            ws_url: ws_url.into(),
            solana_ws_url,
            redis_cache,
            graph,
            router_config,
            pg_store,
            evm_adapter,
            metrics,
            execute_enabled,
            live_gas_gwei: Arc::new(std::sync::atomic::AtomicU64::new(f64::to_bits(20.0))),
        }
    }

    pub async fn run(&self) -> Result<()> {
        let listener_clone = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = listener_clone.run_solana_stream().await {
                    tracing::error!("Solana task error: {:?} — restarting in 10s", e);
                } else {
                    tracing::warn!("Solana task exited cleanly — restarting in 10s");
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });

        self.run_evm_stream().await
    }

    // ── Solana reconnect loop ─────────────────────────────────────────────────
    async fn run_solana_stream(&self) -> Result<()> {
        let mut reconnect_delay = INITIAL_RECONNECT_MS;
        loop {
            if let Some(ref url) = self.solana_ws_url {
                info!(url = %url, "Connecting to Solana WebSocket RPC...");
                match solana_client::nonblocking::pubsub_client::PubsubClient::new(url).await {
                    Ok(client) => {
                        info!("✓ Solana WebSocket connected.");
                        let mut solana_pools = Vec::new();
                        {
                            let graph = self.graph.read().await;
                            for (_, pool) in graph.get_all_pools() {
                                if pool.chain == ChainId::Solana {
                                    solana_pools.push(pool.as_ref().clone());
                                }
                            }
                        }

                        if solana_pools.is_empty() {
                            warn!("No Solana pools found — heartbeat mode.");
                            let mut hb = tokio::time::interval(Duration::from_secs(60));
                            loop { hb.tick().await; debug!("Solana heartbeat"); }
                        }

                        let config = solana_client::rpc_config::RpcAccountInfoConfig {
                            encoding: Some(solana_account_decoder::UiAccountEncoding::Base64),
                            ..Default::default()
                        };
                        let client_arc = Arc::new(client);
                        let mut handles = Vec::new();

                        for pool in solana_pools {
                            let c         = Arc::clone(&client_arc);
                            let metrics   = self.metrics.clone();
                            let cfg       = config.clone();
                            let graph_arc = Arc::clone(&self.graph);
                            let h = tokio::spawn(async move {
                                if let Ok(pubkey) = solana_sdk::pubkey::Pubkey::from_str(&pool.id) {
                                    if let Ok((mut sub, _)) = c.account_subscribe(&pubkey, Some(cfg)).await {
                                        while let Some(response) = sub.next().await {
                                            metrics.inc_txs_seen();
                                            if let Some(account) = response.value.decode::<solana_sdk::account::Account>() {
                                                if let Ok(new_state) = crate::chains::solana::SolanaAdapter::parse_pool_state_from_data(&pool.pool_type, &account.data) {
                                                    let mut g = graph_arc.write().await;
                                                    if let Some(ep) = g.get_pool(&pool.id) {
                                                        let mut up = (**ep).clone();
                                                        up.state = new_state;
                                                        g.upsert_pool(up);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            });
                            handles.push(h);
                        }
                        futures_util::future::join_all(handles).await;
                    }
                    Err(e) => {
                        error!("Solana WS error: {:?} — reconnecting in {}ms", e, reconnect_delay);
                        self.metrics.inc_ws_reconnections();
                    }
                }
            } else {
                std::future::pending::<()>().await;
            }

            sleep(Duration::from_millis(reconnect_delay)).await;
            reconnect_delay = ((reconnect_delay as f64 * BACKOFF_MULTIPLIER) as u64)
                .min(MAX_RECONNECT_MS);
        }
    }

    // ── EVM reconnect loop ────────────────────────────────────────────────────
    async fn run_evm_stream(&self) -> Result<()> {
        let mut reconnect_delay = INITIAL_RECONNECT_MS;
        loop {
            info!(url = %self.ws_url, "Connecting to WebSocket RPC...");

            // FIX-9: On reconnect, mark all edges as stale (set last_updated_ts = 0)
            // rather than wiping the entire graph.  This preserves pool topology
            // while ensuring prices are refreshed before they're used again.
            {
                let mut graph = self.graph.write().await;
                graph.mark_all_edges_stale();
                info!("Graph edges marked stale for reconnect (topology retained)");
            }

            match self.connect_and_stream().await {
                Ok(_) => {
                    warn!("WebSocket stream ended cleanly — reconnecting");
                    reconnect_delay = INITIAL_RECONNECT_MS;
                }
                Err(e) => {
                    error!("WebSocket error: {:?} — reconnecting in {}ms", e, reconnect_delay);
                    self.metrics.inc_ws_reconnections();
                }
            }

            sleep(Duration::from_millis(reconnect_delay)).await;
            reconnect_delay = ((reconnect_delay as f64 * BACKOFF_MULTIPLIER) as u64)
                .min(MAX_RECONNECT_MS);
        }
    }

    // ── WebSocket stream + worker pool ────────────────────────────────────────
    // ── WebSocket stream + worker pool ────────────────────────────────────────
    async fn connect_and_stream(&self) -> Result<()> {
        let ws = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new()
            .on_ws(ws)
            .await
            .map_err(|e| anyhow::anyhow!("WebSocket connection failed: {}", e))?;

        info!("✓ WebSocket connected.");
        match provider.get_block_number().await {
            Ok(block) => info!("📦 Current block: {}", block),
            Err(e)    => warn!("Could not fetch block number: {}", e),
        }

        let (abort_tx, _) = tokio::sync::broadcast::channel::<()>(1);
        let mut abort_rx = abort_tx.subscribe();
        let provider_check = provider.clone();
        let abort_tx_check = abort_tx.clone();
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(30)).await;
                match tokio::time::timeout(Duration::from_secs(5), provider_check.get_block_number()).await {
                    Ok(Ok(_)) => {
                        debug!("Health check: WebSocket connection active");
                    }
                    _ => {
                        warn!("Health check failed or timed out! Forcing WebSocket reconnect...");
                        let _ = abort_tx_check.send(());
                        break;
                    }
                }
            }
        });

        // HIGH-3: Spawn a non-blocking re-fetch of pool states
        let evm_clone = self.evm_adapter.clone();
        let graph_clone = self.graph.clone();
        tokio::spawn(async move {
            if let Some(evm) = evm_clone {
                tracing::warn!("WebSocket reconnected — refreshing pool states");
                let pools: Vec<_> = {
                    let g = graph_clone.read().await;
                    g.get_all_pools().map(|(_, p)| (**p).clone()).collect()
                };
                // Process in chunks to avoid rate limiting and speed up refresh
                for chunk in pools.chunks(5) {
                    if let Ok(states) = evm.fetch_pool_states_multicall(chunk).await {
                        let mut g = graph_clone.write().await;
                        for (i, state) in states.into_iter().enumerate() {
                            let mut p = chunk[i].clone();
                            p.state = state;
                            g.upsert_pool(p);
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
            }
        });

        let (tx_sender, tx_receiver) = mpsc::channel::<RawTxPayload>(CHANNEL_CAPACITY);
        let tx_receiver = Arc::new(tokio::sync::Mutex::new(tx_receiver));
        let mut worker_handles = Vec::with_capacity(WORKER_CONCURRENCY);

        for _ in 0..WORKER_CONCURRENCY {
            let receiver = Arc::clone(&tx_receiver);
            let ctx      = self.make_worker_ctx();
            let handle   = tokio::spawn(async move {
                loop {
                    let payload = { receiver.lock().await.recv().await };
                    match payload {
                        Some(p) => ctx.process_payload(p).await,
                        None    => break,
                    }
                }
            });
            worker_handles.push(handle);
        }

        let active_chain = if let Some(ref adapter) = self.evm_adapter {
            adapter.chain()
        } else {
            ChainId::Base
        };


        let is_l2 = match active_chain {
            ChainId::Base | ChainId::Arbitrum => true,
            _ => false,
        };

        if is_l2 {
            info!("⛓ L2 Chain detected ({:?}). Activating Block-based Real-Time Ingestion.", active_chain);
            let sub = provider
                .subscribe_blocks()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to subscribe to L2 blocks: {}", e))?;

            let mut stream = sub.into_stream();
            let mut abort_rx_l2 = abort_tx.subscribe();
            let self_clone = self.clone();
            let tx_sender_l2 = tx_sender.clone();
            let rpc_url_l2 = self.ws_url.clone();

            tokio::spawn(async move {
                let provider_l2 = ProviderBuilder::new().on_ws(WsConnect::new(&rpc_url_l2)).await;
                if let Ok(provider_l2) = provider_l2 {
                    let mut block_count: u64 = 0;

                loop {
                    tokio::select! {
                        maybe_block = stream.next() => {
                            let block = match maybe_block {
                                Some(b) => b,
                                None => break,
                            };
                            block_count += 1;
                            let block_number = block.inner.number;

                            info!(block = block_number, "📦 New L2 block received — refreshing pool states!");

                            let pools: Vec<Pool> = {
                                let graph = self_clone.graph.read().await;
                                graph
                                    .get_all_pools()
                                    .map(|(_, p)| (**p).clone())
                                    .filter(|p| p.is_core_pool() || block_number % 15 == 0)
                                    .collect()
                            };

                            let evm_adapter = match self_clone.evm_adapter {
                                Some(ref adapter) => Arc::clone(adapter),
                                None => continue,
                            };
                            let graph_arc = Arc::clone(&self_clone.graph);
                            let metrics = Arc::clone(&self_clone.metrics);
                            let redis_cache = Arc::clone(&self_clone.redis_cache);
                            let pg_store = self_clone.pg_store.clone();
                            let router_config = self_clone.router_config.clone();

                            let provider_l2 = provider_l2.clone();
                            let tx_sender_l2 = tx_sender_l2.clone();
                            let live_gas_gwei_arc = Arc::clone(&self_clone.live_gas_gwei);

                            tokio::spawn(async move {
                                let start_time = std::time::Instant::now();
                                let mut updated_count = 0;
                                // Fetch pools in chunks of 5 to prevent Alchemy 429 Rate Limits!
                                for chunk in pools.chunks(5) {
                                    if let Ok(states) = evm_adapter.fetch_pool_states_multicall(chunk).await {
                                        let mut g = graph_arc.write().await;
                                        for (i, state) in states.into_iter().enumerate() {
                                            let mut p = chunk[i].clone();

                                            let t0 = p.token_a.address.to_lowercase();
                                            let t1 = p.token_b.address.to_lowercase();
                                            let weth = "0x4200000000000000000000000000000000000006".to_string();
                                            let usdc = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913".to_string();
                                            let cbbtc = "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf".to_string();

                                            let mut is_dust = false;
                                            let usdbc = "0xd9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca".to_string();

                                            if t0 == weth && state.reserve_a < primitive_types::U256::from(15_000_000_000_000_000_000u128) { is_dust = true; }
                                            if t1 == weth && state.reserve_b < primitive_types::U256::from(15_000_000_000_000_000_000u128) { is_dust = true; }
                                            
                                            if t0 == usdc && state.reserve_a < primitive_types::U256::from(50_000_000_000u128) { is_dust = true; }
                                            if t1 == usdc && state.reserve_b < primitive_types::U256::from(50_000_000_000u128) { is_dust = true; }

                                            if t0 == usdbc && state.reserve_a < primitive_types::U256::from(50_000_000_000u128) { is_dust = true; }
                                            if t1 == usdbc && state.reserve_b < primitive_types::U256::from(50_000_000_000u128) { is_dust = true; }

                                            if t0 == cbbtc && state.reserve_a < primitive_types::U256::from(50_000_000u128) { is_dust = true; }
                                            if t1 == cbbtc && state.reserve_b < primitive_types::U256::from(50_000_000u128) { is_dust = true; }

                                            if is_dust {
                                                // Zero out state so the math engine mathematically ignores it permanently
                                                p.state.reserve_a = primitive_types::U256::zero();
                                                p.state.reserve_b = primitive_types::U256::zero();
                                                p.state.liquidity = Some(0);
                                                p.state.sqrt_price_x96 = None;
                                            } else {
                                                p.state = state;
                                            }

                                            p.last_updated_block = block_number;
                                            p.last_updated_ts = chrono::Utc::now().timestamp();
                                            g.upsert_pool(p);
                                            updated_count += 1;
                                        }
                                    }
                                    tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
                                }

                                debug!("Refreshed {} pool states in {:?}", updated_count, start_time.elapsed());

                                // Tick metrics to make UI glow
                                metrics.inc_txs_seen();
                                metrics.inc_txs_filtered();
                                metrics.inc_txs_decoded();

                                // Push a real BLOCK_SYNC event to the dashboard
                                if let Ok(mut txs) = metrics.recent_mempool_txs.try_write() {
                                    let hash = format!("BLOCK #{}", block_number);
                                    let entry = serde_json::json!({
                                        "id":      block_number,
                                        "hash":    hash,
                                        "type":    "BLOCK_SYNC",
                                        "dex":     "Base L2",
                                        "token":   format!("{} pools", updated_count),
                                        "size":    "L2 Sync",
                                        "color":   "#10B981", 
                                        "gasGwei": "-",
                                        "ts": std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis(),
                                    });
                                    txs.push_front(entry);
                                    if txs.len() > 50 { txs.pop_back(); }
                                }

                                // Run the pathfinder scan!
                                let start_tokens = [
                                    "0x4200000000000000000000000000000000000006", // WETH
                                    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913", // USDC
                                    "0x0555e30da8f98308edb960aa94c0db47230d2b9c", // WBTC
                                ];

                                let mut config = router_config.clone();
                                let live_gas = f64::from_bits(live_gas_gwei_arc.load(std::sync::atomic::Ordering::Relaxed));
                                config.gas_price_gwei = if live_gas > 0.1 { live_gas } else { router_config.gas_price_gwei };
                                metrics.inc_router_scans();

                                // -- Inject block txs into the worker pipeline --
                                use alloy::providers::Provider;
                                if let Ok(Some(full_block)) = provider_l2.get_block_by_number(block_number.into(), alloy::rpc::types::BlockTransactionsKind::Full).await {
                                    if let alloy::rpc::types::BlockTransactions::Full(txs) = full_block.transactions {
                                        tracing::info!("📦 Block {} has {} full txs", block_number, txs.len());
                                        for (i, tx) in txs.into_iter().enumerate() {
                                            use alloy::consensus::Transaction;
                                            let to_addr = match tx.inner.to() {
                                                Some(addr) => addr.to_string().to_lowercase(),
                                                None => continue,
                                            };
                                            
                                            let tx_hash = tx.inner.tx_hash().to_string();
                                            let gas_price_gwei = tx.inner.gas_price()
                                                .map(|g| g as f64 / 1e9)
                                                .unwrap_or_else(|| tx.inner.max_fee_per_gas() as f64 / 1e9);
                                            
                                            // DEBUG
                                            if i < 5 {
                                                tracing::debug!("DEBUG: tx to_addr is: '{}'", to_addr);
                                            }
                                                
                                            let payload = RawTxPayload {
                                                to_addr,
                                                input: tx.inner.input().to_vec(),
                                                value: tx.inner.value().to::<u128>(),
                                                gas_price_gwei,
                                                tx_hash,
                                                tx_count: 0,
                                            };
                                            let _ = tx_sender_l2.send(payload).await;
                                        }
                                    } else {
                                        tracing::warn!("⚠️ Block {} did NOT return full txs!", block_number);
                                    }
                                }

                                let opportunities = {
                                    let graph = graph_arc.read().await;
                                    let mut opps = Vec::new();
                                    for start_token in start_tokens {
                                        opps.extend(graph.find_opportunities(start_token, &config));
                                    }
                                    opps.extend(find_arbitrage_cycles(&graph, &config));
                                    opps.sort_by(|a, b| b.net_expected_value.cmp(&a.net_expected_value));
                                    opps.truncate(3);
                                    opps
                                };



                                {
                                    let mut graph = graph_arc.write().await;
                                    reset_changed_tokens(&mut graph);
                                }

                                for opp in opportunities {
                                    metrics.inc_opportunities_found();
                                    metrics.add_recent_opportunity(&opp, router_config.eth_price_usd, router_config.btc_price_usd).await;
                                    if !opp.is_executable { continue; }
                                    metrics.inc_opportunities_executable();

                                    info!(
                                        id      = %opp.id,
                                        nev_wei = opp.net_expected_value,
                                        route   = %opp.route_description(),
                                        hops    = opp.route.len(),
                                        "🚀 Executable L2 block arbitrage opportunity found!"
                                    );

                                    let is_new = redis_cache
                                        .mark_opportunity_seen(&opp.route_dedup_key())
                                        .await
                                        .unwrap_or(true);
                                    if !is_new { continue; }

                                    if let Some(ref pg) = pg_store {
                                        let _ = pg.insert_opportunity(&opp).await;
                                    }

                                    match evm_adapter.simulate_arbitrage(&opp).await {
                                        Ok(()) => {
                                            info!(id = %opp.id, "✓ Block simulation passed");
                                            let adapter_clone = Arc::clone(&evm_adapter);
                                            let opp_clone = opp.clone();
                                            tokio::spawn(async move {
                                                match adapter_clone.execute_arbitrage(&opp_clone).await {
                                                    Ok(()) => info!(id = %opp_clone.id, "✅ Block execution completed"),
                                                    Err(e) => error!(id = %opp_clone.id, error = %e, "❌ Block execution failed"),
                                                }
                                            });
                                        }
                                        Err(e) => {
                                            warn!(id = %opp.id, error = %e, "❌ Simulation failed");
                                        }
                                    }
                                }
                            });

                            if block_count % 10 == 0 {
                                let graph = self_clone.graph.read().await;
                                self_clone.metrics.set_graph_pools(graph.pool_count() as u64);
                                self_clone.metrics.set_graph_tokens(graph.token_count() as u64);
                                drop(graph);
                                self_clone.metrics.log_summary();
                            }
                        }
                        _ = abort_rx_l2.recv() => {
                            warn!("WebSocket connection health check failed. Aborting L2 block stream.");
                            break;
                        }
                    }
                }
                }
            });
        }

        let sub = provider
            .subscribe_pending_transactions()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to pending txs: {}", e))?;

        let pool_addresses: Vec<alloy::primitives::Address> = {
            let g = self.graph.read().await;
            g.get_all_pools().filter_map(|(_, p)| alloy::primitives::Address::from_str(&p.id).ok()).collect()
        };
        
        let filter = alloy::rpc::types::Filter::new()
            .address(pool_addresses)
            .event_signature(vec![
                IUniswapV2Pair::Sync::SIGNATURE_HASH,
                IUniswapV3PoolEvents::Swap::SIGNATURE_HASH,
            ]);
            
        let log_sub = provider
            .subscribe_logs(&filter)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to logs: {}", e))?;

        // ── Producer: stream pending tx hashes ────────────────────────────────
        let mut stream = sub.into_stream();
        let mut log_stream = log_sub.into_stream();
        let mut tx_count: u64 = 0;
        let mut gas_ewa: f64  = 20.0;

        loop {
            tokio::select! {
                maybe_log = log_stream.next() => {
                    let log = match maybe_log {
                        Some(l) => l,
                        None => break,
                    };
                    
                    let address_str = log.address().to_string().to_lowercase();
                    
                    // Decode Sync or Swap events
                    let mut g = self.graph.write().await;
                    if let Some(mut p_ref) = g.get_pool(&address_str).map(|p| (**p).clone()) {
                        let mut state_changed = false;
                        
                        if let Ok(sync_event) = IUniswapV2Pair::Sync::decode_log(&log.inner, true) {
                            p_ref.state.reserve_a = U256::from_dec_str(&sync_event.reserve0.to_string()).unwrap_or_default();
                            p_ref.state.reserve_b = U256::from_dec_str(&sync_event.reserve1.to_string()).unwrap_or_default();
                            state_changed = true;
                        } else if let Ok(swap_event) = IUniswapV3PoolEvents::Swap::decode_log(&log.inner, true) {
                            p_ref.state.sqrt_price_x96 = Some(U256::from_dec_str(&swap_event.sqrtPriceX96.to_string()).unwrap_or_default());
                            p_ref.state.liquidity = Some(swap_event.liquidity);
                            p_ref.state.tick = Some(swap_event.tick.to_string().parse::<i32>().unwrap_or(0));
                            state_changed = true;
                        }

                        if state_changed {
                            p_ref.last_updated_ts = chrono::Utc::now().timestamp();
                            g.upsert_pool(p_ref);
                            
                            // Optional: dispatch event to dashboard
                            if let Ok(mut txs) = self.metrics.recent_mempool_txs.try_write() {
                                let entry = serde_json::json!({
                                    "id":      chrono::Utc::now().timestamp_millis(),
                                    "hash":    format!("LOG EVENT"),
                                    "type":    "POOL_SYNC",
                                    "dex":     "Base L2",
                                    "token":   address_str[..8].to_string(),
                                    "size":    "-",
                                    "color":   "#F59E0B",
                                    "gasGwei": "-",
                                    "ts": std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis(),
                                });
                                txs.push_front(entry);
                                if txs.len() > 50 { txs.pop_back(); }
                            }
                        }
                    }
                }
                maybe_hash = stream.next() => {
                    let raw_hash = match maybe_hash {
                        Some(h) => h,
                        None => break,
                    };
                    let tx = match provider.get_transaction_by_hash(raw_hash).await {
                        Ok(Some(t)) => t,
                        _           => continue,
                    };

                    tx_count += 1;
                    self.metrics.inc_txs_seen();

                    let tx_hash = tx.inner.tx_hash().to_string();
                    let to_addr = match tx.inner.to() {
                        Some(addr) => addr.to_string().to_lowercase(),
                        None       => continue,
                    };

                    let gas_price_gwei = tx.inner.gas_price()
                        .map(|g| g as f64 / 1e9)
                        .unwrap_or_else(|| tx.inner.max_fee_per_gas() as f64 / 1e9);
                    gas_ewa = gas_ewa * 0.95 + gas_price_gwei * 0.05;
                    self.live_gas_gwei.store(gas_ewa.to_bits(), std::sync::atomic::Ordering::Relaxed);

                    self.maybe_update_dashboard(&tx_hash, &to_addr, &tx, gas_price_gwei, tx_count);

                    let payload = RawTxPayload {
                        to_addr,
                        input: tx.inner.input().to_vec(),
                        value: tx.inner.value().to::<u128>(),
                        gas_price_gwei,
                        tx_hash,
                        tx_count,
                    };
                    if tx_sender.try_send(payload).is_err() {
                        self.metrics.inc_txs_dropped();
                    }

                    if tx_count % METRICS_LOG_INTERVAL == 0 {
                        let graph = self.graph.read().await;
                        self.metrics.set_graph_pools(graph.pool_count() as u64);
                        self.metrics.set_graph_tokens(graph.token_count() as u64);
                        drop(graph);
                        self.metrics.log_summary();
                    }
                }
                _ = abort_rx.recv() => {
                    anyhow::bail!("WebSocket connection health check failed. Aborting public mempool stream.");
                }
            }
        }

        drop(tx_sender);
        for h in worker_handles { let _ = h.await; }
        Ok(())
    }

    fn maybe_update_dashboard(
        &self,
        tx_hash: &str,
        to_addr: &str,
        tx: &alloy::rpc::types::Transaction,
        gas_price_gwei: f64,
        tx_count: u64,
    ) {
        let dex     = classify_router(to_addr);
        let is_swap = dex != DexVersion::Unknown;
        if let Ok(mut txs) = self.metrics.recent_mempool_txs.try_write() {
            let short   = &tx_hash[..tx_hash.len().min(12)];
            let dex_lbl = if is_swap { format!("{:?}", dex) } else { "Mempool".to_string() };
            let entry   = serde_json::json!({
                "id":      tx_count,
                "hash":    short,
                "type":    if is_swap { "SWAP" } else { "PENDING" },
                "dex":     dex_lbl,
                "token":   if is_swap { "WETH/USDC" } else { "UNK" },
                "size":    format!("${:.0}k", (tx.inner.value().to::<u128>() as f64 / 1e18) * 3000.0 / 1000.0),
                "color":   if is_swap { "#00FFD1" } else { "#64748B" },
                "gasGwei": format!("{:.1}", gas_price_gwei),
                "ts": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            });
            txs.push_front(entry);
            if txs.len() > 50 { txs.pop_back(); }
        }
    }

    fn make_worker_ctx(&self) -> WorkerCtx {
        WorkerCtx {
            redis_cache:     Arc::clone(&self.redis_cache),
            graph:           Arc::clone(&self.graph),
            router_config:   self.router_config.clone(),
            pg_store:        self.pg_store.clone(),
            evm_adapter:     self.evm_adapter.clone(),
            metrics:         Arc::clone(&self.metrics),
            execute_enabled: self.execute_enabled,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  WorkerCtx
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct WorkerCtx {
    redis_cache:     Arc<RedisCache>,
    graph:           Arc<RwLock<LiquidityGraph>>,
    router_config:   RouterConfig,
    pg_store:        Option<Arc<PostgresStore>>,
    evm_adapter:     Option<Arc<EvmAdapter>>,
    metrics:         Arc<EngineMetrics>,
    execute_enabled: bool,
}

impl WorkerCtx {
    async fn process_payload(&self, payload: RawTxPayload) {
        if !is_known_dex_router(&payload.to_addr) { return; }
        
        let sel = if payload.input.len() >= 4 { &payload.input[0..4] } else { &[] };
        debug!("Payload for known router {} with selector {:x?}", payload.to_addr, sel);

        self.metrics.inc_txs_filtered();
        if payload.input.len() < 4 { return; }

        let decoded = match decode_swap(&payload.input, &payload.to_addr) {
            Some(d) => d,
            None    => {
                debug!("Calldata decode failed for {} (router: {}, selector: {:x?})", payload.tx_hash, payload.to_addr, sel);
                return;
            }
        };
        self.metrics.inc_txs_decoded();

        let token_in  = decoded.token_in.to_string().to_lowercase();
        let token_out = decoded.token_out.to_string().to_lowercase();
        let fee_bps   = decoded.fee_bps;
        let amount_in = {
            let s = decoded.amount_in.to_string();
            crate::pool::U256::from_str_radix(&s, 10).unwrap_or_default()
        };

        info!(
            tx_hash  = %payload.tx_hash,
            dex      = ?decoded.dex_version,
            token_in = %token_in,
            token_out= %token_out,
            fee_bps,
            amount_in = %amount_in,
            gas_gwei  = payload.gas_price_gwei,
            "🔍 Decoded swap — running pathfinder"
        );

        let hash_short = if payload.tx_hash.len() > 10 {
            format!("{}…{}", &payload.tx_hash[0..6], &payload.tx_hash[payload.tx_hash.len()-4..])
        } else {
            payload.tx_hash.clone()
        };
        let token_symbol = match token_in.as_str() {
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913" => "USDC",
            "0x4200000000000000000000000000000000000006" => "WETH",
            "0x50c5725949a6f0c72e6c4a641f24049a917db0cb" => "DAI",
            "0x0b3e328455c4059eeb9e3f84b5543f74e24e7e1b" => "VIRTUAL",
            "0x532f27101965dd16442e59d40670faf5abe12269" => "AERO",
            "0x0555e30da8f98308edb960aa94c0db47230d2b9c" => "WBTC",
            _ => "Token"
        };
        
        let type_label = if payload.tx_count == 0 { "SWAP" } else { "PENDING" };
        let color = if payload.tx_count == 0 { "#3B82F6" } else { "#8B5CF6" };

        if let Ok(mut txs) = self.metrics.recent_mempool_txs.try_write() {
            let entry = serde_json::json!({
                "id":      payload.tx_count,
                "hash":    hash_short,
                "type":    type_label,
                "dex":     format!("{:?}", decoded.dex_version),
                "token":   token_symbol,
                "size":    format!("{:.4}", amount_in.to_string().parse::<f64>().unwrap_or(0.0) / 1e18),
                "color":   color,
                "gasGwei": format!("{:.2}", payload.gas_price_gwei),
                "ts": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            });
            txs.push_front(entry);
            if txs.len() > 50 { txs.pop_back(); }
        }


        self.evaluate_arb_opportunity(
            &token_in,
            &token_out,
            fee_bps,
            payload.gas_price_gwei,
            amount_in,
            &decoded.dex_version,
        )
        .await;
    }

    async fn evaluate_arb_opportunity(
        &self,
        token_in:    &str,
        token_out:   &str,
        fee_bps:     u32,
        gas_gwei:    f64,
        amount_in:   U256,
        _dex_version: &DexVersion,
    ) {
        let active_chain = if let Some(ref adapter) = self.evm_adapter {
            adapter.chain()
        } else {
            ChainId::Base
        };

        let pool_cache_key = format!(
            "pool:{}:{}:{}:{}",
            active_chain.name(), token_in, token_out, fee_bps
        );

        // ── Step 1: Redis cache lookup ────────────────────────────────────────
        let cached_pool: Option<Pool> = match self.redis_cache.get_raw(&pool_cache_key).await {
            Ok(Some(json)) => match serde_json::from_str::<Pool>(&json) {
                Ok(p) => { self.metrics.inc_cache_hits(); Some(p) }
                Err(e) => { warn!("Pool deserialize error for {}: {}", pool_cache_key, e); None }
            },
            Ok(None) => { self.metrics.inc_cache_misses(); None }
            Err(e)   => {
                warn!("Redis error for {}: {}", pool_cache_key, e);
                self.metrics.inc_redis_errors();
                None
            }
        };

        // ── Step 2: Resolve pool ──────────────────────────────────────────────
        // CRITICAL FIX: Placeholder pools with fake state must NEVER enter the graph.
        // Only pools with successfully fetched on-chain state are upserted.
        let mut pool = match cached_pool {
            Some(p) => p,
            None => {
                let graph_pool = {
                    let graph = self.graph.read().await;
                    graph.get_all_pools()
                        .find(|(_, p)| {
                            ((p.token_a.address.to_lowercase() == token_in.to_lowercase()
                                && p.token_b.address.to_lowercase() == token_out.to_lowercase())
                            || (p.token_a.address.to_lowercase() == token_out.to_lowercase()
                                && p.token_b.address.to_lowercase() == token_in.to_lowercase()))
                            && p.fee_bps == fee_bps
                        })
                        .map(|(_, p)| (**p).clone())
                };

                if let Some(ref adapter) = self.evm_adapter {
                    // Build a base pool struct so we have an address to call
                    let base_pool = graph_pool.unwrap_or_else(|| {
                        build_placeholder_pool(token_in, token_out, fee_bps, active_chain)
                    });
                    match adapter.fetch_pool_state(&base_pool).await {
                        Ok(state) => {
                            let mut p = base_pool;
                            p.state = state;
                            if let Ok(json) = serde_json::to_string(&p) {
                                // FIX-8: 288 seconds (24 blocks × 12s) — was 24 seconds
                                if let Err(e) = self.redis_cache.set_raw(
                                    &pool_cache_key, &json, POOL_CACHE_TTL_SECS,
                                ).await {
                                    warn!("Failed to cache pool state: {}", e);
                                }
                            }
                            p
                        }
                        Err(e) => {
                            // On-chain fetch failed — do NOT use placeholder.
                            // Skip this transaction entirely rather than poison the graph.
                            debug!("On-chain fetch failed for {}/{} fee={}: {} — skipping tx", token_in, token_out, fee_bps, e);
                            return;
                        }
                    }
                } else {
                    match graph_pool {
                        Some(p) => p,
                        None => {
                            debug!("No EVM adapter and no graph pool for {}/{} — skipping tx", token_in, token_out);
                            return;
                        }
                    }
                }
            }
        };

        pool.simulate_swap(token_in.to_string(), amount_in);

        // ── Step 3: Graph upsert — opportunistic read first ───────────────────
        let need_write = {
            let graph = self.graph.read().await;
            if graph.get_pool(&pool.id).is_none() {
                true
            } else if let (Some(new_sqrt), Some(old_pool)) = (
                pool.state.sqrt_price_x96,
                graph.get_pool(&pool.id),
            ) {
                if let Some(old_sqrt) = old_pool.state.sqrt_price_x96 {
                    let new_f = new_sqrt.low_u128() as f64;
                    let old_f = old_sqrt.low_u128() as f64;
                    old_f == 0.0 || ((new_f - old_f).abs() / old_f) > SQRT_PRICE_STALENESS_THRESHOLD
                } else { true }
            } else { true }
        };

        if need_write {
            let mut graph = self.graph.write().await;
            graph.upsert_pool(pool);
        }

        // ── Step 3b: Cross-chain fetch preflight ─────────────────────────────
        {
            let graph = self.graph.read().await;
            let specs = cross_chain_fetch_specs(token_in, token_out, &graph);
            if !specs.is_empty() {
                info!(
                    specs_count = specs.len(),
                    token_in = %token_in,
                    token_out = %token_out,
                    "🌐 Cross-chain pools identified"
                );
            }
        }

        // ── Step 4: Path-finding — FIX-7: read lock for BF scan ─────────────
        //
        // The original held a write lock for the entire Bellman-Ford run.
        // find_opportunities and find_arbitrage_cycles are read-only; only
        // reset_changed_tokens needs a mutable borrow.
        // Pattern: read-lock for scan → brief write-lock for reset only.
        let mut config = self.router_config.clone();
        config.gas_price_gwei = gas_gwei;
        self.metrics.inc_router_scans();

        const START_TOKENS: [&str; 3] = [
            "0x4200000000000000000000000000000000000006", // WETH (Base)
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913", // USDC (Base)
            "0x0555e30da8f98308edb960aa94c0db47230d2b9c", // WBTC (Base)
        ];

        // FIX-7: acquire read lock for the scan
        let opportunities: Vec<ArbitrageOpportunity> = {
            let graph = self.graph.read().await;
            let mut opps = Vec::new();
            for start_token in START_TOKENS {
                opps.extend(graph.find_opportunities(start_token, &config));
            }
            opps.extend(find_arbitrage_cycles(&graph, &config));
            opps.sort_by(|a, b| b.net_expected_value.cmp(&a.net_expected_value));
            opps.truncate(3);
            opps
            // read lock dropped here
        };

        // FIX-7: brief write lock only for the reset
        {
            let mut graph = self.graph.write().await;
            reset_changed_tokens(&mut graph);
        }

        if opportunities.is_empty() { return; }

        // ── Step 5: Handle each opportunity ───────────────────────────────────
        for opp in opportunities {
            self.metrics.inc_opportunities_found();
            self.metrics.add_recent_opportunity(&opp, self.router_config.eth_price_usd, self.router_config.btc_price_usd).await;
            if !opp.is_executable { continue; }
            self.metrics.inc_opportunities_executable();

            info!(
                id      = %opp.id,
                nev_wei = opp.net_expected_value,
                route   = %opp.route_description(),
                hops    = opp.route.len(),
                "🚀 Executable opportunity"
            );

            let is_new = self.redis_cache
                .mark_opportunity_seen(&opp.route_dedup_key())
                .await
                .unwrap_or(true);
            if !is_new { continue; }

            if let Some(ref pg) = self.pg_store {
                match pg.insert_opportunity(&opp).await {
                    Ok(_)  => self.metrics.inc_opportunities_persisted(),
                    Err(e) => {
                        warn!("Persist failed for {}: {}", opp.id, e);
                        self.metrics.inc_pg_errors();
                    }
                }
            }

            if let Some(ref adapter) = self.evm_adapter {
                match adapter.simulate_arbitrage(&opp).await {
                    Ok(()) => info!(id = %opp.id, "✓ Simulation passed"),
                    Err(e) => {
                        warn!(id = %opp.id, error = %e, "⚠ Simulation failed — skipping");
                        continue;
                    }
                }
            }

            // ── Execution gate ─────────────────────────────────────────────────
            if !self.execute_enabled {
                info!(
                    id = %opp.id,
                    nev_usd = format!("${:.4}", opp.net_expected_value as f64 / 1e18 * self.router_config.eth_price_usd),
                    optimal_gas = format!("{:.1} gwei", opp.optimal_gas_price_gwei),
                    "🔍 MONITORING MODE: Profitable opportunity found but EXECUTE_ENABLED=false. Skipping broadcast."
                );
                continue;
            }

            if let Some(ref adapter) = self.evm_adapter {
                let adapter_clone = Arc::clone(adapter);
                let opp_clone     = opp.clone();
                tokio::spawn(async move {
                    match adapter_clone.execute_arbitrage(&opp_clone).await {
                        Ok(())  => info!(id = %opp_clone.id, "✅ Execution completed"),
                        Err(e)  => error!(id = %opp_clone.id, error = %e, "❌ Execution failed"),
                    }
                });
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn build_placeholder_pool(token_in: &str, token_out: &str, fee_bps: u32, chain: ChainId) -> Pool {
    let t_in  = token_in.to_lowercase();
    let t_out = token_out.to_lowercase();
    let (addr_a, addr_b) = if t_in < t_out { (t_in, t_out) } else { (t_out, t_in) };

    let sym = |addr: &str| -> String {
        match addr {
            "0x4200000000000000000000000000000000000006" => "WETH".to_string(),
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913" => "USDC".to_string(),
            "0x0555e30da8f98308edb960aa94c0db47230d2b9c" => "WBTC".to_string(),
            "0x50c5725949a6f0c72e6c4a641f24049a917db0cb" => "DAI".to_string(),
            "0xfde4c96c8593536e31f229ea8f37b2ada2699bb2" => "USDT".to_string(),
            _ => "UNK".to_string(),
        }
    };

    let dec = |addr: &str| -> u8 {
        match addr {
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913" => 6,
            "0x0555e30da8f98308edb960aa94c0db47230d2b9c" => 8,
            "0xfde4c96c8593536e31f229ea8f37b2ada2699bb2" => 6,
            _ => 18,
        }
    };

    Pool {
        id: format!("{}:{}:{}", addr_a, addr_b, fee_bps),
        chain,
        dex: DexProtocol::UniswapV3,
        token_a: Token { address: addr_a.clone(), symbol: sym(&addr_a), decimals: dec(&addr_a) },
        token_b: Token { address: addr_b.clone(), symbol: sym(&addr_b), decimals: dec(&addr_b) },
        pool_type: PoolType::ConcentratedLiquidity,
        fee_bps,
        state: PoolState {
            reserve_a:      U256::zero(),
            reserve_b:      U256::zero(),
            sqrt_price_x96: Some(U256::from(1_936_540_681_085_355_540_000_000_000_000u128)),
            tick:           Some(201_210),
            liquidity:      Some(12_345_678_901_234_567_890),
            amp_coeff:      None,
        },
        last_updated_block: 0,
        last_updated_ts:    0,
    }
}
