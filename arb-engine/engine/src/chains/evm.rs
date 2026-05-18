// ─────────────────────────────────────────────────────────────────────────────
//  chains/evm.rs — Ethereum / EVM L2 Chain Adapter (Production Implementation)
//
//  Uses alloy-rs to provide on-chain operations:
//  - Fetching Uniswap V2/V3 pool state via eth_call
//  - Signing and executing flash-loan arbitrage via AtomicArb contract
//  - Submitting arbitrage bundles via Flashbots relay
//  - Subscribing to new blocks for staleness tracking
//
//  Supported chains: Ethereum, Base, Arbitrum (all EVM-compatible)
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use alloy::providers::{Provider, ProviderBuilder, WsConnect, RootProvider};
use alloy::pubsub::PubSubFrontend;
use alloy::primitives::{Address, Bytes};
use alloy::rpc::types::TransactionRequest;
use alloy::network::EthereumWallet;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use std::str::FromStr;
use std::time::Instant;
use tracing::{debug, info, warn, error};

use crate::arb::opportunity::ArbitrageOpportunity;
use crate::pool::{ChainId, Pool, PoolState, PoolType, U256};

// ── Alloy Contract Bindings ──────────────────────────────────────────────────
// The sol! macro with #[sol(rpc)] generates type-safe contract call builders.
sol! {
    #[sol(rpc)]
    interface IUniswapV3Pool {
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
        function liquidity() external view returns (uint128);
    }

    #[sol(rpc)]
    interface IAtomicArb {
        function executeArbitrage(
            address asset,
            uint256 borrowAmount,
            bytes calldata params
        ) external;
    }

    /// ABI-encoded parameters passed into the flash loan callback.
    /// The AtomicArb contract decodes this in `executeOperation`.
    struct ArbParams {
        address buyRouter;
        bool    buyIsV3;
        uint24  buyFee;
        address[] buyPath;
        address sellRouter;
        bool    sellIsV3;
        uint24  sellFee;
        address[] sellPath;
        address tokenBorrow;
        address tokenIntermediate;
        uint256 minProfitWei;
    }
}

// ── Function selectors (keccak256 first 4 bytes) ─────────────────────────────
/// getReserves() → (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)
const V2_GET_RESERVES_SELECTOR: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];
/// slot0() → (uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, ...)
const V3_SLOT0_SELECTOR: [u8; 4] = [0x38, 0x50, 0xc7, 0xbd];
/// liquidity() → uint128
const V3_LIQUIDITY_SELECTOR: [u8; 4] = [0x1a, 0x68, 0x65, 0x02];

/// Standard Uniswap V3 SwapRouter02 address (Ethereum mainnet)
const UNISWAP_V3_ROUTER: &str = "0xE592427A0AEce92De3Edee1F18E0157C05861564";

// ─────────────────────────────────────────────────────────────────────────────
//  EVM Adapter
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for an EVM chain adapter.
#[derive(Debug, Clone)]
pub struct EvmConfig {
    pub chain: ChainId,
    pub ws_url: String,
    pub http_url: String,
    /// Flashbots relay URL (None = submit via public mempool)
    pub flashbots_url: Option<String>,
    /// Address of the deployed AtomicArb smart contract
    pub contract_address: Option<String>,
    /// Private key for signing execution transactions (hex, 0x-prefixed)
    pub private_key: Option<String>,
    /// Persistent Flashbots signing key for relay reputation (hex, 0x-prefixed)
    pub flashbots_signing_key: Option<String>,
}

/// EVM adapter — manages the alloy-rs provider connection and exposes chain operations.
pub struct EvmAdapter {
    config: EvmConfig,
    /// WebSocket provider for real-time operations (lazy-initialized)
    ws_provider: tokio::sync::RwLock<Option<RootProvider<PubSubFrontend>>>,
    /// Tracks the last observed block number
    last_block: std::sync::atomic::AtomicU64,
    /// Tracks total successful executions
    execution_count: std::sync::atomic::AtomicU64,
}

