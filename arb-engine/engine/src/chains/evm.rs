// ─────────────────────────────────────────────────────────────────────────────
//  chains/evm.rs  [PATCHED]
//
//  FIXES applied vs. original:
//
//  FIX-1: Fee unit mismatch.
//    Internal `fee_bps` (e.g., 30 for 0.30%) was passed directly as the
//    Uniswap V3 `uint24 fee` parameter.  Uniswap V3 expects per-million units
//    (3000 for 0.30%).  All `ArbParams` construction sites now multiply by 100.
//
//  FIX-2: Chain-aware, per-leg router addresses.
//    The hardcoded constant `UNISWAP_V3_ROUTER` pointed at the Ethereum mainnet
//    SwapRouter V1 address and was used for BOTH legs unconditionally.
//    Both execute_arbitrage() and simulate_arbitrage() now:
//      a) Resolve the correct SwapRouter02 address for the active chain.
//      b) Extract the per-leg router from arb.route[n].dex so cross-DEX
//         opportunities (e.g., Aerodrome → Uniswap) use the correct routers.
//
//  FIX-6: Cached HTTP provider.
//    get_v3_pool_state() and get_v2_pool_state() previously created a brand-new
//    reqwest/hyper connection pool on every call, exhausting RPC rate limits.
//    An `http_provider` field (RwLock<Option<...>>) is now cached and reused,
//    matching the existing pattern for `ws_provider`.
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
use tracing::{debug, info, warn};

use crate::arb::opportunity::ArbitrageOpportunity;
use crate::pool::{ChainId, Pool, PoolState, PoolType, U256};

// ── Alloy Contract Bindings ──────────────────────────────────────────────────
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
        function aavePool() external view returns (address);
        
        function executeArbitrage(
            address asset,
            uint256 borrowAmount,
            bytes calldata params,
            uint256 deadline
        ) external;
    }

    #[sol(rpc)]
    interface IAavePool {
        function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);
    }

    // [C-3] Updated to include per-leg expected outputs for slippage protection
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
        uint256 expectedBuyOut;   // expected output from buy leg (0 = use global slippageBps)
        uint256 expectedSellOut;  // expected output from sell leg (0 = use global slippageBps)
    }
}

// ── Function selectors ────────────────────────────────────────────────────────
const V2_GET_RESERVES_SELECTOR: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];
const V3_SLOT0_SELECTOR:        [u8; 4] = [0x38, 0x50, 0xc7, 0xbd];
const V3_LIQUIDITY_SELECTOR:    [u8; 4] = [0x1a, 0x68, 0x65, 0x02];

// ── FIX-2: Chain-aware router table ──────────────────────────────────────────
//
// The original code used a single hardcoded constant for all chains:
//   const UNISWAP_V3_ROUTER: &str = "0xE592427...";  // Ethereum mainnet V1 ONLY
//
// Correct SwapRouter02 addresses by chain:
const SWAP_ROUTER_02_ETHEREUM: &str = "0x68b3465833fb72A70eCDF485E0e4C7bD8665Fc45";
const SWAP_ROUTER_02_BASE:     &str = "0x2626664c2603336E57B271c5C0b26F421741e481";
const SWAP_ROUTER_02_ARBITRUM: &str = "0x68b3465833fb72A70eCDF485E0e4C7bD8665Fc45";

// Aerodrome V2 Router on Base (Solidly-compatible, UniV2 interface)
const AERODROME_V2_ROUTER:      &str = "0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43";
// Aerodrome router on Base (for Aerodrome V3 / Universal Router swaps)
const AERODROME_UNIVERSAL_ROUTER: &str = "0x6Cb442acF35158D5eDa88fe602221b67B400Be3E";

// ─────────────────────────────────────────────────────────────────────────────
//  EVM Adapter
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EvmConfig {
    pub chain:                ChainId,
    pub ws_url:               String,
    pub http_url:             String,
    pub flashbots_url:        Option<String>,
    pub contract_address:     Option<String>,
    pub private_key:          Option<String>,
    pub flashbots_signing_key: Option<String>,
    pub private_rpc_url:      Option<String>,
}

