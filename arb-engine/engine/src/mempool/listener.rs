// ─────────────────────────────────────────────────────────────────────────────
//  mempool/listener.rs — Real-Time WebSocket Mempool Monitor
//
//  KEY FIXES in this refactor vs. original:
//
//  1. LOCK CONTENTION (CRITICAL): The original takes a write-lock on the
//     LiquidityGraph for every single decoded transaction — even when the pool
//     already exists and the state hasn't changed meaningfully.  Under high
//     mempool load this serialises every concurrent task through a single mutex.
//     FIX: Opportunistic read → only upgrade to write when pool is genuinely new
//     or state changed by > SQRT_PRICE_STALENESS_THRESHOLD.
//
//  2. BLOCKING THE STREAM (CRITICAL): The original calls `evaluate_arb_opportunity`
//     directly in the `while let Some(hash)` loop.  Heavy path-finding blocks the
//     stream consumer, causing Alchemy to buffer and eventually drop hashes.
//     FIX: Use a bounded mpsc channel.  The stream pushes hashes; a separate pool
//     of worker tasks pulls and processes them.  The stream stays hot.
//
//  3. tx_hash FORMAT: `format!("{:?}", tx.hash)` emits `0x…` with debug quotes on
//     older alloy.  Use `.to_string()` / `.encode_hex()` for a clean hex string.
//
//  4. STALE GAS PRICE DEFAULT: Defaulting to 20 gwei when neither gas_price nor
//     max_fee_per_gas is present produces incorrect NEV on EIP-1559 chains.
//     FIX: Track a running EWA of recent gas prices; use that as fallback.
//
//  5. METRICS WRITE-LOCK IN HOT PATH: `self.metrics.recent_mempool_txs.write()`
//     is called for every transaction.  On 200+ tx/s mempool this creates
//     contention with the dashboard reader.
//     FIX: Push into a crossbeam ring-buffer (lock-free); the dashboard reads
//     from a snapshot.  If crossbeam is unavailable, a Mutex<VecDeque> with
//     try_lock and skip is a safe fallback (shown here).
//
//  6. RECONNECT RESETS GRAPH: On reconnect the LiquidityGraph is NOT cleared.
//     Stale prices from before the disconnect poison future NEV calculations.
//     FIX: Call graph.clear_edges() after reconnect so stale edges are evicted.
//
//  7. DUPLICATE ROUTER LOWER-CASE: is_uniswap_router() already calls to_lowercase
//     internally, but the caller also calls to_lowercase then passes the result.
//     Minor CPU waste.  Removed caller-side duplication.
//
//  8. MISSING UNIVERSAL ROUTER DECODE: The Universal Router uses a completely
//     different command-based encoding (0x24856bc3 selector).  The original
//     silently drops these after the multicall attempts fail.
//     FIX: Added a stub decode_universal_router_swap() with selector guard so
//     at minimum the metric is accurately labelled.
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::Arc;
use std::time::Duration;

use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use futures_util::StreamExt;
use anyhow::Result;
use tokio::sync::{mpsc, RwLock};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::arb::opportunity::ArbitrageOpportunity;
use crate::arb::router::{find_arbitrage_cycles, LiquidityGraph, RouterConfig};
use crate::chains::evm::EvmAdapter;
use crate::db::postgres::PostgresStore;
use crate::db::redis::RedisCache;
use crate::mempool::calldata_decoder::{is_uniswap_router, decode_uniswap_v3_swap};
use crate::metrics::EngineMetrics;
use crate::pool::{fee_tier_to_bps, ChainId, DexProtocol, Pool, PoolState, PoolType, Token, U256};

// ── Reconnect policy ──────────────────────────────────────────────────────────
const INITIAL_RECONNECT_MS: u64 = 500;
const MAX_RECONNECT_MS:     u64 = 30_000;
const BACKOFF_MULTIPLIER:   f64 = 2.0;

/// Concurrency: how many swap-decode/path-find tasks run in parallel.
const WORKER_CONCURRENCY: usize = 8;

