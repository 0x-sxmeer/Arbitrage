# MEV Engine v2 — $20/Hour Upgrade Package

> **From**: ~$1-5/hr DEX arbitrage  
> **To**: ~$18-31/hr institutional MEV  
> **Method**: Flash loans + CEX-DEX arb + liquidations + cross-chain

---

## Package Contents

```
mev_upgrade/
├── ROADMAP.md                          ← START HERE (step-by-step integration)
├── README.md                           ← This file
│
├── contracts/
│   ├── AtomicArbV2.sol                 ← Phase 1: New institutional contract
│   │   ├── Dual flash loans (Aave + Balancer 0% fee)
│   │   ├── Yul-optimized swap dispatcher
│   │   ├── Up to 5-hop multi-leg routing
│   │   └── Batch execution (3 arbs per tx)
│   └── DeployV2.s.sol                  ← Foundry deploy script (Base/OP/ARB)
│
└── engine/
    ├── Cargo.toml                      ← Updated dependencies
    ├── .env.example                    ← All new env vars documented
    └── src/
        ├── cex_dex/                    ← Phase 2: CEX-DEX Statistical Arbitrage
        │   ├── mod.rs
        │   ├── binance_feed.rs         ← Binance WebSocket mark-price feed
        │   ├── spread_engine.rs        ← Spread detection + execution
        │   ├── kelly_sizer.rs          ← Kelly criterion position sizing
        │   └── position_manager.rs     ← Inventory tracking + stop-losses
        │
        ├── liquidations/               ← Phase 3: Advanced Mempool MEV
        │   ├── mod.rs
        │   ├── backrun.rs              ← Backrunning strategy + impact estimation
        │   ├── liquidation_monitor.rs  ← Aave V3 + Moonwell health factor scanning
        │   └── bundle_builder.rs       ← Flashbots bundle composer + submitter
        │
        └── cross_chain/               ← Phase 4: Cross-Chain Arbitrage
            ├── mod.rs
            ├── chain_monitor.rs        ← Per-chain DEX price poller
            ├── cross_chain_engine.rs   ← Price divergence detector + executor
            ├── inventory_manager.rs    ← Multi-chain token inventory
            └── bridge_rebalancer.rs    ← LayerZero/Stargate rebalancing
```

---

## Quick Start

1. **Read** `ROADMAP.md` — it has exact file paths, code snippets, and commands
2. **Copy** contract + module files into the existing `Arbitrage/` codebase
3. **Update** `Cargo.toml` and `.env`
4. **Extend** `config.rs` and `main.rs` with the wiring code in ROADMAP
5. **Deploy** `AtomicArbV2` to Base (and later OP + ARB for Phase 4)
6. **Enable** phases one at a time, monitor 24h each

---

## What Each File Does

### `AtomicArbV2.sol`
The upgraded on-chain contract. Replaces `AtomicArb.sol` for new executions.

Key improvements over V1:
- **Balancer flash loans**: 0% fee (vs 0.05% on Aave) — saves ~$500/day on $1M daily volume
- **Yul dispatcher**: ~3,000 gas cheaper per swap vs. Solidity dispatch
- **5-hop routing**: captures complex triangle/quad arb paths V1 misses
- **`receiveFlashLoan`**: Balancer callback (new interface alongside existing Aave callback)

### `binance_feed.rs`
Connects to `wss://fstream.binance.com` and streams real-time mark prices for ETHUSDT, BTCUSDT, etc. Applies EWMA smoothing to filter noise. Marks prices stale after 5s without update.

### `spread_engine.rs`
The decision engine for Phase 2. Runs every 50ms:
1. Reads CEX price from `binance_feed`
2. Reads DEX price from on-chain pools
3. Computes spread; if > 0.15% after fees → execute
4. Uses `KellySizer` to size position optimally

### `backrun.rs`
Decodes pending swap calldata, estimates price impact using constant-product math, and generates `BackrunOpportunity` structs. Called from `mempool/listener.rs` on each pending tx.

### `liquidation_monitor.rs`
Tracks all borrowers on Aave V3 and Moonwell. Every block, checks health factors of at-risk positions. When HF < 1.0, builds a `LiquidationOpportunity` with exact repay/receive amounts.

### `bundle_builder.rs`
Assembles Flashbots bundles. For backruns: `[victim_tx, our_tx]`. For liquidations: single private tx via Flashbots Protect. Signs with the Flashbots auth key.

### `cross_chain_engine.rs`
Maintains a `PriceMatrix` indexed by `(ChainId, token_symbol)`. Scans all chain-pairs for each token. When spread > 0.30% net of gas and DEX fees, fires simultaneous execution on both chains using pre-positioned inventory.

### `inventory_manager.rs`
Tracks USDC, ETH, and other token balances on each chain. Checks if we have enough inventory before approving a cross-chain trade. Triggers bridge rebalancing when a chain runs low.

### `bridge_rebalancer.rs`
Moves tokens between chains asynchronously via Stargate (fast, 6bps fee) or native L2 bridges (free, slower). Never blocks the hot path — runs independently.

---

## Revenue Breakdown

```
Phase 1 (Flash Loan Arb):
  Capital:  $1M+ per trade (Balancer 0% fee)
  Volume:   ~$5M/day
  Profit:   0.02% net = $1,000/day = $42/hr
  Realistic with competition: $50-120/day = $2-5/hr

Phase 2 (CEX-DEX):
  Spread:   0.15-0.50% on ETHUSDT/BTCUSDT
  Frequency: 5-15 opportunities/hr
  Size:     $500k per trade
  Profit:   $8-12/hr when markets are volatile

Phase 3 (Backrunning):
  Target:   Swaps >$50k (impact > 20bps)
  Frequency: 2-8/hr
  Profit:   $30-150/backrun = $3-5/hr avg

Phase 3 (Liquidations):
  Frequency: 1-3/day on Base
  Profit:   $50-500/liquidation = $2-4/hr avg

Phase 4 (Cross-Chain):
  Divergences: Base↔OP↔ARB price gaps
  Frequency: 3-8/hr during volatility
  Profit:   $30-80/trade = $3-5/hr avg

TOTAL TARGET: $18-31/hr ($432-744/day)
```

---

## Risk Management

All strategies have hardcoded safeguards:

| Risk | Mitigation |
|------|------------|
| Contract exploit | V2 inherits V1 security: nonReentrant, onlyOwner, circuit breaker |
| Flash loan unprofitable | Inline Yul profit check — reverts entire tx if `finalAmount < repayAmount + minProfit` |
| CEX-DEX adverse selection | Kelly fraction = 0.25, max inventory cap, stop-loss at 2x entry spread |
| Liquidation front-run | Submitted via Flashbots Protect (private mempool) |
| Cross-chain inventory depletion | Per-chain minimum reserves enforced before each trade |
| Runaway losses | Circuit breaker: auto-pause if net loss > 0.5 ETH/hour |

---

## Support

- Integration issues → check `ROADMAP.md` "Common Integration Errors" section
- Contract verification → use Basescan with `forge verify-contract`
- CEX-DEX not firing → check `BINANCE_WS_URL` and symbol format (`ETHUSDT` not `ETH/USDT`)
