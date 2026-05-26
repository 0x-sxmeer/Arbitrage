# MEV Engine v2 — Mega Scanner Upgrade

## What This Package Does

Transforms the existing static-pool arb engine into a **self-updating,
10,000+ pool universe** with hundreds to thousands of tokens per phase.

```
Before:  ~20 hardcoded tokens per phase
After:   300-1000 live-scored tokens per phase, rotating every 30s
```

---

## Package Contents

```
MEV_Upgrade_Final/
├── INTEGRATION_GUIDE.md          ← READ FIRST — exact line-by-line instructions
├── contracts/
│   ├── AtomicArbV2.sol           ← Upgraded contract (Balancer + Yul + 5-hop)
│   └── DeployV2.s.sol            ← Foundry deploy script (Base/OP/ARB)
└── engine/src/
    ├── discovery/
    │   ├── mega_scanner.rs       ← Scans 10,000+ pools from 8 sources in parallel
    │   ├── factory_watcher.rs    ← Real-time new pool detection from 11 factories
    │   ├── whale_detector.rs     ← Whale movement tracking + score boosts
    │   └── mod.rs
    ├── scoring/
    │   ├── mega_scorer.rs        ← Scores all tokens with 7 signals, 30s rescore
    │   └── mod.rs
    ├── cex_dex/
    │   ├── binance_feed.rs       ← Binance WS mark prices (400 symbols at once)
    │   ├── spread_engine.rs      ← CEX-DEX divergence detection + execution
    │   └── mod.rs
    ├── liquidations/
    │   ├── liquidation_monitor.rs ← Aave V3 + Moonwell health factor scanning
    │   ├── backrun.rs             ← Backrun opportunity detector from mempool
    │   └── mod.rs
    ├── cross_chain/
    │   ├── cross_chain_engine.rs  ← Base↔OP↔ARB price divergence scanner
    │   ├── inventory_manager.rs   ← Multi-chain token inventory tracker
    │   └── mod.rs
    ├── main_patch.rs              ← Exact code to paste into existing main.rs
    ├── config_patch.rs            ← Exact fields to add to existing config.rs
    ├── Cargo.toml                 ← Updated with new dependencies
    └── .env.new_variables         ← New env vars to add to existing .env
```

---

## Token Count After Integration

| Phase | Count | Quality Gate | Signal |
|-------|-------|-------------|--------|
| **Phase 1 DEX-DEX** | 300-600 | TVL >$50K + 2+ pools | Vol/TVL ratio, pool count |
| **Phase 2 CEX-DEX** | 200-400 | Binance USDT perp listed | CEX price lag vs DEX |
| **Phase 3 Backrun** | 600-1000 | Vol >$1K in 24h | Whale score, trend signal |
| **Phase 4 X-Chain** | 200-400 | On 2+ chains, TVL >$10K | Inter-chain price gap |

**No rugs / no dead pools** — quality gates filter:
- TVL < $10K → excluded
- Vol < $1K/24h → excluded
- Both tokens are stablecoins → excluded
- Pool count = 0 → excluded

---

## 7 Live Scoring Signals

| Signal | Weight | What it measures |
|--------|--------|-----------------|
| Vol/TVL ratio | 35% | Price movement frequency — high = price desync between pools |
| Multi-pool | 18% | Same token on 3+ pools = triangle arb paths available |
| Whale score | 16% | Recent large wallet movements — boosts P3 backrun priority |
| CEX listing | 11% | Binance perp = P2 eligible, verified token (no rug risk) |
| Trend score | 9% | 1h vol spike vs 24h avg — trending tokens = wider spreads |
| Freshness | 5% | New pools have worst price sync = easiest arb |
| Liquidity | 4% | TVL score — enough depth to execute without huge slippage |

---

## Data Sources (All Parallel)

| Source | Pools | Refresh |
|--------|-------|---------|
| DeFiLlama /pools | ~8,000 | 5 min |
| GeckoTerminal ×10 pages ×9 chains | ~1,800 | 1 min |
| Uniswap V3 subgraph (5 chains) | ~7,500 | 2 min |
| Aerodrome subgraph (Base) | ~2,000 | 2 min |
| Velodrome subgraph (OP) | ~1,500 | 2 min |
| PancakeSwap subgraph (BNB) | ~500 | 2 min |
| Camelot subgraph (ARB) | ~400 | 2 min |
| Factory events (11 contracts) | real-time | instant |
| Binance exchange info | ~400 perps | 1 hr |

**Total: 12,000+ pools → 3,000+ unique tokens → 300-1000 active per phase**

---

## Revenue Model (All 4 Phases Active)

| Phase | Strategy | $/hr target | Key signal |
|-------|---------|------------|-----------|
| 1 | DEX-DEX flash arb | $2-5 | Vol/TVL ratio |
| 2 | CEX-DEX spread | $8-12 | Binance mark vs DEX lag |
| 3 | Backrunning | $3-5 | Whale swap detection |
| 3 | Liquidations | $2-4 | Health factor < 1.0 |
| 4 | Cross-chain | $3-5 | Inter-chain price gap |
| **Total** | | **$18-31/hr** | |

---

## Rollout Schedule

```
Week 1: EXECUTE_ENABLED=true only (Phase 1 — safest, 300+ tokens)
Week 2: + CEX_DEX_ENABLED=true (Phase 2 — 200+ Binance tokens)
Week 3: + LIQUIDATIONS_ENABLED=true (Phase 3 liquidations)
Week 4: + BACKRUN_ENABLED=true (Phase 3 backrunning)
Week 5: + CROSS_CHAIN_ENABLED=true (Phase 4)
```

Follow `INTEGRATION_GUIDE.md` for exact step-by-step implementation.
