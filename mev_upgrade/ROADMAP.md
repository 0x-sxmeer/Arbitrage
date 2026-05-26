# MEV Engine v2 — Implementation Roadmap
## For Claude Sonnet in Antigravity

---

## Overview

This document is the **step-by-step integration guide** for merging the Phase 1–4 upgrade files into the existing `Arbitrage/` codebase. Each phase is self-contained and can be merged independently. Complete them in order — each phase builds on the last.

**Target**: Scale from ~$1-5/hour DEX arb → **$20/hour ($480/day)** via four compounding strategies.

```
Phase 1: Flash Loan Capital + Yul Optimization  →  +30-50% win rate improvement
Phase 2: CEX-DEX Statistical Arbitrage          →  ~$8-12/hr alone
Phase 3: Backrunning + Liquidations             →  ~$5-8/hr alone
Phase 4: Cross-Chain (Base↔OP↔ARB)             →  ~$3-5/hr alone
```

---

## Repository Structure After Integration

```
Arbitrage/
├── arb-engine/
│   ├── contracts/evm/
│   │   └── src/
│   │       ├── AtomicArb.sol          ← EXISTING (keep for reference)
│   │       └── AtomicArbV2.sol        ← NEW: copy from upgrade/contracts/
│   │   └── script/
│   │       ├── DeployBase.s.sol       ← EXISTING
│   │       └── DeployV2.s.sol         ← NEW: copy from upgrade/contracts/
│   ├── engine/
│   │   ├── Cargo.toml                 ← REPLACE with upgrade/engine/Cargo.toml
│   │   ├── .env                       ← EXTEND with upgrade/engine/.env.example
│   │   └── src/
│   │       ├── main.rs                ← MODIFY (add new module wiring)
│   │       ├── config.rs              ← MODIFY (add new config fields)
│   │       ├── cex_dex/               ← NEW: copy entire folder
│   │       ├── liquidations/          ← NEW: copy entire folder
│   │       └── cross_chain/           ← NEW: copy entire folder
```

---

## PHASE 1 — Flash Loan Capital + Yul Optimization

### Step 1.1 — Copy the new contract

```bash
cp upgrade/contracts/AtomicArbV2.sol \
   Arbitrage/arb-engine/contracts/evm/src/AtomicArbV2.sol

cp upgrade/contracts/DeployV2.s.sol \
   Arbitrage/arb-engine/contracts/evm/script/DeployV2.s.sol
```

### Step 1.2 — Deploy AtomicArbV2

```bash
cd Arbitrage/arb-engine/contracts/evm

# Install deps (if not already done)
forge soldeer install

# Deploy to Base mainnet
forge script script/DeployV2.s.sol \
  --rpc-url $BASE_HTTP_URL \
  --broadcast \
  --verify \
  --etherscan-api-key $BASESCAN_API_KEY \
  -vvvv
```

After deployment, note the printed contract address and add it to `.env`:
```
CONTRACT_ADDRESS=0x_YOUR_DEPLOYED_ADDRESS
BALANCER_VAULT_ADDRESS=0xBA12222222228d8Ba445958a75a0704d566BF2C8
```

### Step 1.3 — Update Cargo.toml

Replace `Arbitrage/arb-engine/engine/Cargo.toml` with `upgrade/engine/Cargo.toml`.

Key new dependencies added:
- `tokio-tungstenite` — Binance WebSocket (Phase 2)
- `fixed` — fixed-point price math
- `parking_lot` — faster RwLock on hot paths
- `k256` + `sha3` — Flashbots signing (Phase 3)
- `metrics-exporter-prometheus` — observability

```bash
cp upgrade/engine/Cargo.toml \
   Arbitrage/arb-engine/engine/Cargo.toml

# Verify it compiles
cd Arbitrage/arb-engine/engine
cargo check
```

### Step 1.4 — Update executor to use AtomicArbV2

Open `Arbitrage/arb-engine/engine/src/executor/mod.rs`.

Find the `build_arb_calldata` function and add a new variant that builds `ArbParamsV2`-encoded calldata for the V2 contract. The V2 calldata:

```rust
// In executor/mod.rs — add alongside existing execute_opportunity():

pub async fn execute_opportunity_v2(
    &self,
    opp: &ArbitrageOpportunity,
    use_balancer: bool,
) -> Result<H256> {
    // Build ArbParamsV2 struct
    let legs: Vec<SwapLegEncoded> = opp.steps.iter().map(|step| SwapLegEncoded {
        router:      step.router_address.parse().unwrap(),
        router_type: step.router_type as u8,
        fee:         step.fee as u32,
        token_in:    step.token_in.parse().unwrap(),
        token_out:   step.token_out.parse().unwrap(),
    }).collect();

    // ABI-encode for executeArbitrageV2(ArbParamsV2)
    // Use alloy sol! codegen or manual ABI encoding
    let calldata = encode_execute_v2(
        opp.token_in.parse()?,
        opp.borrow_amount,
        use_balancer,
        legs,
        opp.paths.clone(),
        opp.min_profit_wei,
        chrono::Utc::now().timestamp() as u64 + 30,
    );

    self.submit_transaction(calldata).await
}
```

---

## PHASE 2 — CEX-DEX Statistical Arbitrage

### Step 2.1 — Copy module files

```bash
cp -r upgrade/engine/src/cex_dex \
      Arbitrage/arb-engine/engine/src/cex_dex
```

### Step 2.2 — Register module in main.rs

Open `Arbitrage/arb-engine/engine/src/main.rs`.

Add at the top with the other `mod` declarations:

```rust
mod cex_dex;
```

### Step 2.3 — Wire CEX-DEX engine in main()

Find the section in `main()` that spawns the `MempoolListener` task. After it, add:

```rust
// ── Phase 2: CEX-DEX Engine ───────────────────────────────────────────────
if config.cex_dex_enabled {
    use cex_dex::binance_feed::BinancePriceFeeder;
    use cex_dex::spread_engine::{SpreadEngine, SpreadConfig};

    let symbols = config.cex_dex_symbols
        .split(',')
        .map(String::from)
        .collect::<Vec<_>>();

    let (binance_feeder, cex_feed) = BinancePriceFeeder::new(
        symbols.clone(),
        5_000, // stale after 5s
    );

    // DEX price feed (shares the same PriceMatrix as cross-chain monitor)
    let dex_feed = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));

    // Symbol → DEX pool mapping (configure per your pools)
    let mut symbol_to_dex = std::collections::HashMap::new();
    symbol_to_dex.insert("ETHUSDT".to_string(), "eth_usdc_base".to_string());
    symbol_to_dex.insert("BTCUSDT".to_string(), "wbtc_usdc_base".to_string());

    let spread_config = SpreadConfig {
        min_spread_pct:     config.cex_dex_min_spread_pct,
        loan_size_usd:      config.cex_dex_loan_size_usd,
        max_inventory_usd:  config.cex_dex_max_inventory_usd,
        ..Default::default()
    };

    let engine = SpreadEngine::new(
        spread_config,
        cex_feed.clone(),
        dex_feed.clone(),
        symbol_to_dex,
    );

    // Spawn Binance feeder
    tokio::spawn(async move {
        if let Err(e) = binance_feeder.run().await {
            error!("Binance feeder crashed: {}", e);
        }
    });

    // Spawn spread engine
    let execute = config.execute_enabled;
    tokio::spawn(async move {
        if let Err(e) = engine.run(execute).await {
            error!("CEX-DEX engine crashed: {}", e);
        }
    });

    info!("✓ CEX-DEX engine started ({} symbols)", symbols.len());
} else {
    info!("  CEX-DEX engine disabled (set CEX_DEX_ENABLED=true to enable)");
}
```

### Step 2.4 — Add config fields

Open `Arbitrage/arb-engine/engine/src/config.rs`.

Add these fields to the `Config` struct:

```rust
// Phase 2: CEX-DEX
pub cex_dex_enabled:        bool,
pub cex_dex_min_spread_pct: f64,
pub cex_dex_loan_size_usd:  f64,
pub cex_dex_max_inventory_usd: f64,
pub cex_dex_symbols:        String,
pub binance_api_key:        Option<String>,
pub binance_api_secret:     Option<String>,
```

Add to `from_env()`:

```rust
cex_dex_enabled:          env_bool("CEX_DEX_ENABLED", false),
cex_dex_min_spread_pct:   env_f64("CEX_DEX_MIN_SPREAD_PCT", 0.15),
cex_dex_loan_size_usd:    env_f64("CEX_DEX_LOAN_SIZE_USD", 500_000.0),
cex_dex_max_inventory_usd: env_f64("CEX_DEX_MAX_INVENTORY_USD", 100_000.0),
cex_dex_symbols:          std::env::var("CEX_DEX_SYMBOLS")
                              .unwrap_or_else(|_| "ETHUSDT,BTCUSDT".to_string()),
binance_api_key:          std::env::var("BINANCE_API_KEY").ok(),
binance_api_secret:       std::env::var("BINANCE_API_SECRET").ok(),
```