pub struct EvmAdapter {
    config:           EvmConfig,
    ws_provider:      tokio::sync::RwLock<Option<RootProvider<PubSubFrontend>>>,
    // FIX-6: cached HTTP provider — avoids creating a new connection on every call
    http_provider:    tokio::sync::RwLock<Option<alloy::providers::RootProvider<
                          alloy::transports::BoxTransport
                      >>>,
    last_block:       std::sync::atomic::AtomicU64,
    execution_count:  std::sync::atomic::AtomicU64,
}

impl EvmAdapter {
    pub fn new(config: EvmConfig) -> Self {
        info!(
            chain           = %config.chain.name(),
            ws_url          = %config.ws_url,
            has_contract    = config.contract_address.is_some(),
            has_private_key = config.private_key.is_some(),
            "Initializing EVM adapter"
        );
        Self {
            config,
            ws_provider:  tokio::sync::RwLock::new(None),
            http_provider: tokio::sync::RwLock::new(None),
            last_block:    std::sync::atomic::AtomicU64::new(0),
            execution_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn chain(&self) -> ChainId {
        self.config.chain
    }

    // ── FIX-2: chain-aware router resolution ─────────────────────────────────

    /// Return the correct SwapRouter02 address for the active chain.
    fn default_v3_router(&self) -> &'static str {
        match self.config.chain {
            ChainId::Base     => SWAP_ROUTER_02_BASE,
            ChainId::Arbitrum => SWAP_ROUTER_02_ARBITRUM,
            _                 => SWAP_ROUTER_02_ETHEREUM,
        }
    }

    /// Resolve the router address for a specific swap leg.
    ///
    /// Prefers the DEX string encoded in the route step so cross-DEX arbs
    /// (e.g., Aerodrome buy → Uniswap sell) use the correct router per leg.
    fn resolve_router_for_dex(&self, dex: &str) -> &'static str {
        let lower = dex.to_lowercase();
        match self.config.chain {
            ChainId::Base => {
                if lower.contains("uniswap v2") || lower.contains("aerodrome v2") || lower.contains("v2") {
                    AERODROME_V2_ROUTER
                } else if lower.contains("aerodrome") || lower.contains("universal") {
                    AERODROME_UNIVERSAL_ROUTER
                } else {
                    SWAP_ROUTER_02_BASE
                }
            }
            ChainId::Arbitrum => {
                if lower.contains("v2") || lower.contains("sushiswap") {
                    // Sushiswap V2 or Uni V2 on Arbitrum
                    "0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506" // SushiSwap Router
                } else {
                    SWAP_ROUTER_02_ARBITRUM
                }
            }
            _ => { // Ethereum / Default
                if lower.contains("v2") || lower.contains("sushiswap") {
                    "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D" // Uniswap V2 Router
                } else {
                    SWAP_ROUTER_02_ETHEREUM
                }
            }
        }
    }

    // ── WebSocket provider (lazy, cached) ────────────────────────────────────

    async fn get_or_connect_ws(&self) -> Result<RootProvider<PubSubFrontend>> {
        {
            let guard = self.ws_provider.read().await;
            if let Some(ref p) = *guard {
                return Ok(p.clone());
            }
        }

        let ws = WsConnect::new(&self.config.ws_url);
        let provider = ProviderBuilder::new()
            .on_ws(ws)
            .await
            .with_context(|| format!(
                "Failed to connect WebSocket to {} (chain {})",
                self.config.ws_url, self.config.chain.name()
            ))?;

        info!(chain = %self.config.chain.name(), "WebSocket provider connected");

        let mut guard = self.ws_provider.write().await;
        *guard = Some(provider.clone());
        Ok(provider)
    }

    // ── FIX-6: HTTP provider (lazy, cached) ──────────────────────────────────

    /// Return a cached HTTP provider.  Creates one on first call; reuses it
    /// on all subsequent calls so we don't open a new TCP connection per fetch.
    async fn get_or_connect_http(&self)
        -> Result<alloy::providers::RootProvider<
                alloy::transports::BoxTransport
           >>
    {
        {
            let guard = self.http_provider.read().await;
            if let Some(ref p) = *guard {
                return Ok(p.clone());
            }
        }

        let provider = ProviderBuilder::new()
            .on_builtin(&self.config.http_url)
            .await
            .with_context(|| format!(
                "HTTP provider connection failed for {}",
                self.config.http_url
            ))?;

        info!(chain = %self.config.chain.name(), "HTTP provider connected (cached)");

        let mut guard = self.http_provider.write().await;
        *guard = Some(provider.clone());
        Ok(provider)
    }

