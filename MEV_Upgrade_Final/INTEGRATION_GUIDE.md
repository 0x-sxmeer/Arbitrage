# MEV Engine v2 — Antigravity Integration Guide
## For Claude Sonnet — Step-by-Step Implementation

---

## Overview

This guide tells you EXACTLY what to do to the existing `Arbitrage/` project.
Every instruction has a specific file path, line number, and exact code.

**What this adds:**
- `MegaScanner` — scans 10,000+ pools from 8 sources in parallel
- `MegaScorer` — scores ALL tokens with 7 signals, re-ranks every 30s
- Phase 1: 300-600 DEX-DEX eligible tokens (was static ~20)
- Phase 2: 200-400 CEX-DEX tokens via Binance WS
- Phase 3: 600-1000 backrun targets with whale detection
- Phase 4: 200-400 cross-chain divergence pairs

---

## Step 1 — Copy New Module Files

```bash
# Copy all new source files into the existing engine
cp -r upgrade/engine/src/discovery   Arbitrage/arb-engine/engine/src/
cp -r upgrade/engine/src/scoring     Arbitrage/arb-engine/engine/src/
cp -r upgrade/engine/src/cex_dex     Arbitrage/arb-engine/engine/src/
cp -r upgrade/engine/src/liquidations Arbitrage/arb-engine/engine/src/
cp -r upgrade/engine/src/cross_chain  Arbitrage/arb-engine/engine/src/
```

---

## Step 2 — Update Cargo.toml

Replace `Arbitrage/arb-engine/engine/Cargo.toml` with `upgrade/engine/Cargo.toml`.

New dependencies added (diff from existing):
- `tokio-tungstenite` — Binance WebSocket connection
- `parking_lot`       — faster RwLock for hot scoring paths
- `k256` + `sha3`     — Flashbots bundle signing
- `fixed`             — fixed-point price math
- `metrics-exporter-prometheus` — observability dashboard

```bash
cp upgrade/engine/Cargo.toml Arbitrage/arb-engine/engine/Cargo.toml
cd Arbitrage/arb-engine/engine && cargo check  # verify it compiles
```

---

## Step 3 — Modify main.rs (5 insertions)

Open `Arbitrage/arb-engine/engine/src/main.rs`

### 3A — Add module declarations (after line ~22, after existing `mod pool;`)

```rust
mod discovery {
    pub mod mega_scanner;
}
mod scoring {
    pub mod mega_scorer;
}
mod cex_dex {
    pub mod binance_feed;
    pub mod spread_engine;
}
mod liquidations {
    pub mod liquidation_monitor;
    pub mod backrun;
}
mod cross_chain {
    pub mod cross_chain_engine;
    pub mod inventory_manager;
}
```

### 3B — Add imports (after existing `use metrics::EngineMetrics;`)

```rust
use discovery::mega_scanner::MegaScanner;
use scoring::mega_scorer::{MegaScorer, WhaleScores};
use std::collections::HashMap;
```

### 3C — Add MegaScanner startup (after `let graph = Arc::new(RwLock::new(...));` ~line 168)

```rust
// ── Mega Token Universe ─────────────────────────────────────────────────────
let whale_scores: WhaleScores = Arc::new(RwLock::new(HashMap::new()));
let (mega_scanner, pool_registry, binance_listed) = MegaScanner::new();
let (mega_scorer, phase_lists) = MegaScorer::new(
    pool_registry.clone(),
    binance_listed.clone(),
    whale_scores.clone(),
);

tokio::spawn(async move {
    if let Err(e) = mega_scanner.run().await {
        tracing::error!("MegaScanner crashed: {}", e);
    }
});

tokio::spawn(async move {
    mega_scorer.run().await;
});

// Stats logger every 60s
{
    let pl = phase_lists.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            t.tick().await;
            let l = pl.read().await;
            tracing::info!(
                "📊 Universe | pools:{} tokens:{} | P1:{} P2:{} P3:{} P4:{}",
                l.total_pools_scanned, l.total_tokens_scored,
                l.phase1.len(), l.phase2.len(), l.phase3.len(), l.phase4.len(),
            );
        }
    });
}
info!("✅ Mega Token Universe started (10+ sources, 30s rescore)");
```

