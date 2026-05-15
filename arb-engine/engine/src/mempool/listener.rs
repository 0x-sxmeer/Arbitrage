// ─────────────────────────────────────────────────────────────────────────────
//  mempool/listener.rs — Real-Time WebSocket Mempool Monitor
//
//  Connects to an Ethereum WebSocket RPC, subscribes to pending transactions,
//  filters for Uniswap V3 Router calls, decodes calldata, and triggers the
//  NEV calculator for affected pools.
//
//  Full pipeline on each detected swap:
//    1. Decode calldata (calldata_decoder)
//    2. Load pool state from Redis cache (or fetch from chain via EvmAdapter)
//    3. Upsert pool into the shared LiquidityGraph
//    4. Run Bellman-Ford (find_arbitrage_cycles)
//    5. For each executable opportunity:
//         a. Mark as seen in Redis (deduplication)
//         b. Persist to PostgreSQL if available
//         c. Submit Flashbots bundle (simulated in Phase 1)
//
//  Uses alloy-rs for real pending transaction subscriptions.
//  Auto-reconnects with exponential backoff on any WebSocket failure.
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::Arc;
use std::time::Duration;

use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::rpc::types::Transaction;
use futures_util::StreamExt;
use anyhow::Result;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::arb::opportunity::ArbitrageOpportunity;
use crate::arb::router::{find_arbitrage_cycles, LiquidityGraph, RouterConfig};
use crate::chains::evm::EvmAdapter;
use crate::db::postgres::PostgresStore;
use crate::db::redis::RedisCache;
use crate::mempool::calldata_decoder::{self, DecodedCall, WATCHED_ROUTERS};
use crate::metrics::EngineMetrics;
use crate::pool::{fee_tier_to_bps, ChainId, DexProtocol, Pool, PoolState, PoolType, Token, U256};

// ── Reconnect policy ──────────────────────────────────────────────────────────
const INITIAL_RECONNECT_MS: u64 = 500;
const MAX_RECONNECT_MS:     u64 = 30_000;
const BACKOFF_MULTIPLIER:   f64 = 2.0;

/// How often to log metrics summary (every N transactions)
const METRICS_LOG_INTERVAL: u64 = 100;

// ─────────────────────────────────────────────────────────────────────────────
//  MempoolListener
// ─────────────────────────────────────────────────────────────────────────────

pub struct MempoolListener {
    ws_url:        String,
    redis_cache:   Arc<RedisCache>,
    /// Shared liquidity graph — updated on every detected swap
    graph:         Arc<RwLock<LiquidityGraph>>,
    router_config: RouterConfig,
    /// Optional Postgres store for persisting opportunities
    pg_store:      Option<Arc<PostgresStore>>,
    /// Optional EVM adapter for on-chain pool state fetching
    evm_adapter:   Option<Arc<EvmAdapter>>,
    /// Engine-wide metrics
    metrics:       Arc<EngineMetrics>,
}

impl MempoolListener {
    pub fn new(
        ws_url: impl Into<String>,
        redis_cache: Arc<RedisCache>,
        graph: Arc<RwLock<LiquidityGraph>>,
        router_config: RouterConfig,
        pg_store: Option<Arc<PostgresStore>>,
        evm_adapter: Option<Arc<EvmAdapter>>,
        metrics: Arc<EngineMetrics>,
    ) -> Self {
        Self {
            ws_url: ws_url.into(),
            redis_cache,
            graph,
            router_config,
            pg_store,
            evm_adapter,
            metrics,
        }
    }

    /// Run forever — subscribes to pending txs, reconnects on failure.
    pub async fn run(&self) -> Result<()> {
        let mut reconnect_delay = INITIAL_RECONNECT_MS;

        loop {
            info!(url = %self.ws_url, "Connecting to WebSocket RPC...");

            match self.connect_and_stream().await {
                Ok(_) => {
                    warn!("WebSocket stream ended cleanly — reconnecting");
                    reconnect_delay = INITIAL_RECONNECT_MS; // reset on clean disconnect
                }
                Err(e) => {
                    error!(
                        "WebSocket error: {:?} — reconnecting in {}ms",
                        e, reconnect_delay
                    );
                    self.metrics.inc_ws_reconnections();
                }
            }

            sleep(Duration::from_millis(reconnect_delay)).await;

            // Exponential backoff capped at MAX_RECONNECT_MS
            reconnect_delay = ((reconnect_delay as f64 * BACKOFF_MULTIPLIER) as u64)
                .min(MAX_RECONNECT_MS);
        }
    }