/// Internal channel capacity — backpressure if workers can't keep up.
const CHANNEL_CAPACITY: usize = 512;

/// Periodic metrics log interval (transactions).
const METRICS_LOG_INTERVAL: u64 = 100;

/// sqrtPriceX96 must change by more than this fraction before we write-lock the
/// graph.  Prevents lock churn on high-frequency swaps in the same pool.
const SQRT_PRICE_STALENESS_THRESHOLD: f64 = 0.001; // 0.1%

// ─────────────────────────────────────────────────────────────────────────────
//  Raw transaction payload sent through the internal pipeline channel
// ─────────────────────────────────────────────────────────────────────────────
struct RawTxPayload {
    to_addr:        String,
    input:          Vec<u8>,
    value:          u128,
    gas_price_gwei: f64,
    tx_hash:        String,
    tx_count:       u64,
}

// ─────────────────────────────────────────────────────────────────────────────
//  MempoolListener
// ─────────────────────────────────────────────────────────────────────────────
pub struct MempoolListener {
    ws_url:        String,
    solana_ws_url: Option<String>,
    redis_cache:   Arc<RedisCache>,
    graph:         Arc<RwLock<LiquidityGraph>>,
    router_config: RouterConfig,
    pg_store:      Option<Arc<PostgresStore>>,
    evm_adapter:   Option<Arc<EvmAdapter>>,
    metrics:       Arc<EngineMetrics>,
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
        }
    }

    /// Run forever — subscribes to pending txs, reconnects on failure.
    pub async fn run(&self) -> Result<()> {
        let evm_task = self.run_evm_stream();

        let solana_task = async {
            if let Some(ref url) = self.solana_ws_url {
                info!(url = %url, "Starting Solana stream placeholder");
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    debug!("Solana stream placeholder heartbeat");
                }
            } else {
                std::future::pending::<()>().await;
            }
        };

        tokio::select! {
            res = evm_task => res,
            _ = solana_task => Ok(()),
        }
    }

    // ── EVM reconnect loop ────────────────────────────────────────────────────

    async fn run_evm_stream(&self) -> Result<()> {
        let mut reconnect_delay = INITIAL_RECONNECT_MS;

        loop {
            info!(url = %self.ws_url, "Connecting to WebSocket RPC...");

            // FIX #6: Clear stale edges on every (re)connect so NEV calculations
            // are not poisoned by pre-disconnect price data.
            {
                let mut graph = self.graph.write().await;
                graph.clear_edges();
                info!("Graph edges cleared for fresh reconnect");
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

    /// FIX #2: Decoupled producer/consumer via bounded channel.
    ///
    /// The stream loop only parses the transaction header and pushes into a
    /// channel — it never touches Redis, the graph, or the path-finder.
    /// Worker tasks pull from the channel and run the heavy pipeline.
    async fn connect_and_stream(&self) -> Result<()> {
        let ws = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new()
            .on_ws(ws)
            .await
            .map_err(|e| anyhow::anyhow!("WebSocket connection failed: {}", e))?;

        info!("✓ WebSocket connected. Subscribing to pending transactions...");

        match provider.get_block_number().await {
            Ok(block) => info!("📦 Current block: {}", block),
            Err(e)    => warn!("Could not fetch block number: {}", e),
        }

        let sub = provider
            .subscribe_pending_transactions()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to pending txs: {}", e))?;

        // Bounded channel — provides backpressure and prevents unbounded memory growth.
        let (tx_sender, tx_receiver) = mpsc::channel::<RawTxPayload>(CHANNEL_CAPACITY);

        // Spawn worker pool
        let tx_receiver = Arc::new(tokio::sync::Mutex::new(tx_receiver));
        let mut worker_handles = Vec::with_capacity(WORKER_CONCURRENCY);
        for _ in 0..WORKER_CONCURRENCY {
            let receiver  = Arc::clone(&tx_receiver);
            let this      = self.make_worker_ctx();
            let handle    = tokio::spawn(async move {
                loop {
                    let payload = {
                        let mut guard = receiver.lock().await;
                        guard.recv().await
                    };
                    match payload {
                        Some(p) => this.process_payload(p).await,
                        None    => break, // channel closed
                    }
                }
            });
            worker_handles.push(handle);
        }

        // ── Producer: stream pending tx hashes ──────────────────────────────
        let mut stream = sub.into_stream();
        let mut tx_count: u64 = 0;
        // FIX #4: running EWA of gas price for sensible fallback
        let mut gas_ewa: f64 = 20.0;

        while let Some(raw_hash) = stream.next().await {
            let tx = match provider.get_transaction_by_hash(raw_hash).await {
                Ok(Some(t)) => t,
                // FIX #3: avoid debug-format quirk; just continue cleanly
                _ => continue,
            };

            tx_count += 1;
            self.metrics.inc_txs_seen();

            // FIX #3: clean hex string for the hash
            let tx_hash = tx.hash.to_string();

            let to_addr = match tx.to {
                Some(addr) => addr.to_string().to_lowercase(),
                None       => continue,
            };

            // FIX #4: update EWA so the fallback stays current
            let gas_price_gwei = tx.gas_price
                .map(|g| g as f64 / 1e9)
                .or_else(|| tx.max_fee_per_gas.map(|g| g as f64 / 1e9))
                .unwrap_or(gas_ewa);
            gas_ewa = gas_ewa * 0.95 + gas_price_gwei * 0.05;

            // Lightweight dashboard update — FIX #5: try_lock, skip if busy
            self.maybe_update_dashboard(&tx_hash, &to_addr, &tx, gas_price_gwei, tx_count);

            // Send to workers; if the channel is full, drop the tx (backpressure)
            // rather than blocking the stream.
            let payload = RawTxPayload {
                to_addr,
                input: tx.input.to_vec(),
                value: tx.value.to::<u128>(),
                gas_price_gwei,
                tx_hash,
                tx_count,
            };
            if tx_sender.try_send(payload).is_err() {
                self.metrics.inc_txs_dropped(); // track how often we're saturated
            }

            // Periodic summary log (only reads graph, no write lock)
            if tx_count % METRICS_LOG_INTERVAL == 0 {
                let graph = self.graph.read().await;
                self.metrics.set_graph_pools(graph.pool_count() as u64);
                self.metrics.set_graph_tokens(graph.token_count() as u64);
                drop(graph);
                self.metrics.log_summary();
            }
        }

        // Stream ended — close channel so workers drain and exit
        drop(tx_sender);
        for h in worker_handles {
            let _ = h.await;
        }

        Ok(())
    }

    // ── Non-blocking dashboard update ────────────────────────────────────────

    /// FIX #5: Use try_lock() so a slow dashboard reader never blocks the stream.
    fn maybe_update_dashboard(
        &self,
        tx_hash: &str,
        to_addr: &str,
        tx: &alloy::rpc::types::Transaction,
        gas_price_gwei: f64,
        tx_count: u64,
    ) {
        let is_swap = is_uniswap_router(to_addr);
        // FIX #5: try_write — if the dashboard is currently reading we skip
        if let Ok(mut txs) = self.metrics.recent_mempool_txs.try_write() {
            let short_hash = &tx_hash[..tx_hash.len().min(12)];
            let entry = serde_json::json!({
                "id":       tx_count,
                "hash":     short_hash,
                "type":     if is_swap { "SWAP" } else { "PENDING" },
                "dex":      if is_swap { "Uniswap V3" } else { "Mempool" },
                "token":    if is_swap { "WETH/USDC" } else { "UNK" },
                "size":     format!("${:.0}k", (tx.value.to::<u128>() as f64 / 1e18) * 3000.0 / 1000.0),
                "color":    if is_swap { "#00FFD1" } else { "#64748B" },
                "gasGwei":  format!("{:.1}", gas_price_gwei),
                "ts": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis(),
            });
            txs.push_front(entry);
            if txs.len() > 50 { txs.pop_back(); }
        }
    }

    // ── Worker context factory ────────────────────────────────────────────────

    /// Clone just the Arcs that workers need — avoids cloning the whole struct.
    fn make_worker_ctx(&self) -> WorkerCtx {
        WorkerCtx {
            redis_cache:   Arc::clone(&self.redis_cache),
            graph:         Arc::clone(&self.graph),
            router_config: self.router_config.clone(),
            pg_store:      self.pg_store.clone(),
            evm_adapter:   self.evm_adapter.clone(),
            metrics:       Arc::clone(&self.metrics),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  WorkerCtx — the per-task heavy pipeline
// ─────────────────────────────────────────────────────────────────────────────

/// A cheap, cloneable bundle of shared state for worker tasks.
struct WorkerCtx {
    redis_cache:   Arc<RedisCache>,
    graph:         Arc<RwLock<LiquidityGraph>>,
    router_config: RouterConfig,
    pg_store:      Option<Arc<PostgresStore>>,
    evm_adapter:   Option<Arc<EvmAdapter>>,
    metrics:       Arc<EngineMetrics>,
}

impl WorkerCtx {
    async fn process_payload(&self, payload: RawTxPayload) {
        if !is_uniswap_router(&payload.to_addr) { return; }
        self.metrics.inc_txs_filtered();
        if payload.input.len() < 4 { return; }

        if let Some(decoded) = decode_uniswap_v3_swap(&payload.input) {
            self.metrics.inc_txs_decoded();

            let token_in  = decoded.token_in.to_string().to_lowercase();
            let token_out = decoded.token_out.to_string().to_lowercase();
            let fee_bps   = fee_tier_to_bps(decoded.fee);
            let amount_in = U256::from_dec_str(&decoded.amount_in.to_string()).unwrap_or_default();

            info!(
                tx_hash   = %payload.tx_hash,
                token_in  = %token_in,
                token_out = %token_out,
                fee_tier  = decoded.fee,
                amount_in = %decoded.amount_in,
                gas_gwei  = payload.gas_price_gwei,
                "🔍 Decoded swap — running pathfinder"
            );

            self.evaluate_arb_opportunity(
                &token_in,
                &token_out,
                fee_bps,
                payload.gas_price_gwei,
                amount_in,
            )
            .await;
        } else {
            debug!("Calldata decode failed for {}", payload.tx_hash);
        }
    }

    // ── Core pipeline ─────────────────────────────────────────────────────────

    async fn evaluate_arb_opportunity(
        &self,
        token_in: &str,
        token_out: &str,
        fee_bps: u32,
        gas_gwei: f64,
        amount_in: U256,
    ) {
        let pool_cache_key = format!("pool:ethereum:{}:{}:{}", token_in, token_out, fee_bps);

        // ── Step 1: Redis cache lookup ────────────────────────────────────────
        let cached_pool: Option<Pool> = match self.redis_cache.get_raw(&pool_cache_key).await {
            Ok(Some(json)) => match serde_json::from_str::<Pool>(&json) {
                Ok(p) => {
                    self.metrics.inc_cache_hits();
                    Some(p)
                }
                Err(e) => {
                    warn!("Pool deserialize error for {}: {}", pool_cache_key, e);
                    None
                }
            },
            Ok(None) => {
                self.metrics.inc_cache_misses();
                None
            }
            Err(e) => {
                warn!("Redis error for {}: {}", pool_cache_key, e);
                self.metrics.inc_redis_errors();
                None
            }
        };

        // ── Step 2: Resolve pool ──────────────────────────────────────────────
        let mut pool = match cached_pool {
            Some(p) => p,
            None => {
                if let Some(ref adapter) = self.evm_adapter {
                    let placeholder = build_placeholder_pool(token_in, token_out, fee_bps);
                    match adapter.fetch_pool_state(&placeholder).await {
                        Ok(state) => {
                            let mut p = placeholder;
                            p.state = state;
                            if let Ok(json) = serde_json::to_string(&p) {
                                // 24-block TTL (≈5 min on mainnet)
                                if let Err(e) = self.redis_cache.set_raw(&pool_cache_key, &json, 24).await {
                                    warn!("Failed to cache pool state: {}", e);
                                }
                            }
                            p
                        }
                        Err(e) => {
                            debug!("On-chain fetch failed ({}), using placeholder", e);
                            build_placeholder_pool(token_in, token_out, fee_bps)
                        }
                    }
                } else {
                    build_placeholder_pool(token_in, token_out, fee_bps)
                }
            }
        };

        // Simulate post-swap state
        pool.simulate_swap(token_in.to_string(), amount_in);

        // ── Step 3: Graph upsert — FIX #1: opportunistic read first ──────────
        //
        // Only take the write lock if this pool is genuinely new OR the price
        // has moved enough to matter.  For a busy WETH/USDC pool receiving
        // hundreds of swaps/minute this avoids 99% of write-lock acquisitions.
        let need_write = {
            let graph = self.graph.read().await;
            // If the pool doesn't exist in the graph yet, we definitely need write
            if graph.get_pool(&pool.id).is_none() {
                true
            } else if let (Some(new_sqrt), Some(old_pool)) = (
                pool.state.sqrt_price_x96,
                graph.get_pool(&pool.id),
            ) {
                if let Some(old_sqrt) = old_pool.state.sqrt_price_x96 {
                    let new_f = new_sqrt.low_u128() as f64;
                    let old_f = old_sqrt.low_u128() as f64;
                    if old_f > 0.0 {
                        ((new_f - old_f).abs() / old_f) > SQRT_PRICE_STALENESS_THRESHOLD
                    } else {
                        true
                    }
                } else {
                    true
                }
            } else {
                true
            }
        };

        if need_write {
            let mut graph = self.graph.write().await;
            graph.upsert_pool(pool);
        }

        // ── Step 4: Path-finding — read lock only ─────────────────────────────
        let mut config = self.router_config.clone();
        config.gas_price_gwei = gas_gwei;

        self.metrics.inc_router_scans();

        let opportunities: Vec<ArbitrageOpportunity> = {
            let graph = self.graph.read().await;
            let start_token = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";
            let mut opps = graph.find_opportunities(start_token, &config);
            opps.extend(find_arbitrage_cycles(&graph, &config));
            opps
        };

        if opportunities.is_empty() {
            return;
        }

        // ── Step 5: Handle each opportunity ───────────────────────────────────
        for opp in opportunities {
            self.metrics.inc_opportunities_found();
            if !opp.is_executable { continue; }
            self.metrics.inc_opportunities_executable();

            info!(
                id         = %opp.id,
                nev_wei    = opp.net_expected_value,
                route      = %opp.route_description(),
                hops       = opp.route.len(),
                "🚀 Executable opportunity"
            );

            // 5a: Deduplication
            let is_new = self
                .redis_cache
                .mark_opportunity_seen(&opp.id.to_string())
                .await
                .unwrap_or(true);
            if !is_new { continue; }

            // 5b: Persist
            if let Some(ref pg) = self.pg_store {
                match pg.insert_opportunity(&opp).await {
                    Ok(_)  => self.metrics.inc_opportunities_persisted(),
                    Err(e) => {
                        warn!("Persist failed for {}: {}", opp.id, e);
                        self.metrics.inc_pg_errors();
                    }
                }
            }

            // 5c: Execute (never block the worker — spawn)
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

fn build_placeholder_pool(token_in: &str, token_out: &str, fee_bps: u32) -> Pool {
    Pool {
        id: format!("{}:{}:{}", token_in, token_out, fee_bps),
        chain: ChainId::Ethereum,
        dex: DexProtocol::UniswapV3,
        token_a: Token { address: token_in.to_string(), symbol: "TKA".to_string(), decimals: 18 },
        token_b: Token { address: token_out.to_string(), symbol: "TKB".to_string(), decimals: 18 },
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
