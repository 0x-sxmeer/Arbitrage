// ─────────────────────────────────────────────────────────────────────────────
//  mempool/listener.rs  [FIXED PRODUCTION VERSION]
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

const INITIAL_RECONNECT_MS: u64 = 500;
const MAX_RECONNECT_MS:     u64 = 30_000;
const BACKOFF_MULTIPLIER:   f64 = 2.0;

const WORKER_CONCURRENCY: usize = 8;
const CHANNEL_CAPACITY:   usize = 1024; // Expanded for block bursts
const METRICS_LOG_INTERVAL: u64 = 100;
const SQRT_PRICE_STALENESS_THRESHOLD: f64 = 0.0001; // Tightened threshold

const POOL_CACHE_TTL_SECS: usize = 288;

struct RawTxPayload {
    to_addr:        String,
    input:          Vec<u8>,
    value:          u128,
    gas_price_gwei: f64,
    tx_hash:        String,
    tx_count:       u64,
}

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
            live_gas_gwei: Arc::new(std::sync::atomic::AtomicU64::new(f64::to_bits(0.1))),
        }
    }

    pub async fn run(&self) -> Result<()> {
        let listener_clone = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = listener_clone.run_solana_stream().await {
                    tracing::error!("Solana task error: {:?} — restarting in 10s", e);
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });

        self.run_evm_stream().await
    }

    async fn run_solana_stream(&self) -> Result<()> {
        let mut reconnect_delay = INITIAL_RECONNECT_MS;
        loop {
            if let Some(ref url) = self.solana_ws_url {
                info!("Connecting to Solana WebSocket RPC...");
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
                            let mut hb = tokio::time::interval(Duration::from_secs(60));
                            loop { hb.tick().await; }
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
                        error!("Solana WS error: {:?} — reconnecting", e);
                        self.metrics.inc_ws_reconnections();
                    }
                }
            } else {
                std::future::pending::<()>().await;
            }
            sleep(Duration::from_millis(reconnect_delay)).await;
            reconnect_delay = ((reconnect_delay as f64 * BACKOFF_MULTIPLIER) as u64).min(MAX_RECONNECT_MS);
        }
    }

    async fn run_evm_stream(&self) -> Result<()> {
        let mut reconnect_delay = INITIAL_RECONNECT_MS;
        loop {
            {
                let mut graph = self.graph.write().await;
                graph.mark_all_edges_stale();
            }

            match self.connect_and_stream().await {
                Ok(_) => {
                    reconnect_delay = INITIAL_RECONNECT_MS;
                }
                Err(e) => {
                    error!("WebSocket error: {:?} — reconnecting", e);
                    self.metrics.inc_ws_reconnections();
                }
            }

            sleep(Duration::from_millis(reconnect_delay)).await;
            reconnect_delay = ((reconnect_delay as f64 * BACKOFF_MULTIPLIER) as u64).min(MAX_RECONNECT_MS);
        }
    }

    async fn connect_and_stream(&self) -> Result<()> {
        let ws = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new()
            .on_ws(ws)
            .await
            .map_err(|e| anyhow::anyhow!("WebSocket connection failed: {}", e))?;

        info!("✓ EVM WebSocket connected.");

        let (abort_tx, _) = tokio::sync::broadcast::channel::<()>(1);
        let abort_tx_check = abort_tx.clone();
        let provider_check = provider.clone();
        
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(30)).await;
                match tokio::time::timeout(Duration::from_secs(5), provider_check.get_block_number()).await {
                    Ok(Ok(_)) => {}
                    _ => {
                        let _ = abort_tx_check.send(());
                        break;
                    }
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

        let is_l2 = matches!(active_chain, ChainId::Base | ChainId::Arbitrum);

        if is_l2 {
            info!("⛓ Real-time Block Execution Pipeline Active for Network: {:?}", active_chain);
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

                    // ── [FIXED] Spin up Event Log Listener for real-time Sync & Swap reserves ──
                    let filter = alloy::rpc::types::Filter::new()
                        .event_signature(vec![
                            IUniswapV2Pair::Sync::SIGNATURE_HASH,
                            IUniswapV3PoolEvents::Swap::SIGNATURE_HASH,
                        ]);
                    
                    if let Ok(sub_logs) = provider_l2.subscribe_logs(&filter).await {
                        let mut log_stream = sub_logs.into_stream();
                        let graph_arc_logs = Arc::clone(&self_clone.graph);
                        
                        tokio::spawn(async move {
                            while let Some(log) = log_stream.next().await {
                                let pool_address = log.address().to_string().to_lowercase();

                                let mut g = graph_arc_logs.write().await;
                                if let Some(ep) = g.get_pool(&pool_address) {
                                    let mut up = (**ep).clone();
                                    
                                    if log.topics().first() == Some(&IUniswapV2Pair::Sync::SIGNATURE_HASH) {
                                        if let Ok(sync) = IUniswapV2Pair::Sync::decode_log(&log.inner, true) {
                                            up.state.reserve_a = crate::pool::U256::from_str_radix(&sync.reserve0.to_string(), 10).unwrap_or_default();
                                            up.state.reserve_b = crate::pool::U256::from_str_radix(&sync.reserve1.to_string(), 10).unwrap_or_default();
                                            up.last_updated_ts = chrono::Utc::now().timestamp();
                                            g.upsert_pool(up);
                                            tracing::info!("Updated pool {} reserves: A: {}, B: {}", pool_address, sync.reserve0, sync.reserve1);
                                        }
                                    } else if log.topics().first() == Some(&IUniswapV3PoolEvents::Swap::SIGNATURE_HASH) {
                                        if let Ok(swap) = IUniswapV3PoolEvents::Swap::decode_log(&log.inner, true) {
                                            up.state.sqrt_price_x96 = Some(crate::pool::U256::from_str_radix(&swap.sqrtPriceX96.to_string(), 10).unwrap_or_default());
                                            up.state.liquidity = Some(swap.liquidity.to_string().parse::<u128>().unwrap_or_default());
                                            up.state.tick = Some(swap.tick.to_string().parse::<i32>().unwrap_or_default());
                                            up.last_updated_ts = chrono::Utc::now().timestamp();
                                            g.upsert_pool(up);
                                            tracing::info!("Updated pool {} reserves: sqrtPriceX96: {}, liquidity: {}", pool_address, swap.sqrtPriceX96, swap.liquidity);
                                        }
                                    }
                                }
                            }
                        });
                    }

                    while let Some(block) = stream.next().await {
                        block_count += 1;
                        let block_number = block.inner.number;

                        // ── Step 1: Update on-chain states FIRST ──
                        let pools: Vec<Pool> = {
                            let graph = self_clone.graph.read().await;
                            graph.get_all_pools().map(|(_, p)| (**p).clone()).collect()
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
                        let live_gas_gwei_arc = Arc::clone(&self_clone.live_gas_gwei);

                        // Fetch states via Multicall
                        for chunk in pools.chunks(15) { // Expanded chunk size for rapid lookup
                            if let Ok(states) = evm_adapter.fetch_pool_states_multicall(chunk).await {
                                let mut g = graph_arc.write().await;
                                for (i, state) in states.into_iter().enumerate() {
                                    let mut p = chunk[i].clone();
                                    
                                    // FIXED: Removed aggressive dust filtering that was zeroing out micro-liquidity pools
                                    p.state = state;
                                    p.last_updated_block = block_number;
                                    p.last_updated_ts = chrono::Utc::now().timestamp();
                                    g.upsert_pool(p);
                                }
                            }
                        }

                        // ── Step 2: Extract Block Txs to discover dynamic shifting loops ──
                        match provider_l2.get_block_by_number(block_number.into(), alloy::rpc::types::BlockTransactionsKind::Hashes).await {
                            Ok(Some(block_with_hashes)) => {
                                let mut sent = 0;
                                if let alloy::rpc::types::BlockTransactions::Hashes(hashes) = block_with_hashes.transactions {
                                    for hash in hashes {
                                        // Fetch each transaction individually to bypass deserialization errors on L1 deposit txs
                                        if let Ok(Some(tx)) = provider_l2.get_transaction_by_hash(hash).await {
                                            let to_addr = match tx.inner.to() {
                                                Some(addr) => addr.to_string().to_lowercase(),
                                                None => continue,
                                            };
                                            let tx_hash = tx.inner.tx_hash().to_string();
                                            let gas_price_gwei = tx.inner.gas_price().map(|g| g as f64 / 1e9).unwrap_or(0.1);
                                            
                                            let payload = RawTxPayload {
                                                to_addr,
                                                input: tx.inner.input().to_vec(),
                                                value: tx.inner.value().to::<u128>(),
                                                gas_price_gwei,
                                                tx_hash,
                                                tx_count: block_number,
                                            };
                                            let _ = tx_sender_l2.send(payload).await;
                                            sent += 1;
                                        }
                                    }
                                }
                                tracing::debug!("Extracted {} txs from block {}", sent, block_number);
                            }
                            Ok(None) => tracing::warn!("Block {} returned None", block_number),
                            Err(e) => tracing::warn!("Failed to fetch block {}: {}", block_number, e),
                        }

                        // ── Step 3: Run Engine Pathfinder IMMEDIATELY after syncing block state ──
                        let start_tokens = [
                            "0x4200000000000000000000000000000000000006", // WETH
                            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913", // USDC
                            "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf", // cbBTC
                        ];

                        let mut config = router_config.clone();
                        let live_gas = f64::from_bits(live_gas_gwei_arc.load(std::sync::atomic::Ordering::Relaxed));
                        config.gas_price_gwei = if live_gas > 0.001 { live_gas } else { router_config.gas_price_gwei };
                        metrics.inc_router_scans();

                        let opportunities = {
                            let graph = graph_arc.read().await;
                            let mut opps = Vec::new();
                            for start_token in start_tokens {
                                opps.extend(graph.find_opportunities(start_token, &config));
                            }
                            opps.extend(find_arbitrage_cycles(&graph, &config));
                            opps.sort_by(|a, b| b.net_expected_value.cmp(&a.net_expected_value));
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

                            let is_new = redis_cache.mark_opportunity_seen(&opp.route_dedup_key()).await.unwrap_or(true);
                            if !is_new { continue; }

                            if let Some(ref pg) = pg_store {
                                let _ = pg.insert_opportunity(&opp).await;
                            }

                            match evm_adapter.simulate_arbitrage(&opp).await {
                                Ok(()) => {
                                    info!("🔥 Arbitrage Loop Found! Route: {} Net Value: {} Wei", opp.route_description(), opp.net_expected_value);
                                    if self_clone.execute_enabled {
                                        let adapter_clone = Arc::clone(&evm_adapter);
                                        tokio::spawn(async move {
                                            let _ = adapter_clone.execute_arbitrage(&opp).await;
                                        });
                                    }
                                }
                                Err(_) => {
                                    let mut graph_lock = graph_arc.write().await;
                                    for step in &opp.route {
                                        graph_lock.blacklist_pool(&step.pool_id);
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }

        // Keep standard pending stream listening as a fallback mechanism
        let sub = provider
            .subscribe_pending_transactions()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to pending txs: {}", e))?;
        let mut stream = sub.into_stream();
        let mut tx_count: u64 = 0;
        while let Some(raw_hash) = stream.next().await {
            tx_count += 1;
            self.metrics.inc_txs_seen();
            if let Ok(Some(tx)) = provider.get_transaction_by_hash(raw_hash).await {
                let to_addr = match tx.inner.to() {
                    Some(addr) => addr.to_string().to_lowercase(),
                    None => continue,
                };
                let tx_hash = tx.inner.tx_hash().to_string();
                let gas_price_gwei = tx.inner.gas_price().map(|g| g as f64 / 1e9).unwrap_or(10.0);
                
                let payload = RawTxPayload {
                    to_addr,
                    input: tx.inner.input().to_vec(),
                    value: tx.inner.value().to::<u128>(),
                    gas_price_gwei,
                    tx_hash,
                    tx_count: tx_count,
                };
                let _ = tx_sender.try_send(payload);
            }
        }

        Ok(())
    }

    fn maybe_update_dashboard(&self, tx_hash: &str, to_addr: &str, tx: &alloy::rpc::types::Transaction, gas_price_gwei: f64, tx_count: u64) {
        let dex = classify_router(to_addr);
        let is_swap = dex != DexVersion::Unknown;
        if let Ok(mut txs) = self.metrics.recent_mempool_txs.try_write() {
            let entry = serde_json::json!({
                "id":      tx_count,
                "hash":    &tx_hash[..12],
                "type":    if is_swap { "SWAP" } else { "PENDING" },
                "dex":     format!("{:?}", dex),
                "token":   "UNI/AERO",
                "size":    "-",
                "color":   if is_swap { "#00FFD1" } else { "#64748B" },
                "gasGwei": format!("{:.2}", gas_price_gwei),
                "ts":      chrono::Utc::now().timestamp_millis(),
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
        self.metrics.inc_txs_seen();

        // Broaden execution decoding criteria to catch non-standard contract proxy trades
        let decoded = match decode_swap(&payload.input, &payload.to_addr) {
            Some(d) => d,
            None => {
                // Populate mempool dashboard with pending tx
                if let Ok(mut txs) = self.metrics.recent_mempool_txs.try_write() {
                    let entry = serde_json::json!({
                        "id":      format!("{}-{}", payload.tx_hash, payload.tx_count),
                        "hash":    &payload.tx_hash[..12],
                        "type":    "PENDING",
                        "dex":     "UNK",
                        "token":   "-",
                        "size":    "-",
                        "color":   "#64748B",
                        "gasGwei": format!("{:.2}", payload.gas_price_gwei),
                        "ts":      chrono::Utc::now().timestamp_millis(),
                    });
                    txs.push_front(entry);
                    if txs.len() > 50 { txs.pop_back(); }
                }
                return;
            }
        };

        self.metrics.inc_txs_decoded();

        let dex = classify_router(&payload.to_addr);
        if let Ok(mut txs) = self.metrics.recent_mempool_txs.try_write() {
            let entry = serde_json::json!({
                "id":      format!("{}-{}", payload.tx_hash, payload.tx_count),
                "hash":    &payload.tx_hash[..12],
                "type":    "SWAP",
                "dex":     format!("{:?}", dex),
                "token":   "ERC20",
                "size":    "-",
                "color":   "#00FFD1",
                "gasGwei": format!("{:.2}", payload.gas_price_gwei),
                "ts":      chrono::Utc::now().timestamp_millis(),
            });
            txs.push_front(entry);
            if txs.len() > 50 { txs.pop_back(); }
        }

        let token_in  = decoded.token_in.to_string().to_lowercase();
        let token_out = decoded.token_out.to_string().to_lowercase();
        let amount_in = crate::pool::U256::from_str_radix(&decoded.amount_in.to_string(), 10).unwrap_or_default();

        self.evaluate_arb_opportunity(
            &token_in,
            &token_out,
            decoded.fee_bps,
            payload.gas_price_gwei,
            amount_in,
            &decoded.dex_version,
        )
        .await;
    }

    async fn evaluate_arb_opportunity(&self, token_in: &str, token_out: &str, fee_bps: u32, gas_gwei: f64, amount_in: U256, dex_version: &DexVersion) {
        let active_chain = self.evm_adapter.as_ref().map(|a| a.chain()).unwrap_or(ChainId::Base);
        let pool_cache_key = format!("pool:{}:{}:{}:{}", active_chain.name(), token_in, token_out, fee_bps);

        let cached_pool: Option<Pool> = match self.redis_cache.get_raw(&pool_cache_key).await {
            Ok(Some(json)) => serde_json::from_str::<Pool>(&json).ok(),
            _ => None,
        };

        let mut pool = match cached_pool {
            Some(p) => p,
            None => {
                let graph_pool = {
                    let graph = self.graph.read().await;
                    graph.get_all_pools()
                        .find(|(_, p)| {
                            ((p.token_a.address.to_lowercase() == token_in && p.token_b.address.to_lowercase() == token_out) ||
                             (p.token_a.address.to_lowercase() == token_out && p.token_b.address.to_lowercase() == token_in)) &&
                            p.fee_bps == fee_bps
                        })
                        .map(|(_, p)| (**p).clone())
                };

                if let Some(ref adapter) = self.evm_adapter {
                    let base_pool = if let Some(p) = graph_pool {
                        p
                    } else {
                        let mut p = build_placeholder_pool(token_in, token_out, fee_bps, active_chain);
                        if let Ok(real_address) = adapter.query_pool_address(token_in, token_out, fee_bps, dex_version).await {
                            p.id = real_address;
                            p.pool_type = match dex_version {
                                DexVersion::UniswapV3 | DexVersion::AerodromeV3 => crate::pool::PoolType::ConcentratedLiquidity,
                                _ => crate::pool::PoolType::ConstantProduct,
                            };
                            p.dex = match dex_version {
                                DexVersion::AerodromeV2 | DexVersion::UniswapV2 | DexVersion::BaseSwap => crate::pool::DexProtocol::UniswapV2,
                                DexVersion::AerodromeV3 | DexVersion::UniswapV3 => crate::pool::DexProtocol::UniswapV3,
                                _ => crate::pool::DexProtocol::UniswapV3,
                            };
                            p
                        } else {
                            return;
                        }
                    };
                    
                    match adapter.fetch_pool_state(&base_pool).await {
                        Ok(state) => {
                            let mut p = base_pool;
                            p.state = state;
                            if let Ok(json) = serde_json::to_string(&p) {
                                let _ = self.redis_cache.set_raw(&pool_cache_key, &json, POOL_CACHE_TTL_SECS).await;
                            }
                            p
                        }
                        Err(_) => return,
                    }
                } else {
                    match graph_pool {
                        Some(p) => p,
                        None => return,
                    }
                }
            }
        };

        pool.simulate_swap(token_in.to_string(), amount_in);

        {
            let mut graph = self.graph.write().await;
            graph.upsert_pool(pool);
        }
    }
}

fn build_placeholder_pool(token_in: &str, token_out: &str, fee_bps: u32, chain: ChainId) -> Pool {
    let t_in  = token_in.to_lowercase();
    let t_out = token_out.to_lowercase();
    let (addr_a, addr_b) = if t_in < t_out { (t_in, t_out) } else { (t_out, t_in) };

    Pool {
        id: format!("{}:{}:{}", addr_a, addr_b, fee_bps),
        chain,
        dex: DexProtocol::UniswapV3,
        token_a: Token { address: addr_a.clone(), symbol: "TOK_A".to_string(), decimals: crate::pool::get_token_decimals(&addr_a) },
        token_b: Token { address: addr_b.clone(), symbol: "TOK_B".to_string(), decimals: crate::pool::get_token_decimals(&addr_b) },
        pool_type: PoolType::ConcentratedLiquidity,
        fee_bps,
        state: PoolState {
            reserve_a:      U256::from(100_000_000_000_000_000_000u128),
            reserve_b:      U256::from(100_000_000_000_000_000_000u128),
            sqrt_price_x96: Some(U256::from(1_936_540_681_085_355_540_000_000_000_000u128)),
            tick:           Some(201_210),
            liquidity:      Some(12_345_678_901_234_567_890),
            amp_coeff:      None,
        },
        last_updated_block: 0,
        last_updated_ts:    0,
    }
}