Add these helpers at the bottom of `config.rs`:

```rust
fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key).map(|v| v == "true" || v == "1").unwrap_or(default)
}
fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
```

---

## PHASE 3 — Backrunning + Liquidations

### Step 3.1 — Copy module files

```bash
cp -r upgrade/engine/src/liquidations \
      Arbitrage/arb-engine/engine/src/liquidations
```

### Step 3.2 — Register module in main.rs

```rust
mod liquidations;
```

### Step 3.3 — Connect private mempool feed

In `Arbitrage/arb-engine/engine/src/mempool/listener.rs`, find the WebSocket connection setup. Add a secondary connection to the private mempool feed (Bloxroute):

```rust
// After the primary WS connection setup, add:
if let Some(ref private_rpc) = config.private_rpc_url {
    // Bloxroute / Chainbound provide streaming pending tx feed
    // They send raw pending txs before they hit public mempool
    let blx_url = format!(
        "wss://virginia.blxrbdn.com/ws?Authorization={}",
        config.bloxroute_api_key.as_deref().unwrap_or("")
    );
    // Subscribe to pendingTxs stream
    // Feed into the same tx_sender channel as the public mempool
    tokio::spawn(connect_private_feed(blx_url, tx_sender.clone()));
}
```

### Step 3.4 — Wire backrunner into mempool pipeline

In `mempool/listener.rs`, inside the worker loop where `evaluate_opportunity()` is called, add backrun evaluation:

```rust
// After the existing BF opportunity check, add:
if config.backrun_enabled {
    if let Some(swap) = decode_as_swap(&tx_payload) {
        let opp = backrun_detector.evaluate(&swap).await;
        if let Some(opp) = opp {
            if config.execute_enabled {
                bundle_builder.submit_backrun_bundle(
                    &opp,
                    current_block + 1,
                    swap.gas_price,
                ).await.ok();
            }
        }
    }
}
```

### Step 3.5 — Wire liquidation monitor

In `main.rs`, spawn the liquidation monitor alongside the mempool listener:

```rust
if config.liquidations_enabled {
    use liquidations::liquidation_monitor::LiquidationMonitor;
    use liquidations::bundle_builder::BundleBuilder;

    let token_prices = Arc::new(RwLock::new(HashMap::new())); // share with price feed
    let monitor = LiquidationMonitor::new(
        config.liquidation_min_profit_usd,
        token_prices.clone(),
    );

    let bundle_builder = BundleBuilder::new(
        config.flashbots_url.clone(),
        config.flashbots_signing_key.clone().unwrap_or_default(),
        config.private_key.clone().unwrap_or_default(),
        config.contract_address.clone().unwrap_or_default(),
        8453, // Base chain ID
    );

    // Spawn: scan every new block
    tokio::spawn(async move {
        let mut block_sub = evm_adapter.subscribe_blocks().await?;
        while let Some(block) = block_sub.next().await {
            let opps = monitor.scan_block(block.number).await;
            for opp in opps {
                if config.execute_enabled {
                    bundle_builder.submit_liquidation_tx(&opp, gas_price).await.ok();
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    });

    info!("✓ Liquidation monitor started");
}
```

### Step 3.6 — Add config fields

```rust
// In Config struct:
pub backrun_enabled:           bool,
pub backrun_min_impact_bps:    f64,
pub backrun_min_profit_usd:    f64,
pub liquidations_enabled:      bool,
pub liquidation_min_profit_usd: f64,
pub bloxroute_api_key:         Option<String>,

// In from_env():
backrun_enabled:           env_bool("BACKRUN_ENABLED", false),
backrun_min_impact_bps:    env_f64("BACKRUN_MIN_IMPACT_BPS", 20.0),
backrun_min_profit_usd:    env_f64("BACKRUN_MIN_PROFIT_USD", 10.0),
liquidations_enabled:      env_bool("LIQUIDATIONS_ENABLED", false),
liquidation_min_profit_usd: env_f64("LIQUIDATION_MIN_PROFIT_USD", 20.0),
bloxroute_api_key:         std::env::var("BLOXROUTE_API_KEY").ok(),
```