    /// Dynamically query the Aave FLASHLOAN_PREMIUM_TOTAL via the AtomicArb contract.
    pub async fn get_aave_premium(&self) -> Result<u32> {
        let contract_addr_str = self.config.contract_address.as_deref()
            .context("CONTRACT_ADDRESS not set")?;
        let contract_addr = Address::from_str(contract_addr_str)?;
        let provider = self.get_or_connect_http().await?;
        
        let atomic_arb = IAtomicArb::new(contract_addr, provider.clone());
        let aave_pool_addr = atomic_arb.aavePool().call().await?._0;
        let aave_pool = IAavePool::new(aave_pool_addr, provider);
        
        let premium = aave_pool.FLASHLOAN_PREMIUM_TOTAL().call().await?._0;
        Ok(premium as u32)
    }

    /// Query Aave FLASHLOAN_PREMIUM_TOTAL directly bypassing our contract.
    pub async fn get_aave_premium_direct(&self, pool_address: &str) -> Result<u32> {
        let pool_addr = Address::from_str(pool_address)
            .with_context(|| format!("Invalid Aave pool address: {}", pool_address))?;
        let provider = self.get_or_connect_http().await?;
        
        let aave_pool = IAavePool::new(pool_addr, provider);
        
        let premium = aave_pool.FLASHLOAN_PREMIUM_TOTAL().call().await?._0;
        Ok(premium as u32)
    }

    /// Fetch the actual flash loan fee from the Aave V3 pool.
    /// Returns fee in basis points (1 BPS = 0.01%).
    pub async fn get_flash_loan_fee_bps(&self, pool_address: &str) -> Result<u32> {
        let pool_addr: Address = pool_address.parse()
            .context("Invalid Aave pool address")?;
        let provider = self.get_or_connect_http().await?;
        
        let contract = IAavePool::new(pool_addr, provider);
        
        let premium_total = contract.FLASHLOAN_PREMIUM_TOTAL()
            .call()
            .await
            .context("Failed to call FLASHLOAN_PREMIUM_TOTAL")?
            ._0;

        // FLASHLOAN_PREMIUM_TOTAL returns the fee in basis points (100 = 1%)
        Ok(premium_total as u32)
    }

    // ── Live Pool State Fetching ─────────────────────────────────────────────

    /// FIX-6: uses the cached HTTP provider instead of creating a new one.
    pub async fn get_v3_pool_state(&self, pool_address: &str) -> Result<(U256, i32, u128)> {
        let start = Instant::now();
        let provider = self.get_or_connect_http().await?;

        let addr = Address::from_str(pool_address)
            .with_context(|| format!("Invalid pool address: {}", pool_address))?;

        let pool = IUniswapV3Pool::new(addr, provider);

        let slot0 = pool.slot0().call().await
            .map_err(|e| anyhow::anyhow!("slot0() RPC error for {}: {:?}", pool_address, e))?;
        let liquidity = pool.liquidity().call().await
            .map_err(|e| anyhow::anyhow!("liquidity() RPC error for {}: {:?}", pool_address, e))?;

        let sqrt_price = U256::from_dec_str(&slot0.sqrtPriceX96.to_string())
            .unwrap_or(U256::zero());
        let tick: i32 = slot0.tick.to_string().parse().unwrap_or(0);

        debug!(
            pool = %pool_address, sqrtP = %sqrt_price, tick, liq = liquidity._0,
            latency = ?start.elapsed(), "V3 pool state fetched (cached HTTP)"
        );

        Ok((sqrt_price, tick, liquidity._0))
    }