    /// Connect via WebSocket and stream pending transactions in real time.
    async fn connect_and_stream(&self) -> Result<()> {
        // ── Connect to the WebSocket RPC ──────────────────────────────────────
        let ws = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new()
            .on_ws(ws)
            .await
            .map_err(|e| anyhow::anyhow!("WebSocket connection failed: {}", e))?;

        info!("✓ WebSocket connected. Subscribing to pending transactions...");
        info!("🔎 Watching {} Uniswap V3 router addresses", WATCHED_ROUTERS.len());

        // Fetch and log current block
        match provider.get_block_number().await {
            Ok(block) => info!("📦 Current block: {}", block),
            Err(e) => warn!("Could not fetch block number: {}", e),
        }

        // ── Subscribe to full pending transactions ────────────────────────────
        let sub = provider.subscribe_full_pending_transactions()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to pending txs: {}", e))?;

        let mut stream = sub.into_stream();
        let mut tx_count: u64 = 0;

        info!("🚀 Real-time mempool stream active — listening for pending transactions");

        while let Some(tx) = stream.next().await {
            tx_count += 1;
            self.metrics.inc_txs_seen();

            // Extract transaction fields
            let tx_hash = format!("{:?}", tx.hash);
            let to_addr = match tx.to {
                Some(addr) => format!("{:?}", addr).to_lowercase(),
                None => continue, // Skip contract creation txs
            };

            let gas_price_gwei = tx.gas_price
                .map(|g| g as f64 / 1e9)
                .or_else(|| tx.max_fee_per_gas.map(|g| g as f64 / 1e9))
                .unwrap_or(20.0);

            // Push to recent_mempool_txs for the dashboard UI
            {
                let is_swap = WATCHED_ROUTERS.iter().any(|r| *r == to_addr);
                let mut txs = self.metrics.recent_mempool_txs.write().await;
                let t = serde_json::json!({
                    "id": tx_count,
                    "hash": &tx_hash[..std::cmp::min(12, tx_hash.len())],
                    "type": if is_swap { "SWAP" } else { "PENDING" },
                    "dex": if is_swap { "Uniswap V3" } else { "Mempool" },
                    "token": if is_swap { "WETH/USDC" } else { "UNK" },
                    "size": format!("${:.0}k", (tx.value.to::<u128>() as f64 / 1e18) * 3000.0 / 1000.0),
                    "color": if is_swap { "#00FFD1" } else { "#64748B" },
                    "gasGwei": format!("{:.1}", gas_price_gwei),
                    "ts": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                });
                txs.push_front(t);
                if txs.len() > 50 {
                    txs.pop_back();
                }
            }

            // Process the transaction through our pipeline
            let input_bytes: Vec<u8> = tx.input.to_vec();
            self.process_raw_transaction(
                &to_addr,
                &input_bytes,
                tx.value.to::<u128>(),
                gas_price_gwei,
                &tx_hash,
            ).await;

            // Periodic metrics logging
            if tx_count % METRICS_LOG_INTERVAL == 0 {
                {
                    let graph = self.graph.read().await;
                    self.metrics.set_graph_pools(graph.pool_count() as u64);
                    self.metrics.set_graph_tokens(graph.token_count() as u64);
                }
                self.metrics.log_summary();
            }
        }

        Ok(())
    }

    // ── Public transaction handler ────────────────────────────────────────────