---

## PHASE 4 — Cross-Chain Arbitrage

### Step 4.1 — Copy module files

```bash
cp -r upgrade/engine/src/cross_chain \
      Arbitrage/arb-engine/engine/src/cross_chain
```

### Step 4.2 — Register module and wire in main.rs

```rust
mod cross_chain;
```

```rust
// In main(), after Phase 3 section:
if config.cross_chain_enabled {
    use cross_chain::cross_chain_engine::{CrossChainEngine, ChainId};
    use cross_chain::chain_monitor::ChainMonitor;
    use cross_chain::inventory_manager::InventoryManager;
    use cross_chain::bridge_rebalancer::BridgeRebalancer;

    let price_matrix = Arc::new(RwLock::new(std::collections::HashMap::new()));

    // Spawn one ChainMonitor per chain
    for (chain, rpc_url) in [
        (ChainId::Base,     config.base_http_url.clone().unwrap()),
        (ChainId::Optimism, config.op_http_url.clone().unwrap()),
        (ChainId::Arbitrum, config.arb_http_url.clone().unwrap()),
    ] {
        let monitor = ChainMonitor::new(chain, rpc_url, price_matrix.clone(), 500);
        tokio::spawn(async move {
            if let Err(e) = monitor.run().await {
                error!("Chain monitor {:?} crashed: {}", chain, e);
            }
        });
    }

    // Spawn cross-chain engine
    let engine = CrossChainEngine::new(
        price_matrix.clone(),
        config.cross_chain_trade_size_usd,
    );
    let execute = config.execute_enabled;
    tokio::spawn(async move {
        if let Err(e) = engine.run(execute).await {
            error!("Cross-chain engine crashed: {}", e);
        }
    });

    info!("✓ Cross-chain engine started (Base + Optimism + Arbitrum)");
}
```

### Step 4.3 — Add config fields

```rust
// In Config struct:
pub cross_chain_enabled:        bool,
pub cross_chain_trade_size_usd: f64,
pub op_http_url:                Option<String>,
pub op_ws_url:                  Option<String>,
pub arb_http_url:               Option<String>,
pub cross_chain_min_usdc_reserve: f64,

// In from_env():
cross_chain_enabled:          env_bool("CROSS_CHAIN_ENABLED", false),
cross_chain_trade_size_usd:   env_f64("CROSS_CHAIN_TRADE_SIZE_USD", 50_000.0),
op_http_url:                  std::env::var("OP_HTTP_URL").ok(),
op_ws_url:                    std::env::var("OP_WS_URL").ok(),
arb_http_url:                 std::env::var("ARB_HTTP_URL").ok(),
cross_chain_min_usdc_reserve: env_f64("CROSS_CHAIN_MIN_USDC_RESERVE", 20_000.0),
```

### Step 4.4 — Deploy contract on Optimism and Arbitrum

```bash
# Optimism
forge script script/DeployV2.s.sol \
  --rpc-url $OP_HTTP_URL \
  --broadcast --verify \
  --etherscan-api-key $OPTIMISM_ETHERSCAN_KEY \
  -vvvv

# Arbitrum
forge script script/DeployV2.s.sol \
  --rpc-url $ARB_HTTP_URL \
  --broadcast --verify \
  --etherscan-api-key $ARBISCAN_API_KEY \
  -vvvv
```

Update `.env` with both addresses:
```
CONTRACT_ADDRESS_OPTIMISM=0x...
CONTRACT_ADDRESS_ARBITRUM=0x...
```

---

## Testing Each Phase

### Phase 1 — Simulation test (Foundry)

```bash
cd Arbitrage/arb-engine/contracts/evm

# Fork Base and simulate a flash loan arb
forge test --fork-url $BASE_HTTP_URL -vvvv --match-contract AtomicArbV2Test
```

### Phase 2 — Dry run CEX-DEX detection

```bash
cd Arbitrage/arb-engine/engine

# Enable detection, disable execution
CEX_DEX_ENABLED=true EXECUTE_ENABLED=false cargo run 2>&1 | grep "CEX-DEX SPREAD"
```

You should see output like:
```
💰 CEX-DEX SPREAD | ETHUSDT | 0.215% | dir=BuyDexSellCex | size=$320k | expPnL=$482.50
```