    /// FIX-6: uses the cached HTTP provider.
    pub async fn get_v2_pool_state(&self, pool_address: &str) -> Result<(U256, U256)> {
        let start = Instant::now();
        let provider = self.get_or_connect_http().await?;

        let addr = Address::from_str(pool_address)
            .with_context(|| format!("Invalid pool address: {}", pool_address))?;

        let call_data = Bytes::from(V2_GET_RESERVES_SELECTOR.to_vec());
        let tx = TransactionRequest::default().to(addr).input(call_data.into());

        let result = provider.call(&tx).await
            .with_context(|| format!("getReserves() call failed for pool {}", pool_address))?;

        let result_bytes: &[u8] = result.as_ref();
        if result_bytes.len() < 64 {
            bail!("getReserves() response too short: {} bytes", result_bytes.len());
        }

        let reserve0 = U256::from_big_endian(&result_bytes[0..32]);
        let reserve1 = U256::from_big_endian(&result_bytes[32..64]);

        debug!(
            pool = %pool_address, reserve0 = %reserve0, reserve1 = %reserve1,
            latency = ?start.elapsed(), "V2 reserves fetched (cached HTTP)"
        );

        Ok((reserve0, reserve1))
    }

    // ── ArbParams builder helper ──────────────────────────────────────────────

    /// Construct the `ArbParams` struct for a two-leg arbitrage opportunity.
    ///
    /// FIX-1: `fee_bps` values are multiplied by 100 to produce Uniswap V3
    ///        per-million fee units (e.g. 30 bps → 3000 fee units).
    /// FIX-2: Router addresses are resolved per-leg and per-chain.
    fn build_arb_params(&self, arb: &ArbitrageOpportunity) -> Result<ArbParams> {
        if arb.route.len() < 2 {
            bail!("Arbitrage route must have at least 2 hops (got {})", arb.route.len());
        }

        let token_borrow = Address::from_str(&arb.route[0].token_in)
            .context("Invalid token_in address on first hop")?;
        let token_intermediate = Address::from_str(&arb.route[0].token_out)
            .context("Invalid token_out address on first hop")?;

        // FIX-2: per-leg router resolution
        let buy_router_str  = self.resolve_router_for_dex(&arb.route[0].dex);
        let sell_router_str = self.resolve_router_for_dex(&arb.route[1].dex);
        let buy_router  = Address::from_str(buy_router_str).context("Invalid buy router address")?;
        let sell_router = Address::from_str(sell_router_str).context("Invalid sell router address")?;

        // Determine if each leg is V3 or V2
        let buy_is_v3 = {
            let lower = arb.route[0].dex.to_lowercase();
            lower.contains("v3") || lower.contains("concentrated") || lower.contains("universal")
        };
        let sell_is_v3 = {
            let lower = arb.route[1].dex.to_lowercase();
            lower.contains("v3") || lower.contains("concentrated") || lower.contains("universal")
        };

        // Normalize fee units: handle both mempool basis points (e.g. 5, 30)
        // and main.rs initialized Uniswap V3 units (e.g. 500, 3000)
        let normalize_fee = |fee: u32, is_v3: bool| -> u32 {
            if !is_v3 {
                return 0;
            }
            match fee {
                1 => 100,
                5 => 500,
                30 => 3000,
                100 => 100,   // 0.01% V3 tier
                500 => 500,   // 0.05% V3 tier
                3000 => 3000, // 0.30% V3 tier
                10000 => 10000, // 1.00% V3 tier
                _ => {
                    if fee < 100 {
                        fee * 100
                    } else {
                        fee
                    }
                }
            }
        };

        let buy_fee_units  = normalize_fee(arb.route[0].fee_bps, buy_is_v3);
        let sell_fee_units = normalize_fee(arb.route[1].fee_bps, sell_is_v3);
        let buy_fee  = alloy::primitives::Uint::<24, 1>::from(buy_fee_units);
        let sell_fee = alloy::primitives::Uint::<24, 1>::from(sell_fee_units);

        let buy_path = if !buy_is_v3 {
            vec![token_borrow, token_intermediate]
        } else {
            vec![]
        };

        let sell_path = if !sell_is_v3 {
            vec![token_intermediate, token_borrow]
        } else {
            vec![]
        };

        let min_profit = alloy::primitives::U256::from_str(&arb.net_expected_value.to_string())
            .unwrap_or_default();

        // [C-3] Calculate expected outputs for per-leg slippage protection
        let expected_buy_out = if arb.route.len() > 0 {
            alloy::primitives::U256::from_str(&arb.route[0].expected_amount_out.to_string())
                .unwrap_or_default()
        } else {
            alloy::primitives::U256::ZERO
        };
        let expected_sell_out = if arb.route.len() > 1 {
            alloy::primitives::U256::from_str(&arb.route[1].expected_amount_out.to_string())
                .unwrap_or_default()
        } else {
            alloy::primitives::U256::ZERO
        };

        Ok(ArbParams {
            buyRouter:         buy_router,
            buyIsV3:           buy_is_v3,
            buyFee:            buy_fee,
            buyPath:           buy_path,
            sellRouter:        sell_router,
            sellIsV3:          sell_is_v3,
            sellFee:           sell_fee,
            sellPath:          sell_path,
            tokenBorrow:       token_borrow,
            tokenIntermediate: token_intermediate,
            minProfitWei:      min_profit,
            expectedBuyOut:    expected_buy_out,
            expectedSellOut:   expected_sell_out,
        })
    }

