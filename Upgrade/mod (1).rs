// ─────────────────────────────────────────────────────────────────────────────
//  liquidations/mod.rs — Phase 3: Advanced Mempool MEV
//
//  STRATEGIES:
//    A) Backrunning — detect large swaps in mempool that will move the price,
//       immediately trade after them to capture the price movement
//
//    B) Liquidations — monitor undercollateralized loans on Aave/Moonwell,
//       execute liquidations for the 5-8% bonus (often $50-200 per hit)
//
//  PIPELINE:
//    1. Private mempool feed (Bloxroute/Chainbound) → raw pending txs
//    2. BackrunDetector: decode swap calldata, estimate price impact
//    3. LiquidationMonitor: scan health factors every block
//    4. Executor: bundle-submit via Flashbots for guaranteed ordering
// ─────────────────────────────────────────────────────────────────────────────

pub mod backrun;
pub mod liquidation_monitor;
pub mod bundle_builder;