### 3D — Add Phase 2 CEX-DEX (after the block above)

```rust
if config.cex_dex_enabled {
    use cex_dex::binance_feed::BinancePriceFeeder;
    use cex_dex::spread_engine::SpreadEngine;

    let syms: Vec<String> = phase_lists.read().await.phase2.iter()
        .map(|t| format!("{}USDT", t.symbol.to_uppercase()))
        .take(400).collect();

    let (feeder, cex_feed) = BinancePriceFeeder::new(syms, 5_000);
    let engine = SpreadEngine::new(
        cex_feed, phase_lists.clone(),
        config.cex_dex_min_spread_pct, config.cex_dex_loan_size_usd,
    );
    tokio::spawn(async move { feeder.run().await.ok(); });
    let exec = config.execute_enabled;
    tokio::spawn(async move { engine.run(exec).await.ok(); });
    info!("✅ CEX-DEX engine started");
}
```

### 3E — Add Phase 3 Liquidations + Phase 4 Cross-Chain (after Phase 2 block)

```rust
if config.liquidations_enabled {
    use liquidations::liquidation_monitor::LiquidationMonitor;
    let monitor = LiquidationMonitor::new(config.liquidation_min_profit_usd, whale_scores.clone());
    tokio::spawn(async move { monitor.run().await; });
    info!("✅ Liquidation monitor started");
}

if config.cross_chain_enabled {
    use cross_chain::cross_chain_engine::CrossChainEngine;
    let xc = CrossChainEngine::new(
        phase_lists.clone(),
        config.cross_chain_trade_size_usd,
        config.op_http_url.clone(),
        config.arb_http_url.clone(),
    );
    let exec = config.execute_enabled;
    tokio::spawn(async move { xc.run(exec).await.ok(); });
    info!("✅ Cross-chain engine started");
}
```

---

## Step 4 — Modify config.rs (2 insertions)

Open `Arbitrage/arb-engine/engine/src/config.rs`

### 4A — Add fields to Config struct (after `pub execute_enabled: bool,`)

```rust
// Phase 2
pub cex_dex_enabled:         bool,
pub cex_dex_min_spread_pct:  f64,
pub cex_dex_loan_size_usd:   f64,
pub binance_api_key:         Option<String>,
// Phase 3
pub backrun_enabled:          bool,
pub liquidations_enabled:     bool,
pub liquidation_min_profit_usd: f64,
// Phase 4
pub cross_chain_enabled:        bool,
pub cross_chain_trade_size_usd: f64,
pub op_http_url:                Option<String>,
pub arb_http_url:               Option<String>,
pub contract_address_op:        Option<String>,
pub contract_address_arb:       Option<String>,
pub cross_chain_min_usdc:       f64,
```

### 4B — Add to from_env() (after `execute_enabled: ...` line)

```rust
cex_dex_enabled:          env_bool("CEX_DEX_ENABLED", false),
cex_dex_min_spread_pct:   env_f64("CEX_DEX_MIN_SPREAD_PCT", 0.15),
cex_dex_loan_size_usd:    env_f64("CEX_DEX_LOAN_SIZE_USD", 500_000.0),
binance_api_key:          std::env::var("BINANCE_API_KEY").ok(),
backrun_enabled:          env_bool("BACKRUN_ENABLED", false),
liquidations_enabled:     env_bool("LIQUIDATIONS_ENABLED", false),
liquidation_min_profit_usd: env_f64("LIQUIDATION_MIN_PROFIT_USD", 20.0),
cross_chain_enabled:        env_bool("CROSS_CHAIN_ENABLED", false),
cross_chain_trade_size_usd: env_f64("CROSS_CHAIN_TRADE_SIZE_USD", 50_000.0),
op_http_url:     std::env::var("OP_HTTP_URL").ok(),
arb_http_url:    std::env::var("ARB_HTTP_URL").ok(),
contract_address_op:  std::env::var("CONTRACT_ADDRESS_OPTIMISM").ok(),
contract_address_arb: std::env::var("CONTRACT_ADDRESS_ARBITRUM").ok(),
cross_chain_min_usdc: env_f64("CROSS_CHAIN_MIN_USDC", 20_000.0),
```