    // ── Execute Arbitrage ────────────────────────────────────────────────────

    pub async fn execute_arbitrage(&self, arb: &ArbitrageOpportunity) -> Result<()> {
        let start = Instant::now();

        let pk = self.config.private_key.as_deref()
            .context("PRIVATE_KEY not set — execution disabled")?;
        let contract_addr_str = self.config.contract_address.as_deref()
            .context("CONTRACT_ADDRESS not set — cannot execute arbitrage")?;

        if arb.net_expected_value <= 0 {
            bail!("Refusing to execute non-profitable arb (NEV = {} wei)", arb.net_expected_value);
        }

        let signer: PrivateKeySigner = pk.parse().context("Failed to parse PRIVATE_KEY")?;
        let wallet = EthereumWallet::from(signer.clone());

        let exec_url = self.config.private_rpc_url.as_deref().unwrap_or(&self.config.http_url);
        if self.config.private_rpc_url.is_some() {
            info!(url = %exec_url, "🔒 Utilizing configured Private RPC endpoint for transaction execution (MEV Protection)");
        } else {
            info!("⚡ Utilizing standard HTTP RPC endpoint for transaction execution");
        }

        let provider = ProviderBuilder::new()
            .wallet(wallet.clone())
            .on_builtin(exec_url)
            .await
            .context("Failed to connect execution provider")?;

        let contract_addr = Address::from_str(contract_addr_str)
            .context("Invalid CONTRACT_ADDRESS")?;
        let atomic_arb = IAtomicArb::new(contract_addr, provider.clone());

        // FIX-1 + FIX-2: build params with correct fee units and per-leg routers
        let params = self.build_arb_params(arb)?;

        use alloy::sol_types::SolValue;
        let encoded_params = params.abi_encode();

        let borrow_amount = alloy::primitives::U256::from_str(&arb.input_amount.to_string())
            .unwrap_or_default();

        // Diagnostic log so router choices are always visible
        info!(
            id              = %arb.id,
            nev_wei         = arb.net_expected_value,
            borrow          = %borrow_amount,
            chain           = %self.config.chain.name(),
            buy_router      = %format!("{:?}", params.buyRouter),
            sell_router     = %format!("{:?}", params.sellRouter),
            buy_fee_units   = params.buyFee.to::<u32>(),  // should be 3000, not 30
            sell_fee_units  = params.sellFee.to::<u32>(),
            contract        = %contract_addr,
            hops            = arb.route.len(),
            "⚡ Firing execution bundle → AtomicArb"
        );

        // [L-3] Deadline: current timestamp + 5 minutes (300 seconds)
        let deadline = alloy::primitives::U256::from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() + 300
        );

        let tx = atomic_arb.executeArbitrage(
            params.tokenBorrow,
            borrow_amount,
            Bytes::from(encoded_params),
            deadline,
        );

        let mut tx_req = tx.into_transaction_request().from(signer.address());
        
        // Estimate gas and populate tx parameters
        let nonce = provider.get_transaction_count(signer.address()).await?;
        let gas = provider.estimate_gas(&tx_req).await?;
        let gas_price = provider.get_gas_price().await?;
        
        tx_req.set_nonce(nonce);
        tx_req.set_gas_limit(gas * 12 / 10); // 20% buffer
        tx_req.set_max_fee_per_gas(gas_price);
        tx_req.set_max_priority_fee_per_gas(gas_price);

        use alloy::network::TransactionBuilder;
        let envelope = tx_req.build(&wallet).await.map_err(|e| anyhow::anyhow!("build error: {}", e))?;
        
