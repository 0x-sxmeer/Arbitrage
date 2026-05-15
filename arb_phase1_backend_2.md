# Cross-Chain Arbitrage Engine — Phase 1 Backend
# Rust + Tokio | alloy-rs | PostgreSQL | Redis

## Project Structure

```
arb-engine/
├── Cargo.toml
├── .env.example
├── contracts/
│   ├── evm/                    # Solidity contracts (Foundry)
│   │   ├── src/AtomicArb.sol
│   │   └── foundry.toml
│   └── solana/                 # Anchor programs
│       └── programs/arb/
├── engine/                     # Rust workspace member — core arb logic
│   ├── src/
│   │   ├── main.rs
│   │   ├── chains/
│   │   │   ├── evm.rs          # Ethereum / L2 adapter
│   │   │   ├── solana.rs       # Solana adapter
│   │   │   └── cosmos.rs       # Cosmos / IBC adapter
│   │   ├── pool/
│   │   │   ├── mod.rs
│   │   │   ├── v2.rs           # Uniswap V2 math (x·y=k)
│   │   │   └── v3.rs           # Uniswap V3 tick math
│   │   ├── arb/
│   │   │   ├── mod.rs
│   │   │   ├── opportunity.rs  # NEV calculator
│   │   │   └── router.rs       # Bellman-Ford pathfinder
│   │   ├── mempool/
│   │   │   ├── mod.rs
│   │   │   └── listener.rs     # WebSocket mempool watcher
│   │   └── db/
│   │       ├── postgres.rs
│   │       └── redis.rs
├── frontend/                   # Next.js 14 (Month 5)
│   ├── app/
│   ├── components/
│   └── package.json
└── infra/
    ├── docker-compose.yml
    └── nginx.conf
```

---

## Cargo.toml

```toml
[workspace]
members = ["engine"]
resolver = "2"

[package]
name = "arb-engine"
version = "0.1.0"
edition = "2021"

[dependencies]
# Async runtime
tokio = { version = "1.35", features = ["full"] }

# Ethereum / EVM
alloy = { version = "0.1", features = [
    "providers",
    "signers",
    "contract",
    "rpc-types",
    "pubsub",
    "ws",
] }

# Solana
solana-client = "1.18"
solana-sdk = "1.18"

# Database
sqlx = { version = "0.7", features = ["postgres", "runtime-tokio-rustls", "uuid"] }
redis = { version = "0.24", features = ["tokio-comp", "connection-manager"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Math (fixed-point, no float rounding errors)
ethnum = "1.5"
uint = "0.9"

# Graph algorithms (Bellman-Ford pathfinding)
petgraph = "0.6"

# Logging & Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Error handling
anyhow = "1"
thiserror = "1"

# HTTP client
reqwest = { version = "0.11", features = ["json"] }

# Config
dotenvy = "0.15"
```

---

## engine/src/pool/mod.rs — Core Data Structures

```rust
use ethnum::U256;
use serde::{Deserialize, Serialize};

/// Represents a single liquidity pool on any supported chain.
/// Handles both V2 (constant product) and V3 (concentrated liquidity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pool {
    pub id: String,              // e.g. "0xUniswapV3Pool..."
    pub chain: ChainId,
    pub dex: DexProtocol,
    pub token_a: Token,
    pub token_b: Token,
    pub pool_type: PoolType,
    pub state: PoolState,
    pub fee_tier: u32,           // in basis points * 100 (e.g. 3000 = 0.3%)
    pub last_updated_block: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PoolType {
    ConstantProduct,             // Uniswap V2, Sushiswap, PancakeSwap
    ConcentratedLiquidity,       // Uniswap V3, Orca Whirlpool
    StableSwap,                  // Curve, Osmosis StableSwap
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolState {
    pub reserve_a: U256,
    pub reserve_b: U256,
    /// For V3: current sqrt price (Q64.96 fixed-point)
    pub sqrt_price_x96: Option<U256>,
    /// For V3: current active tick
    pub tick: Option<i32>,
    /// For V3: total active liquidity in current tick range
    pub liquidity: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub address: String,
    pub symbol: String,
    pub decimals: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainId {
    Ethereum,
    Base,
    Arbitrum,
    Solana,
    Osmosis,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DexProtocol {
    UniswapV2,
    UniswapV3,
    SushiSwap,
    PancakeSwap,
    Raydium,
    Orca,
    Osmosis,
}
```

---

## engine/src/pool/v2.rs — Constant Product AMM Math