    /// Process a raw transaction from the mempool.
    ///
    /// Filters for watched Uniswap V3 Router addresses, decodes calldata, then
    /// evaluates whether a profitable arbitrage opportunity has opened.
    pub async fn process_raw_transaction(
        &self,
        to: &str,
        input: &[u8],
        _value: u128,
        gas_price_gwei: f64,
        tx_hash: &str,
    ) {
        // Filter: only watched Uniswap V3 Router addresses
        let to_lower = to.to_lowercase();
        if !WATCHED_ROUTERS.iter().any(|r| *r == to_lower) {
            return;
        }

        self.metrics.inc_txs_filtered();

        // Decode calldata
        let decoded = match calldata_decoder::decode_calldata(input) {
            Ok(d) => {
                self.metrics.inc_txs_decoded();
                d
            }
            Err(e) => {
                debug!("Calldata decode failed for {}: {}", tx_hash, e);
                return;
            }
        };

        match decoded {
            DecodedCall::ExactInputSingle(params) => {
                info!(
                    tx_hash   = %tx_hash,
                    token_in  = %params.token_in,
                    token_out = %params.token_out,
                    fee_tier  = params.fee,
                    amount_in = params.amount_in,
                    gas_gwei  = gas_price_gwei,
                    "🔍 Detected pending exactInputSingle swap"
                );

                let fee_bps = fee_tier_to_bps(params.fee);

                self.evaluate_arb_opportunity(
                    &params.token_in,
                    &params.token_out,
                    fee_bps,
                    gas_price_gwei,
                )
                .await;
            }

            DecodedCall::ExactInput(params) => {
                let route: Vec<String> = params
                    .hops
                    .iter()
                    .map(|h| {
                        let tin  = if h.token_in.len()  >= 8 { &h.token_in[..8]  } else { &h.token_in };
                        let tout = if h.token_out.len() >= 8 { &h.token_out[..8] } else { &h.token_out };
                        format!("{}→{}(fee={})", tin, tout, h.fee)
                    })
                    .collect();

                info!(
                    tx_hash   = %tx_hash,
                    route     = %route.join(", "),
                    amount_in = params.amount_in,
                    hops      = params.hops.len(),
                    "🔍 Detected pending exactInput (multi-hop) swap"
                );

                if let (Some(first), Some(last)) = (params.hops.first(), params.hops.last()) {
                    let fee_bps = fee_tier_to_bps(first.fee);
                    self.evaluate_arb_opportunity(
                        &first.token_in,
                        &last.token_out,
                        fee_bps,
                        gas_price_gwei,
                    )
                    .await;
                }
            }

            DecodedCall::Unknown { selector } => {
                debug!(
                    selector = %hex::encode(selector),
                    "Unrecognized Uniswap V3 function selector — skipping"
                );
            }
        }
    }

    // ── Core pipeline ─────────────────────────────────────────────────────────