        // Get RLP encoded signed tx
        use alloy::eips::eip2718::Encodable2718;
        let signed_bytes = envelope.encoded_2718();
        
        let (receipt, receipt_str) = if self.config.chain == ChainId::Base || self.config.chain == ChainId::Arbitrum || self.config.private_rpc_url.is_some() {
            if self.config.private_rpc_url.is_some() {
                info!("🔒 Broadcasting raw transaction via private/MEV-protected RPC relay...");
            } else {
                info!(chain = %self.config.chain.name(), "Directly broadcasting raw transaction to L2 sequencer (private mempool)...");
            }
            let tx_hash = envelope.tx_hash();
            let _pending_tx = provider.send_raw_transaction(&signed_bytes).await
                .context("Failed to send raw transaction")?;
            
            let mut receipt = None;
            for _ in 0..15 { // wait up to 15 seconds
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                if let Ok(Some(r)) = provider.get_transaction_receipt(*tx_hash).await {
                    receipt = Some(r);
                    break;
                }
            }
            let rec = receipt.context("Transaction not included after 15s")?;
            (rec, format!("{:?}", tx_hash))
        } else {
            let target_block = provider.get_block_number().await.unwrap_or(0) + 1;
            let receipt_str = self.submit_flashbots_bundle(vec![signed_bytes], target_block).await?;
            
            let tx_hash = envelope.tx_hash();
            let mut receipt = None;
            for _ in 0..15 { // wait up to 15 seconds
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                if let Ok(Some(r)) = provider.get_transaction_receipt(*tx_hash).await {
                    receipt = Some(r);
                    break;
                }
            }
            let rec = receipt.context("Transaction not included after 15s")?;
            (rec, receipt_str)
        };
        
        if !receipt.status() {
            bail!("Tx Reverted!");
        }
        
        let elapsed = start.elapsed();
        let exec_num = self.execution_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

        info!(
            bundle_receipt = %receipt_str,
            tx_hash     = ?receipt.transaction_hash,
            gas_used    = ?receipt.gas_used,
            latency     = ?elapsed,
            exec_number = exec_num,
            "✅ Arbitrage Transaction executed successfully!"
        );