```rust
use ethnum::U256;
use crate::pool::Pool;
use anyhow::{Result, bail};

/// Calculate exact output for a given input on a V2 constant-product pool.
/// Formula: output = (reserve_out * amount_in * 997) / (reserve_in * 1000 + amount_in * 997)
/// (Uniswap V2 takes 0.3% fee before the swap)
pub fn get_amount_out(pool: &Pool, amount_in: U256, zero_for_one: bool) -> Result<U256> {
    let (reserve_in, reserve_out) = if zero_for_one {
        (pool.state.reserve_a, pool.state.reserve_b)
    } else {
        (pool.state.reserve_b, pool.state.reserve_a)
    };

    if reserve_in.is_zero() || reserve_out.is_zero() {
        bail!("Pool has zero reserves — skipping");
    }
    if amount_in.is_zero() {
        bail!("Amount in must be greater than zero");
    }

    // Fee multiplier: 10000 - fee_bps (e.g. 9970 for 0.3%)
    let fee_numerator = U256::from(10_000u32 - pool.fee_tier / 10);
    let amount_in_with_fee = amount_in * fee_numerator;
    let numerator = amount_in_with_fee * reserve_out;
    let denominator = reserve_in * U256::from(10_000u32) + amount_in_with_fee;

    Ok(numerator / denominator)
}

/// Calculate price impact as a percentage (returns basis points)
pub fn calculate_price_impact_bps(pool: &Pool, amount_in: U256, zero_for_one: bool) -> u32 {
    let (reserve_in, _) = if zero_for_one {
        (pool.state.reserve_a, pool.state.reserve_b)
    } else {
        (pool.state.reserve_b, pool.state.reserve_a)
    };

    if reserve_in.is_zero() { return 10_000; } // 100% impact on empty pool

    // Price impact ≈ trade_size / (reserve_in + trade_size)
    let impact = amount_in * U256::from(10_000u32) / (reserve_in + amount_in);
    impact.as_u32()
}
```

---

## engine/src/arb/opportunity.rs — Net Expected Value Calculator

```rust
use ethnum::U256;
use serde::{Deserialize, Serialize};

/// A discovered arbitrage opportunity between two or more pools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    pub id: String,
    pub route: Vec<SwapStep>,
    pub input_amount: U256,
    pub gross_output: U256,

    // Cost components (all denominated in input token wei equivalent)
    pub estimated_gas_units: u64,
    pub gas_price_gwei: f64,       // EIP-1559 base fee + priority tip
    pub total_swap_fees_wei: U256,
    pub price_impact_bps: u32,     // in basis points

    // Final verdict
    pub net_expected_value: i128,  // positive = profitable, signed
    pub is_executable: bool,
    pub discovered_at_block: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapStep {
    pub pool_id: String,
    pub dex: String,
    pub chain: String,
    pub token_in: String,
    pub token_out: String,
    pub amount_in: U256,
    pub expected_amount_out: U256,
    pub fee_bps: u32,
}

impl ArbitrageOpportunity {
    /// Minimum profit threshold in USD equivalent (wei).
    /// Below this we don't execute — gas variance can eat the margin.
    const MIN_PROFIT_THRESHOLD_WEI: i128 = 500_000_000_000_000; // ~$0.50 at ETH $3000

    pub fn calculate_nev(&mut self, eth_price_usd: f64) {
        let gross_profit = self.gross_output.as_i128() - self.input_amount.as_i128();

        // Gas cost in wei: gas_units × (base_fee + priority_tip) gwei
        let gas_cost_wei = (self.estimated_gas_units as f64
            * self.gas_price_gwei
            * 1_000_000_000.0) as i128; // gwei → wei

        let swap_fees = self.total_swap_fees_wei.as_i128();

        // Price impact penalty: approximate loss from moving the market
        let impact_loss = (self.input_amount.as_i128() as f64
            * self.price_impact_bps as f64
            / 10_000.0) as i128;

        self.net_expected_value = gross_profit - gas_cost_wei - swap_fees - impact_loss;
        self.is_executable = self.net_expected_value > Self::MIN_PROFIT_THRESHOLD_WEI;

        let nev_usd = self.net_expected_value as f64 / 1e18 * eth_price_usd;
        tracing::info!(
            opportunity_id = %self.id,
            nev_usd = nev_usd,
            executable = self.is_executable,
            "NEV calculated"
        );
    }
}
```

---

## engine/src/mempool/listener.rs — WebSocket Mempool Monitor