    /// Evaluate whether a pending swap creates an arbitrage opportunity.
    ///
    /// Steps:
    ///   1. Look up pool state in Redis cache (fast path)
    ///   2. If cache miss, try EvmAdapter on-chain fetch, else synthesise placeholder
    ///   3. Upsert the pool into the shared LiquidityGraph
    ///   4. Run Bellman-Ford to find negative-weight cycles
    ///   5. For each executable opportunity: deduplicate → persist → (simulate) submit
    async fn evaluate_arb_opportunity(
        &self,
        token_in: &str,
        token_out: &str,
        fee_bps: u32,
        gas_gwei: f64,
    ) {
        let pool_cache_key = format!("pool:ethereum:{}:{}:{}", token_in, token_out, fee_bps);

        // ── Step 1: Load pool from Redis ──────────────────────────────────────
        let cached_pool: Option<Pool> = match self.redis_cache.get_raw(&pool_cache_key).await {
            Ok(Some(json)) => {
                match serde_json::from_str::<Pool>(&json) {
                    Ok(p) => {
                        debug!(pool_key = %pool_cache_key, "Pool cache hit");
                        self.metrics.inc_cache_hits();
                        Some(p)
                    }
                    Err(e) => {
                        warn!("Pool deserialize error for {}: {}", pool_cache_key, e);
                        None
                    }
                }
            }
            Ok(None) => {
                debug!(pool_key = %pool_cache_key, "Pool cache miss");
                self.metrics.inc_cache_misses();
                None
            }
            Err(e) => {
                warn!("Redis error looking up {}: {}", pool_cache_key, e);
                self.metrics.inc_redis_errors();
                None
            }
        };

        // ── Step 2: Use cached pool, fetch from chain, or build placeholder ──
        let pool = match cached_pool {
            Some(p) => p,
            None => {
                // Try on-chain fetch via EVM adapter if available
                if let Some(ref adapter) = self.evm_adapter {
                    let placeholder = build_placeholder_pool(token_in, token_out, fee_bps);
                    match adapter.fetch_pool_state(&placeholder).await {
                        Ok(state) => {
                            let mut p = placeholder;
                            p.state = state;
                            // Cache the fetched state in Redis
                            if let Ok(json) = serde_json::to_string(&p) {
                                if let Err(e) = self.redis_cache.set_raw(&pool_cache_key, &json, 24).await {
                                    warn!("Failed to cache pool state: {}", e);
                                }
                            }
                            debug!(
                                token_in  = %token_in,
                                token_out = %token_out,
                                "Pool state fetched from chain and cached"
                            );
                            p
                        }
                        Err(e) => {
                            debug!("On-chain fetch failed ({}), using placeholder", e);
                            build_placeholder_pool(token_in, token_out, fee_bps)
                        }
                    }
                } else {
                    debug!(
                        token_in  = %token_in,
                        token_out = %token_out,
                        fee_bps   = fee_bps,
                        "No EVM adapter — building placeholder pool"
                    );
                    build_placeholder_pool(token_in, token_out, fee_bps)
                }
            }
        };

        // ── Step 3: Upsert pool into LiquidityGraph ───────────────────────────
        {
            let mut graph = self.graph.write().await;
            graph.upsert_pool(pool);
        }

        // ── Step 4: Run Bellman-Ford ──────────────────────────────────────────
        let mut router_config = self.router_config.clone();
        router_config.gas_price_gwei = gas_gwei;

        self.metrics.inc_router_scans();

        let opportunities: Vec<ArbitrageOpportunity> = {
            let graph = self.graph.read().await;
            find_arbitrage_cycles(&graph, &router_config)
        };

        if opportunities.is_empty() {
            debug!(
                token_in  = %token_in,
                token_out = %token_out,
                "No arbitrage cycles found"
            );
            return;
        }

        // ── Step 5: Handle each opportunity ───────────────────────────────────
        for opp in opportunities {
            self.metrics.inc_opportunities_found();

            if !opp.is_executable {
                continue;
            }

            self.metrics.inc_opportunities_executable();

            info!(
                id          = %opp.id,
                nev_wei     = opp.net_expected_value,
                route       = %opp.route_description(),
                hops        = opp.route.len(),
                impact_bps  = opp.price_impact_bps,
                "🚀 Executable arbitrage opportunity — proceeding to submission"
            );

            // ── 5a: Deduplicate via Redis ─────────────────────────────────────
            let opp_id_str = opp.id.to_string();
            let is_new = self
                .redis_cache
                .mark_opportunity_seen(&opp_id_str)
                .await
                .unwrap_or(true);

            if !is_new {
                debug!(id = %opp.id, "Opportunity already seen — skipping");
                continue;
            }

            // ── 5b: Persist to Postgres ───────────────────────────────────────
            if let Some(ref pg) = self.pg_store {
                match pg.insert_opportunity(&opp).await {
                    Ok(_) => {
                        self.metrics.inc_opportunities_persisted();
                    }
                    Err(e) => {
                        warn!("Failed to persist opportunity {}: {}", opp.id, e);
                        self.metrics.inc_pg_errors();
                    }
                }
            }

            // ── 5c: Submit Flashbots bundle (simulated) ───────────────────────
            info!(
                id          = %opp.id,
                route       = %opp.route_description(),
                "📦 Flashbots bundle (simulated — Phase 2 will send real tx)"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a minimal placeholder pool for development when there is no cache hit
/// and no EVM adapter is available.
fn build_placeholder_pool(token_in: &str, token_out: &str, fee_bps: u32) -> Pool {
    Pool {
        id: format!("{}:{}:{}", token_in, token_out, fee_bps),
        chain: ChainId::Ethereum,
        dex: DexProtocol::UniswapV3,
        token_a: Token {
            address:  token_in.to_string(),
            symbol:   "TKA".to_string(),
            decimals: 18,
        },
        token_b: Token {
            address:  token_out.to_string(),
            symbol:   "TKB".to_string(),
            decimals: 18,
        },
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