        Ok(())
    }

    // ── Dry-Run Simulation ────────────────────────────────────────────────────

    pub async fn simulate_arbitrage(&self, arb: &ArbitrageOpportunity) -> Result<()> {
        let start = Instant::now();

        let pk = self.config.private_key.as_deref()
            .context("PRIVATE_KEY not set — cannot simulate")?;
        let contract_addr_str = self.config.contract_address.as_deref()
            .context("CONTRACT_ADDRESS not set — cannot simulate")?;

        if arb.route.len() < 2 {
            bail!("Route too short for simulation (need >= 2 hops)");
        }

        let signer: PrivateKeySigner = pk.parse()
            .context("Failed to parse PRIVATE_KEY for simulation")?;
        let from_addr     = signer.address();
        let contract_addr = Address::from_str(contract_addr_str)
            .context("Invalid CONTRACT_ADDRESS")?;

        // FIX-1 + FIX-2: same corrected params builder used for simulate and execute
        let params = self.build_arb_params(arb)?;

        use alloy::sol_types::SolValue;
        let encoded_params = params.abi_encode();

        let borrow_amount = alloy::primitives::U256::from_str(&arb.input_amount.to_string())
            .unwrap_or_default();

        // [L-3] Deadline for simulation: current timestamp + 5 minutes
        let deadline = alloy::primitives::U256::from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() + 300
        );

        use alloy::sol_types::SolCall;
        let call = IAtomicArb::executeArbitrageCall {
            asset:        params.tokenBorrow,
            borrowAmount: borrow_amount,
            params:       Bytes::from(encoded_params),
            deadline,
        };
        let calldata = call.abi_encode();

        let tx = TransactionRequest::default()
            .from(from_addr)
            .to(contract_addr)
            .input(Bytes::from(calldata).into());

        // FIX-6: use cached HTTP provider
        let provider = self.get_or_connect_http().await
            .context("Failed to connect simulation provider")?;

        let mut attempts = 0;
        let max_attempts = 5;
        let mut delay_ms = 100;

        loop {
            attempts += 1;
            match provider.call(&tx).await {
                Ok(_) => {
                    info!(
                        id = %arb.id, latency = ?start.elapsed(), attempts,
                        buy_router  = %format!("{:?}", params.buyRouter),
                        buy_fee     = params.buyFee.to::<u32>(),
                        sell_fee    = params.sellFee.to::<u32>(),
                        "✅ Dry-run simulation PASSED"
                    );
                    return Ok(());
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if (err_str.contains("429") || err_str.contains("rate") || err_str.contains("capacity") || err_str.contains("limit")) && attempts < max_attempts {
                        warn!(
                            id = %arb.id, attempts, delay = delay_ms, error = %err_str,
                            "⚠️ Simulation rate-limited — retrying with backoff"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                        delay_ms *= 2;
                        continue;
                    }
                    warn!(
                        id = %arb.id, latency = ?start.elapsed(), attempts, error = %e,
                        "❌ Dry-run simulation REVERTED — aborting execution"
                    );
                    bail!("Simulation reverted: {}", e)
                }
            }
        }
    }

    // ── fetch_pool_state (unchanged logic, uses cached HTTP now) ─────────────

    pub async fn fetch_pool_state(&self, pool: &Pool) -> Result<PoolState> {
        debug!(
            pool_id = %pool.id,
            chain   = %self.config.chain.name(),
            r#type  = ?pool.pool_type,
            "Fetching pool state from chain"
        );

        if pool.id.contains(':') {
            debug!(pool_id = %pool.id, "Pool ID is a placeholder — returning simulated state");
            return match pool.pool_type {
                PoolType::ConstantProduct      => Ok(self.simulated_v2_state()),
                PoolType::ConcentratedLiquidity => Ok(self.simulated_v3_state()),
                PoolType::StableSwap           => Ok(self.simulated_v2_state()),
            };
        }

        match self.get_or_connect_ws().await {
            Ok(provider) => {
                match pool.pool_type {
                    PoolType::ConstantProduct      => self.fetch_v2_state_live(&provider, pool).await,
                    PoolType::ConcentratedLiquidity => self.fetch_v3_state_live(&provider, pool).await,
                    PoolType::StableSwap           => self.fetch_v2_state_live(&provider, pool).await,
                }
            }
            Err(e) => {
                warn!(pool_id = %pool.id, error = %e, "WebSocket unavailable — using simulated state");
                match pool.pool_type {
                    PoolType::ConstantProduct      => Ok(self.simulated_v2_state()),
                    PoolType::ConcentratedLiquidity => Ok(self.simulated_v3_state()),
                    PoolType::StableSwap           => Ok(self.simulated_v2_state()),
                }
            }
        }
    }

    async fn fetch_v2_state_live(
        &self,
        provider: &RootProvider<PubSubFrontend>,
        pool: &Pool,
    ) -> Result<PoolState> {
        let pool_addr = Address::from_str(&pool.id)
            .with_context(|| format!("Invalid pool address: {}", pool.id))?;

        let call_data = Bytes::from(V2_GET_RESERVES_SELECTOR.to_vec());
        let tx = TransactionRequest::default().to(pool_addr).input(call_data.into());

        let result = provider.call(&tx).await
            .with_context(|| format!("getReserves() call failed for pool {}", pool.id))?;

        let result_bytes: &[u8] = result.as_ref();
        if result_bytes.len() < 64 {
            bail!("getReserves() response too short: {} bytes", result_bytes.len());
        }

        let reserve0 = U256::from_big_endian(&result_bytes[0..32]);
        let reserve1 = U256::from_big_endian(&result_bytes[32..64]);

        if let Ok(block) = provider.get_block_number().await {
            self.last_block.store(block, std::sync::atomic::Ordering::Relaxed);
        }

        Ok(PoolState {
            reserve_a: reserve0,
            reserve_b: reserve1,
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            amp_coeff: None,
        })
    }

    async fn fetch_v3_state_live(
        &self,
        provider: &RootProvider<PubSubFrontend>,
        pool: &Pool,
    ) -> Result<PoolState> {
        let pool_addr = Address::from_str(&pool.id)
            .with_context(|| format!("Invalid pool address: {}", pool.id))?;

        let slot0_tx = TransactionRequest::default()
            .to(pool_addr)
            .input(Bytes::from(V3_SLOT0_SELECTOR.to_vec()).into());

        let slot0_result = provider.call(&slot0_tx).await
            .with_context(|| format!("slot0() call failed for pool {}", pool.id))?;

        let slot0_bytes: &[u8] = slot0_result.as_ref();
        if slot0_bytes.len() < 64 {
            bail!("slot0() response too short: {} bytes", slot0_bytes.len());
        }

        let sqrt_price_x96 = U256::from_big_endian(&slot0_bytes[0..32]);
        let tick_bytes: [u8; 4] = slot0_bytes[60..64].try_into()
            .context("Failed to extract tick bytes")?;
        let tick = i32::from_be_bytes(tick_bytes);

        let liq_tx = TransactionRequest::default()
            .to(pool_addr)
            .input(Bytes::from(V3_LIQUIDITY_SELECTOR.to_vec()).into());

        let liq_result = provider.call(&liq_tx).await
            .with_context(|| format!("liquidity() call failed for pool {}", pool.id))?;

        let liq_bytes: &[u8] = liq_result.as_ref();
        let liquidity = if liq_bytes.len() >= 32 {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&liq_bytes[16..32]);
            u128::from_be_bytes(buf)
        } else {
            0u128
        };

        if let Ok(block) = provider.get_block_number().await {
            self.last_block.store(block, std::sync::atomic::Ordering::Relaxed);
        }

        Ok(PoolState {
            reserve_a: U256::zero(),
            reserve_b: U256::zero(),
            sqrt_price_x96: Some(sqrt_price_x96),
            tick: Some(tick),
            liquidity: Some(liquidity),
            amp_coeff: None,
        })
    }

    fn simulated_v2_state(&self) -> PoolState {
        PoolState {
            reserve_a: U256::from(205_600_000_000_000u128),
            reserve_b: U256::from(100_000_000_000_000_000_000_000u128),
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            amp_coeff: None,
        }
    }

    fn simulated_v3_state(&self) -> PoolState {
        PoolState {
            reserve_a: U256::zero(),
            reserve_b: U256::zero(),
            sqrt_price_x96: Some(U256::from(3_647_949_655_879_842_476_793_799u128)),
            tick: Some(-199_729),
            liquidity: Some(1_462_847_672_098_985_101),
            amp_coeff: None,
        }
    }

    pub fn current_block(&self) -> u64 {
        self.last_block.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub async fn fetch_block_number(&self) -> Result<u64> {
        let provider = self.get_or_connect_ws().await?;
        let block = provider.get_block_number().await
            .context("Failed to fetch block number")?;
        self.last_block.store(block, std::sync::atomic::Ordering::Relaxed);
        Ok(block)
    }

    pub async fn get_provider(&self) -> Result<RootProvider<PubSubFrontend>> {
        self.get_or_connect_ws().await
    }

    pub async fn submit_flashbots_bundle(
        &self,
        bundle_txs: Vec<Vec<u8>>,
        target_block: u64,
    ) -> Result<String> {
        let relay = self.config.flashbots_url.clone().unwrap_or_else(|| "https://relay.flashbots.net".to_string());
        
        let signing_key = match self.config.flashbots_signing_key.as_deref() {
            Some(key) => key.to_string(),
            None => {
                warn!("FLASHBOTS_SIGNING_KEY not set — using random key (no reputation!)");
                let temp_key = alloy::signers::local::PrivateKeySigner::random();
                format!("0x{}", hex::encode(temp_key.to_bytes()))
            }
        };

        let contract_addr_str = self.config.contract_address.as_deref().unwrap_or("0x0000000000000000000000000000000000000000");
        let contract_addr = Address::from_str(contract_addr_str).unwrap_or(Address::ZERO);
        let chain_id = self.config.chain.evm_chain_id().unwrap_or(1);

        let submitter = crate::executor::FlashbotsSubmitter::new(
            relay,
            &signing_key,
            contract_addr,
            chain_id,
        )?;

        submitter.submit_raw_bundle(bundle_txs, target_block).await
    }

    pub fn execution_count(&self) -> u64 {
        self.execution_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Multi-chain manager
// ─────────────────────────────────────────────────────────────────────────────

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
    fn default() -> Self { Self::new() }
}
