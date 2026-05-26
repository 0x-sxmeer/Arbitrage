// ─────────────────────────────────────────────────────────────────────────────
//  engine/src/cex_dex/mod.rs — CEX-DEX Statistical Arbitrage Engine
//
//  PHASE 2: The Real Money Strategy
//
//  Architecture:
//    ┌──────────────────────────────────────────────────────────────────────┐
//    │  BinanceWsFeeder  ──→  PriceFeed (shared Arc<RwLock>)               │
//    │                              │                                        │
//    │  DexPricePoller   ──→  PriceFeed                                    │
//    │                              │                                        │
//    │  CexDexEngine reads both feeds, computes spread, sizes position,    │
//    │  fires execution order when spread > threshold                       │
//    └──────────────────────────────────────────────────────────────────────┘
//
//  How it works:
//    1. Binance WebSocket streams mark prices for ETH/USDT, WBTC/USDT, etc.
//    2. We simultaneously poll on-chain DEX prices (Uniswap V3 slot0).
//    3. If |CEX_price - DEX_price| / CEX_price > SPREAD_THRESHOLD:
//         → If DEX_price < CEX_price: BUY on DEX, hedge (short) on CEX perpetuals
//         → If DEX_price > CEX_price: SELL on DEX (from inventory), close hedge
//    4. Position sizing uses Kelly criterion with volatility-adjusted confidence.
//    5. Execution: atomic flash-loan tx via AtomicArbV2 contract.
//
//  Risk management:
//    • Maximum inventory cap per token
//    • Hedge ratio auto-adjusted by real-time funding rate
//    • Stop-loss triggers if spread reverses > 2x entry spread
// ─────────────────────────────────────────────────────────────────────────────

pub mod binance_feed;
pub mod dex_price;
pub mod spread_engine;
pub mod position_manager;
pub mod kelly_sizer;