```rust
use alloy::{
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::eth::Transaction,
};
use anyhow::Result;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

// Uniswap V3 SwapRouter02 — we watch for pending swaps here
const UNISWAP_V3_ROUTER: &str = "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45";
// Uniswap V3 `exactInputSingle` selector (first 4 bytes of keccak256)
const EXACT_INPUT_SINGLE_SELECTOR: &str = "0x414bf389";

pub struct MempoolListener {
    ws_url: String,
    reconnect_delay_ms: u64,
}

impl MempoolListener {
    pub fn new(ws_url: impl Into<String>) -> Self {
        Self {
            ws_url: ws_url.into(),
            reconnect_delay_ms: 500,
        }
    }

    /// Runs forever, reconnecting on any WebSocket failure.
    pub async fn run(&self) -> Result<()> {
        loop {
            match self.connect_and_stream().await {
                Ok(_) => {
                    warn!("WebSocket stream ended cleanly — reconnecting");
                }
                Err(e) => {
                    error!("WebSocket error: {:?} — reconnecting in {}ms", e, self.reconnect_delay_ms);
                }
            }
            sleep(Duration::from_millis(self.reconnect_delay_ms)).await;
        }
    }

    async fn connect_and_stream(&self) -> Result<()> {
        info!(url = %self.ws_url, "Connecting to WebSocket RPC");

        let ws = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new()
            .on_ws(ws)
            .await?;

        info!("WebSocket connected. Subscribing to pending transactions...");

        // Subscribe to ALL pending transactions in the mempool
        let sub = provider.subscribe_pending_transactions().await?;
        let mut stream = sub.into_stream();

        while let Some(tx_hash) = stream.next().await {
            // Fetch full transaction details
            if let Ok(Some(tx)) = provider.get_transaction_by_hash(tx_hash).await {
                self.process_transaction(tx).await;
            }
        }

        Ok(())
    }

    async fn process_transaction(&self, tx: Transaction) {
        let to = match tx.to {
            Some(addr) => format!("{addr:#x}"),
            None => return, // contract deployment, skip
        };

        // Filter: only care about Uniswap V3 Router interactions
        if to.to_lowercase() != UNISWAP_V3_ROUTER.to_lowercase() {
            return;
        }

        let input = &tx.input;
        if input.len() < 4 {
            return;
        }

        let selector = format!("0x{}", hex::encode(&input[..4]));

        // Only process exactInputSingle swaps (the most common arb trigger)
        if selector == EXACT_INPUT_SINGLE_SELECTOR {
            let value_eth = tx.value.to::<u128>() as f64 / 1e18;

            info!(
                tx_hash = ?tx.hash,
                from = ?tx.from,
                value_eth = value_eth,
                gas_price_gwei = ?tx.gas_price.map(|g| g as f64 / 1e9),
                "🔍 Detected pending Uniswap V3 swap — checking for arb opportunity"
            );

            // TODO: Decode calldata, identify affected pool, run NEV calculator
            // self.arb_engine.evaluate_post_swap_prices(pool_id, decoded_params).await;
        }
    }
}
```

---

## engine/src/main.rs — Entry Point

```rust
use anyhow::Result;
use dotenvy::dotenv;
use tracing_subscriber::EnvFilter;

mod chains;
mod pool;
mod arb;
mod mempool;
mod db;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let eth_ws_url = std::env::var("ETH_WS_URL")
        .expect("ETH_WS_URL must be set (e.g. wss://mainnet.infura.io/ws/v3/YOUR_KEY)");

    tracing::info!("Starting Cross-Chain Arbitrage Engine v0.1.0");

    let listener = mempool::listener::MempoolListener::new(eth_ws_url);

    // Run mempool listener — reconnects automatically on failure
    listener.run().await?;

    Ok(())
}
```

---

## .env.example

```bash
# Ethereum WebSocket RPC (Alchemy / Infura / private node)
ETH_WS_URL=wss://eth-mainnet.g.alchemy.com/v2/YOUR_API_KEY

# Solana RPC (Helius recommended for speed)
SOLANA_RPC_URL=https://mainnet.helius-rpc.com/?api-key=YOUR_KEY

# Database
DATABASE_URL=postgresql://user:password@localhost:5432/arb_engine
REDIS_URL=redis://localhost:6379

# Execution wallet (NEVER commit this)
PRIVATE_KEY=0x...

# CoinGecko for ETH price (gas cost normalization)
COINGECKO_API_KEY=your_key

# Logging
RUST_LOG=arb_engine=info,warn
```

---

## Next Steps (Month 2 Prompt for Claude)

Once you have the mempool listener running and logging detected swaps, start a new chat and say:

> "Here is our current Rust arbitrage engine [paste code]. The `MempoolListener` is detecting pending Uniswap V3 swaps. Now implement:
> 1. Full calldata decoding for `exactInputSingle` and `exactInput` (multi-hop)
> 2. A `PoolStateCache` in Redis that updates reserves on each detected swap
> 3. The `ArbitrageOpportunity` router using Bellman-Ford across all cached pools
> 4. Integration with the `ArbitrageOpportunity::calculate_nev()` function"