### Phase 3 — Verify backrun detection

```bash
BACKRUN_ENABLED=true EXECUTE_ENABLED=false cargo run 2>&1 | grep "BACKRUN"
```

### Phase 4 — Cross-chain price matrix

```bash
CROSS_CHAIN_ENABLED=true EXECUTE_ENABLED=false cargo run 2>&1 | grep "CROSS-CHAIN"
```

---

## Enabling Live Execution (Phase Rollout)

**Always enable one phase at a time. Monitor for 24h before enabling the next.**

```bash
# Week 1: Enable only Phase 1 (Flash Loan arb — lowest risk)
EXECUTE_ENABLED=true
CEX_DEX_ENABLED=false
BACKRUN_ENABLED=false
LIQUIDATIONS_ENABLED=false
CROSS_CHAIN_ENABLED=false

# Week 2: Add CEX-DEX (higher profit, slightly more complexity)
CEX_DEX_ENABLED=true

# Week 3: Add Backrunning
BACKRUN_ENABLED=true

# Week 4: Add Liquidations
LIQUIDATIONS_ENABLED=true

# Week 5: Add Cross-Chain
CROSS_CHAIN_ENABLED=true
```

---

## Revenue Model by Phase

| Phase | Strategy               | Expected $/hr | Risk Level |
|-------|------------------------|--------------|------------|
| 1     | Flash loan arb (DEX)   | $2–5         | Low        |
| 2     | CEX-DEX spread arb     | $8–12        | Medium     |
| 3     | Backrunning            | $3–5         | Low-Med    |
| 3     | Liquidations           | $2–4         | Low        |
| 4     | Cross-chain arb        | $3–5         | Medium     |
| **Σ** | **All phases**         | **$18–31**   | —          |

---

## Common Integration Errors

**`error[E0432]: unresolved import crate::cex_dex`**
→ Ensure `mod cex_dex;` is added to `main.rs`.

**`error: feature "native-tls" not found`**
→ Run `cargo update` after updating `Cargo.toml`.

**`Balancer flash loan reverted`**
→ Check `BALANCER_VAULT_ADDRESS` matches `0xBA12222...`. It is the same on all EVM chains.

**`Flashbots bundle not included`**
→ Increase `gas_premium_bps` in `BackrunOpportunity` from 10 to 50.

**`CEX-DEX: no opportunities found`**
→ Check `BINANCE_WS_URL` is reachable. Verify symbols include perpetuals (e.g. `ETHUSDT`, not `ETH-USDT`).

**`Cross-chain: all prices stale`**
→ Ensure `OP_HTTP_URL` and `ARB_HTTP_URL` are set in `.env`.

---

## Monitoring Dashboard

The existing `app/` React dashboard needs two new panels. In `ExecutionDashboard.jsx`, add:

```jsx
// Phase 2 panel
<CexDexPanel
  spread={metrics.cexDexSpread}
  lastOpportunity={metrics.lastCexDexOpp}
  hourlyPnl={metrics.cexDexHourlyPnl}
/>

// Phase 3 panel
<LiquidationPanel
  pendingLiquidations={metrics.pendingLiquidations}
  executedToday={metrics.liquidationsToday}
  totalBonus={metrics.liquidationBonusUsd}
/>

// Phase 4 panel
<CrossChainPanel
  priceDivergences={metrics.crossChainDivergences}
  inventoryByChain={metrics.chainInventory}
/>
```

---

## Architecture Summary

```
                    ┌─────────────────────────────────────┐
                    │         main.rs (orchestrator)       │
                    └──────────────────┬──────────────────┘
                                       │ spawns
           ┌───────────────────────────┼───────────────────────────┐
           │                           │                           │
     Phase 1+3                    Phase 2                    Phase 4
  MempoolListener           BinancePriceFeeder           ChainMonitor × 3
  BackrunDetector           SpreadEngine                 CrossChainEngine
  LiquidationMonitor        KellySizer                   BridgeRebalancer
           │                    │                               │
           └──────────┬─────────┘                               │
                      ▼                                         │
              LiquidityGraph ←────────────────────────────────-─┘
              (shared RwLock)
                      │
                      ▼
              BundleBuilder / FlashbotsSubmitter
                      │
                      ▼
              AtomicArbV2.sol  ←── Aave V3 flashLoanSimple()
                                   Balancer flashLoan()     (Phase 1)
```