### 4C — Add helper fns at bottom of config.rs (after closing `}`)

```rust
fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key).map(|v| v.to_lowercase() == "true" || v == "1").unwrap_or(default)
}
fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
```

---

## Step 5 — Modify mempool/listener.rs (backrun integration)

Open `Arbitrage/arb-engine/engine/src/mempool/listener.rs`

### 5A — Add BackrunDetector to MempoolListener struct

Find the `MempoolListener` struct definition and add:

```rust
// Inside MempoolListener struct:
backrun_detector: Option<Arc<crate::liquidations::backrun::BackrunDetector>>,
```

### 5B — In the tx evaluation loop (where opportunities are evaluated)

Find where decoded swaps are processed and add after the existing evaluation:

```rust
// After existing opportunity evaluation:
if let Some(ref detector) = self.backrun_detector {
    if let Some(swap) = decoded_as_swap(&tx) {
        if let Some(opp) = detector.evaluate(&swap).await {
            if self.execute_enabled {
                // Submit backrun bundle via Flashbots
                // bundle = [victim_tx_hash, our_backrun_tx]
                tracing::info!("🎯 Backrun queued: {} pnl=${:.2}", opp.token_symbol, opp.exp_profit_usd);
            }
        }
    }
}
```

---

## Step 6 — Update .env

Add the new variables from `upgrade/engine/.env.new_variables` to your existing `.env`.

---

## Step 7 — Build and Test

```bash
cd Arbitrage/arb-engine/engine

# Check everything compiles
cargo check

# Run in monitoring mode (no execution)
EXECUTE_ENABLED=false cargo run 2>&1 | grep -E "📊|✅|Phase"

# Expected output after ~30 seconds:
# ✅ Mega Token Universe started (10+ sources, 30s rescore)
# 📊 Universe | pools:8421 tokens:2847 | P1:312 P2:224 P3:687 P4:188
```

---

## Phase Rollout Schedule

```
Week 1:  EXECUTE_ENABLED=true         (Phase 1 only — safest)
Week 2:  CEX_DEX_ENABLED=true         (Phase 2 added)
Week 3:  LIQUIDATIONS_ENABLED=true    (Phase 3 liquidations)
Week 4:  BACKRUN_ENABLED=true         (Phase 3 backrunning)
Week 5:  CROSS_CHAIN_ENABLED=true     (Phase 4)
```

---

## Expected Token Counts After Integration

| Phase | Count | Filter |
|-------|-------|--------|
| Phase 1 DEX-DEX | 300-600 | TVL > $50K + 2+ pools same chain |
| Phase 2 CEX-DEX | 200-400 | Binance USDT perpetual listed |
| Phase 3 Backrun | 600-1000 | Vol > $1K in 24h (any token) |
| Phase 4 X-Chain | 200-400 | Present on 2+ chains |

---

## Troubleshooting

**`unresolved import crate::discovery`**
→ Missing `mod discovery { pub mod mega_scanner; }` in main.rs

**`GeckoTerminal rate limit (429)`**
→ Normal. The 200ms delay between pages handles this. Reduce max_pages from 10 to 5 if persistent.

**`Phase 2 shows 0 tokens`**
→ Binance fetch failed at startup. Check internet. It retries every hour.

**`cargo: tokio-tungstenite not found`**
→ `cargo update` after copying the new Cargo.toml

**`Phase 3 shows 0 tokens on first run`**
→ Normal — DeFiLlama takes ~15s to load. Wait for "✅ initial scan complete" log.