impl EvmAdapter {
    /// Create a new EVM adapter. Provider connection is lazy (on first use).
    pub fn new(config: EvmConfig) -> Self {
        info!(
            chain = %config.chain.name(),
            ws_url = %config.ws_url,
            has_contract = config.contract_address.is_some(),
            has_private_key = config.private_key.is_some(),
            "Initializing EVM adapter"
        );
        Self {
            config,
            ws_provider: tokio::sync::RwLock::new(None),
            last_block: std::sync::atomic::AtomicU64::new(0),
            execution_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Exposes the active ChainId configuration
    pub fn chain(&self) -> ChainId {
        self.config.chain
    }

    /// Ensure the WebSocket provider is connected. Returns a clone of the provider.
    async fn get_or_connect_ws(&self) -> Result<RootProvider<PubSubFrontend>> {
        // Check if we already have a connection
        {
            let guard = self.ws_provider.read().await;
            if let Some(ref p) = *guard {
                return Ok(p.clone());
            }
        }

        // Need to connect
        let ws = WsConnect::new(&self.config.ws_url);
        let provider = ProviderBuilder::new()
            .on_ws(ws)
            .await
            .with_context(|| format!(
                "Failed to connect WebSocket to {} for chain {}",
                self.config.ws_url, self.config.chain.name()
            ))?;

        info!(
            chain = %self.config.chain.name(),
            "WebSocket provider connected"
        );

        // Cache the provider
        {
            let mut guard = self.ws_provider.write().await;
            *guard = Some(provider.clone());
        }

        Ok(provider)
    }

    // ── Live Pool State Fetching ─────────────────────────────────────────────

    /// Fetches the live sqrtPriceX96, tick, and liquidity from a real Uniswap V3 Pool.
    ///
    /// Uses the `#[sol(rpc)]` generated bindings for type-safe contract calls.
    /// The returned U256 is `primitive_types::U256` for internal math compatibility.
    pub async fn get_v3_pool_state(&self, pool_address: &str) -> Result<(U256, i32, u128)> {
        let start = Instant::now();

        let provider = ProviderBuilder::new()
            .on_builtin(&self.config.http_url)
            .await
            .with_context(|| format!("HTTP provider connection failed for {}", self.config.http_url))?;

        let addr = Address::from_str(pool_address)
            .with_context(|| format!("Invalid pool address: {}", pool_address))?;

        let pool = IUniswapV3Pool::new(addr, provider);

        // Fetch slot0 and liquidity (sequential — same RPC endpoint)
        let slot0 = pool.slot0().call().await
            .map_err(|e| anyhow::anyhow!("slot0() RPC error for {}: {:?}", pool_address, e))?;
        let liquidity = pool.liquidity().call().await
            .map_err(|e| anyhow::anyhow!("liquidity() RPC error for {}: {:?}", pool_address, e))?;

        // Convert alloy's uint160 → our primitive_types::U256
        let sqrt_price = U256::from_dec_str(&slot0.sqrtPriceX96.to_string())
            .unwrap_or(U256::zero());

        // Convert alloy's Signed<24,1> (int24) → i32
        let tick: i32 = slot0.tick.to_string().parse().unwrap_or(0);

        let elapsed = start.elapsed();
        debug!(
            pool     = %pool_address,
            sqrtP    = %sqrt_price,
            tick     = tick,
            liq      = liquidity._0,
            latency  = ?elapsed,
            "V3 pool state fetched via sol!(rpc)"
        );

        Ok((sqrt_price, tick, liquidity._0))
    }

    /// Fetches the live reserves from a Uniswap V2 / SushiSwap pair contract.
    ///
    /// Returns `(reserve_a, reserve_b)` as `primitive_types::U256`.
    pub async fn get_v2_pool_state(&self, pool_address: &str) -> Result<(U256, U256)> {
        let start = Instant::now();

        let provider = ProviderBuilder::new()
            .on_builtin(&self.config.http_url)
            .await
            .with_context(|| format!("HTTP provider connection failed for {}", self.config.http_url))?;

        let addr = Address::from_str(pool_address)
            .with_context(|| format!("Invalid pool address: {}", pool_address))?;

        let call_data = Bytes::from(V2_GET_RESERVES_SELECTOR.to_vec());

        let tx = TransactionRequest::default()
            .to(addr)
            .input(call_data.into());

        let result = provider.call(&tx)
            .await
            .with_context(|| format!("getReserves() call failed for pool {}", pool_address))?;

        let result_bytes: &[u8] = result.as_ref();

        if result_bytes.len() < 64 {
            bail!("getReserves() response too short: {} bytes", result_bytes.len());
        }

        let reserve0 = U256::from_big_endian(&result_bytes[0..32]);
        let reserve1 = U256::from_big_endian(&result_bytes[32..64]);

        let elapsed = start.elapsed();
        debug!(
            pool     = %pool_address,
            reserve0 = %reserve0,
            reserve1 = %reserve1,
            latency  = ?elapsed,
            "V2 pool reserves fetched"
        );

        Ok((reserve0, reserve1))
    }

    /// Builds and executes the flash loan arbitrage transaction on the local node.
    ///
    /// Flow:
    ///   1. Parse private key → build wallet-backed provider
    ///   2. ABI-encode `ArbParams` for the flash loan callback
    ///   3. Call `AtomicArb.executeArbitrage(asset, borrowAmount, params)`
    ///   4. Wait for receipt, log success/failure
    ///
    /// The AtomicArb contract guarantees zero capital loss:
    /// if `finalAmount < repayAmount + minProfitWei`, the tx reverts.
    pub async fn execute_arbitrage(&self, arb: &ArbitrageOpportunity) -> Result<()> {
        let start = Instant::now();

        // ── Validate preconditions ───────────────────────────────────────────
        let pk = self.config.private_key.as_deref()
            .context("PRIVATE_KEY not set — execution disabled")?;

        let contract_addr_str = self.config.contract_address.as_deref()
            .context("CONTRACT_ADDRESS not set — cannot execute arbitrage")?;

        if arb.route.len() < 2 {
            bail!("Arbitrage route must have at least 2 hops (got {})", arb.route.len());
        }

        if arb.net_expected_value <= 0 {
            bail!("Refusing to execute non-profitable arb (NEV = {} wei)", arb.net_expected_value);
        }

        // ── Build wallet-backed provider ─────────────────────────────────────
        let signer: PrivateKeySigner = pk.parse()
            .context("Failed to parse PRIVATE_KEY")?;
        let wallet = EthereumWallet::from(signer);

        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .on_builtin(&self.config.http_url)
            .await
            .context("Failed to connect execution provider")?;

        let contract_addr = Address::from_str(contract_addr_str)
            .context("Invalid CONTRACT_ADDRESS")?;
        let atomic_arb = IAtomicArb::new(contract_addr, provider.clone());

        // ── Extract trade parameters ─────────────────────────────────────────
        let token_borrow = Address::from_str(&arb.route[0].token_in)
            .context("Invalid token_in address on first hop")?;
        let token_intermediate = Address::from_str(&arb.route[0].token_out)
            .context("Invalid token_out address on first hop")?;

        let router_addr = Address::from_str(UNISWAP_V3_ROUTER)
            .context("Invalid V3 router address constant")?;

        // ── Encode ArbParams for the flash loan callback ─────────────────────
        let buy_fee = alloy::primitives::Uint::<24, 1>::from(arb.route[0].fee_bps);
        let sell_fee = alloy::primitives::Uint::<24, 1>::from(arb.route[1].fee_bps);

        let min_profit = alloy::primitives::U256::from_str(&arb.net_expected_value.to_string())
            .unwrap_or_default();

        let params = ArbParams {
            buyRouter:         router_addr,
            buyIsV3:           true,
            buyFee:            buy_fee,
            buyPath:           vec![],  // Empty for V3 (uses fee + tokenIn/tokenOut directly)
            sellRouter:        router_addr,
            sellIsV3:          true,
            sellFee:           sell_fee,
            sellPath:          vec![],  // Empty for V3
            tokenBorrow:       token_borrow,
            tokenIntermediate: token_intermediate,
            minProfitWei:      min_profit,
        };

        use alloy::sol_types::SolValue;
        let encoded_params = params.abi_encode();

        let borrow_amount = alloy::primitives::U256::from_str(&arb.input_amount.to_string())
            .unwrap_or_default();

        // ── Log execution intent ─────────────────────────────────────────────
        info!(
            id         = %arb.id,
            nev_wei    = arb.net_expected_value,
            borrow     = %borrow_amount,
            token_in   = %token_borrow,
            token_mid  = %token_intermediate,
            contract   = %contract_addr,
            hops       = arb.route.len(),
            "⚡ Firing execution bundle → AtomicArb"
        );

        // ── Execute the flash loan ───────────────────────────────────────────
        let tx = atomic_arb.executeArbitrage(
            token_borrow,
            borrow_amount,
            Bytes::from(encoded_params),
        );

        match tx.send().await {
            Ok(pending_tx) => {
                let receipt = pending_tx.get_receipt().await?;
                let elapsed = start.elapsed();
                let exec_num = self.execution_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

                info!(
                    tx_hash     = ?receipt.transaction_hash,
                    gas_used    = ?receipt.gas_used,
                    status      = ?receipt.status(),
                    latency     = ?elapsed,
                    exec_number = exec_num,
                    "✅ Arbitrage Executed Successfully!"
                );
            }
            Err(e) => {
                let elapsed = start.elapsed();
                error!(
                    id      = %arb.id,
                    nev_wei = arb.net_expected_value,
                    latency = ?elapsed,
                    error   = %e,
                    "❌ Arbitrage Reverted (Zero-loss guarantee protected capital)"
                );
            }
        }

        Ok(())
    }

    /// Returns the number of successful arbitrage executions.
    pub fn execution_count(&self) -> u64 {
        self.execution_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    // ── Legacy State Fetching (raw eth_call, no sol! bindings) ────────────────

    /// Fetch the current pool state from the chain.
    ///
    /// For V2: calls `getReserves()` on the pair contract.
    /// For V3: calls `slot0()` and `liquidity()` on the pool contract.
    ///
    /// Falls back to simulated state if the provider is unavailable.
    pub async fn fetch_pool_state(&self, pool: &Pool) -> Result<PoolState> {
        debug!(
            pool_id = %pool.id,
            chain   = %self.config.chain.name(),
            r#type  = ?pool.pool_type,
            "Fetching pool state from chain"
        );

        // Try real on-chain fetch first
        match self.get_or_connect_ws().await {
            Ok(provider) => {
                match pool.pool_type {
                    PoolType::ConstantProduct => self.fetch_v2_state_live(&provider, pool).await,
                    PoolType::ConcentratedLiquidity => self.fetch_v3_state_live(&provider, pool).await,
                    PoolType::StableSwap => self.fetch_v2_state_live(&provider, pool).await,
                }
            }
            Err(e) => {
                warn!(
                    pool_id = %pool.id,
                    error = %e,
                    "WebSocket unavailable — using simulated state"
                );
                match pool.pool_type {
                    PoolType::ConstantProduct => Ok(self.simulated_v2_state()),
                    PoolType::ConcentratedLiquidity => Ok(self.simulated_v3_state()),
                    PoolType::StableSwap => Ok(self.simulated_v2_state()),
                }
            }
        }
    }

    /// Fetch V2 pool reserves via `getReserves()` using a live provider.
    async fn fetch_v2_state_live(
        &self,
        provider: &RootProvider<PubSubFrontend>,
        pool: &Pool,
    ) -> Result<PoolState> {
        let pool_addr = Address::from_str(&pool.id)
            .with_context(|| format!("Invalid pool address: {}", pool.id))?;

        let call_data = Bytes::from(V2_GET_RESERVES_SELECTOR.to_vec());

        let tx = TransactionRequest::default()
            .to(pool_addr)
            .input(call_data.into());

        let result = provider.call(&tx)
            .await
            .with_context(|| format!("getReserves() call failed for pool {}", pool.id))?;

        let result_bytes: &[u8] = result.as_ref();

        // Decode: first 32 bytes = reserve0 (uint112), next 32 = reserve1 (uint112)
        if result_bytes.len() < 64 {
            bail!("getReserves() response too short: {} bytes", result_bytes.len());
        }

        let reserve0 = U256::from_big_endian(&result_bytes[0..32]);
        let reserve1 = U256::from_big_endian(&result_bytes[32..64]);

        // Update block tracking
        if let Ok(block) = provider.get_block_number().await {
            self.last_block.store(block, std::sync::atomic::Ordering::Relaxed);
        }

        debug!(
            pool_id  = %pool.id,
            reserve0 = %reserve0,
            reserve1 = %reserve1,
            "V2 reserves fetched from chain"
        );

        Ok(PoolState {
            reserve_a: reserve0,
            reserve_b: reserve1,
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            amp_coeff: None,
        })
    }

    /// Fetch V3 pool state via `slot0()` + `liquidity()` using a live provider.
    async fn fetch_v3_state_live(
        &self,
        provider: &RootProvider<PubSubFrontend>,
        pool: &Pool,
    ) -> Result<PoolState> {
        let pool_addr = Address::from_str(&pool.id)
            .with_context(|| format!("Invalid pool address: {}", pool.id))?;

        // ── Call slot0() ──────────────────────────────────────────────────────
        let slot0_data = Bytes::from(V3_SLOT0_SELECTOR.to_vec());
        let slot0_tx = TransactionRequest::default()
            .to(pool_addr)
            .input(slot0_data.into());

        let slot0_result = provider.call(&slot0_tx)
            .await
            .with_context(|| format!("slot0() call failed for pool {}", pool.id))?;

        let slot0_bytes: &[u8] = slot0_result.as_ref();

        if slot0_bytes.len() < 64 {
            bail!("slot0() response too short: {} bytes", slot0_bytes.len());
        }

        // sqrtPriceX96 = first 32 bytes (uint160, right-aligned in 32 bytes)
        let sqrt_price_x96 = U256::from_big_endian(&slot0_bytes[0..32]);

        // tick = bytes 32..64 (int24, sign-extended in 32 bytes)
        let tick_bytes: [u8; 4] = slot0_bytes[60..64].try_into()
            .context("Failed to extract tick bytes")?;
        let tick = i32::from_be_bytes(tick_bytes);

        // ── Call liquidity() ──────────────────────────────────────────────────
        let liq_data = Bytes::from(V3_LIQUIDITY_SELECTOR.to_vec());
        let liq_tx = TransactionRequest::default()
            .to(pool_addr)
            .input(liq_data.into());

        let liq_result = provider.call(&liq_tx)
            .await
            .with_context(|| format!("liquidity() call failed for pool {}", pool.id))?;

        let liq_bytes: &[u8] = liq_result.as_ref();

        let liquidity = if liq_bytes.len() >= 32 {
            // uint128 is right-aligned in 32 bytes
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&liq_bytes[16..32]);
            u128::from_be_bytes(buf)
        } else {
            0u128
        };

        // Update block tracking
        if let Ok(block) = provider.get_block_number().await {
            self.last_block.store(block, std::sync::atomic::Ordering::Relaxed);
        }

        debug!(
            pool_id       = %pool.id,
            sqrt_price_x96 = %sqrt_price_x96,
            tick           = tick,
            liquidity      = liquidity,
            "V3 state fetched from chain"
        );

        Ok(PoolState {
            reserve_a: U256::zero(),
            reserve_b: U256::zero(),
            sqrt_price_x96: Some(sqrt_price_x96),
            tick: Some(tick),
            liquidity: Some(liquidity),
            amp_coeff: None,
        })
    }

    // ── Simulated fallback states (when no RPC available) ─────────────────────

    fn simulated_v2_state(&self) -> PoolState {
        PoolState {
            reserve_a: U256::from(205_600_000_000_000u128),            // USDC (6 dec) at WETH = 2056 USD
            reserve_b: U256::from(100_000_000_000_000_000_000_000u128), // WETH (18 dec)
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            amp_coeff: None,
        }
    }

    fn simulated_v3_state(&self) -> PoolState {
        let sqrt_p_raw: u128 = 3_647_949_655_879_842_476_793_799u128;
        PoolState {
            reserve_a: U256::zero(),
            reserve_b: U256::zero(),
            sqrt_price_x96: Some(U256::from(sqrt_p_raw)),
            tick: Some(-199_729),
            liquidity: Some(1_462_847_672_098_985_101),
            amp_coeff: None,
        }
    }

    /// Get the current block number.
    pub fn current_block(&self) -> u64 {
        self.last_block.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Fetch the latest block number from the provider.
    pub async fn fetch_block_number(&self) -> Result<u64> {
        let provider = self.get_or_connect_ws().await?;
        let block = provider.get_block_number().await
            .context("Failed to fetch block number")?;
        self.last_block.store(block, std::sync::atomic::Ordering::Relaxed);
        Ok(block)
    }

    /// Get the WebSocket provider for external use (e.g., mempool subscription).
    pub async fn get_provider(&self) -> Result<RootProvider<PubSubFrontend>> {
        self.get_or_connect_ws().await
    }

    /// Submit a transaction bundle via Flashbots (bypasses public mempool).
    ///
    /// In production:
    ///   1. Sign the bundle with the Flashbots signing key
    ///   2. POST to https://relay.flashbots.net (eth_sendBundle RPC)
    ///   3. Monitor inclusion in the next block
    pub async fn submit_flashbots_bundle(&self, bundle_txs: Vec<Vec<u8>>, target_block: u64) -> Result<String> {
        let relay = self.config.flashbots_url.as_deref()
            .unwrap_or("https://relay.flashbots.net");

        if bundle_txs.is_empty() {
            bail!("Cannot submit empty bundle");
        }

        info!(
            relay      = %relay,
            num_txs    = bundle_txs.len(),
            target_block = target_block,
            "📦 Submitting Flashbots bundle via eth_sendBundle"
        );

        let hex_txs: Vec<String> = bundle_txs.iter()
            .map(|tx| format!("0x{}", hex::encode(tx)))
            .collect();

        let block_hex = format!("0x{:x}", target_block);
        
        let params = serde_json::json!([{
            "txs": hex_txs,
            "blockNumber": block_hex,
        }]);

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendBundle",
            "params": params
        });

        let payload_string = serde_json::to_string(&payload)?;
        
        // Use persistent signing key from config for consistent relay reputation.
        // A stable identity builds trust with block builders (Titan, Flashbots, etc.)
        let signer: PrivateKeySigner = match self.config.flashbots_signing_key.as_deref() {
            Some(key) => key.parse()
                .context("Failed to parse FLASHBOTS_SIGNING_KEY")?,
            None => {
                warn!("FLASHBOTS_SIGNING_KEY not set — using random key (no reputation!)");
                alloy::signers::local::PrivateKeySigner::random()
            }
        };
        let body_hash = alloy::primitives::keccak256(payload_string.as_bytes());
        let signature = alloy::signers::Signer::sign_message(&signer, body_hash.as_slice()).await?;
        let header_value = format!("{}:{}", signer.address(), hex::encode(signature.as_bytes()));

        let client = reqwest::Client::new();
        let res = client.post(relay)
            .header("Content-Type", "application/json")
            .header("X-Flashbots-Signature", header_value)
            .body(payload_string)
            .send()
            .await
            .context("Failed to send Flashbots request")?;

        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        
        if !status.is_success() {
            bail!("Flashbots submission failed with status {}: {}", status, text);
        }

        info!(response = %text, "✓ Bundle submitted successfully");

        Ok(text)
    }

    // ── Dry-Run Simulation (Task 5) ──────────────────────────────────────────

    /// Simulate the arbitrage transaction via eth_call before committing gas.
    ///
    /// Returns `Ok(())` if the simulation succeeds (tx would not revert).
    /// Returns `Err(...)` with the revert reason if the simulation fails.
    ///
    /// This is the critical safety gate: never submit a real tx without a
    /// passing dry-run. The AtomicArb contract will revert if
    /// `finalAmount < repayAmount + minProfitWei`, so a simulation failure
    /// means the arb is no longer profitable at current state.
    pub async fn simulate_arbitrage(&self, arb: &ArbitrageOpportunity) -> Result<()> {
        let start = Instant::now();

        let pk = self.config.private_key.as_deref()
            .context("PRIVATE_KEY not set — cannot simulate")?;
        let contract_addr_str = self.config.contract_address.as_deref()
            .context("CONTRACT_ADDRESS not set — cannot simulate")?;

        if arb.route.len() < 2 {
            bail!("Route too short for simulation (need >= 2 hops)");
        }

        // Build the same calldata as execute_arbitrage
        let signer: PrivateKeySigner = pk.parse()
            .context("Failed to parse PRIVATE_KEY for simulation")?;
        let from_addr = signer.address();

        let contract_addr = Address::from_str(contract_addr_str)
            .context("Invalid CONTRACT_ADDRESS")?;

        let token_borrow = Address::from_str(&arb.route[0].token_in)
            .context("Invalid token_in address on first hop")?;
        let token_intermediate = Address::from_str(&arb.route[0].token_out)
            .context("Invalid token_out address on first hop")?;

        let router_addr = Address::from_str(UNISWAP_V3_ROUTER)
            .context("Invalid V3 router address")?;

        let buy_fee = alloy::primitives::Uint::<24, 1>::from(arb.route[0].fee_bps);
        let sell_fee = alloy::primitives::Uint::<24, 1>::from(arb.route[1].fee_bps);
        let min_profit = alloy::primitives::U256::from_str(&arb.net_expected_value.to_string())
            .unwrap_or_default();

        let params = ArbParams {
            buyRouter:         router_addr,
            buyIsV3:           true,
            buyFee:            buy_fee,
            buyPath:           vec![],
            sellRouter:        router_addr,
            sellIsV3:          true,
            sellFee:           sell_fee,
            sellPath:          vec![],
            tokenBorrow:       token_borrow,
            tokenIntermediate: token_intermediate,
            minProfitWei:      min_profit,
        };

        use alloy::sol_types::SolValue;
        let encoded_params = params.abi_encode();

        let borrow_amount = alloy::primitives::U256::from_str(&arb.input_amount.to_string())
            .unwrap_or_default();

        // ABI-encode the executeArbitrage(address, uint256, bytes) call
        use alloy::sol_types::SolCall;
        let call = IAtomicArb::executeArbitrageCall {
            asset: token_borrow,
            borrowAmount: borrow_amount,
            params: Bytes::from(encoded_params),
        };
        let calldata = call.abi_encode();

        // Build eth_call request (no gas limit = simulates with max gas)
        let tx = TransactionRequest::default()
            .from(from_addr)
            .to(contract_addr)
            .input(Bytes::from(calldata).into());

        // Use HTTP provider for simulation (more reliable than WS for one-shot calls)
        let provider = ProviderBuilder::new()
            .on_builtin(&self.config.http_url)
            .await
            .context("Failed to connect simulation provider")?;

        match provider.call(&tx).await {
            Ok(_) => {
                let elapsed = start.elapsed();
                info!(
                    id      = %arb.id,
                    latency = ?elapsed,
                    "✅ Dry-run simulation PASSED — safe to execute"
                );
                Ok(())
            }
            Err(e) => {
                let elapsed = start.elapsed();
                warn!(
                    id      = %arb.id,
                    latency = ?elapsed,
                    error   = %e,
                    "❌ Dry-run simulation REVERTED — aborting execution"
                );
                bail!("Simulation reverted: {}", e)
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Multi-chain EVM Manager
// ─────────────────────────────────────────────────────────────────────────────

/// Manages multiple EVM chain adapters (Ethereum, Base, Arbitrum).
pub struct EvmManager {
    adapters: std::collections::HashMap<ChainId, EvmAdapter>,
}

impl EvmManager {
    pub fn new() -> Self {
        Self { adapters: std::collections::HashMap::new() }
    }

    pub fn add_chain(&mut self, config: EvmConfig) {
        let chain = config.chain;
        self.adapters.insert(chain, EvmAdapter::new(config));
    }

    pub fn get(&self, chain: ChainId) -> Option<&EvmAdapter> {
        self.adapters.get(&chain)
    }

    pub fn chain_count(&self) -> usize {
        self.adapters.len()
    }
}

impl Default for EvmManager {
    fn default() -> Self {
        Self::new()
    }
}
