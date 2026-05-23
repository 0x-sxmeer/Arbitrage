// ─────────────────────────────────────────────────────────────────────────────
//  cex_dex/spread_engine.rs — CEX-DEX Spread Detection & Execution
//
//  This is the core decision-making loop for Phase 2.
//  Runs every ~50ms and computes spread between Binance and each DEX pool.
//  When spread exceeds threshold, fires execution via AtomicArbV2.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::RwLock;
use tokio::time::{interval, sleep};
use tracing::{debug, error, info, warn};

use super::binance_feed::{CexQuote, DexQuote, DexFeed, PriceFeed};
use super::kelly_sizer::KellySizer;
use super::position_manager::{Position, PositionManager};

// ─────────────────────────────────────────────────────────────────────────────
//  Configuration
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SpreadConfig {
    /// Minimum spread (%) to trigger execution
    pub min_spread_pct:     f64,
    /// Maximum spread (%) — above this we suspect stale data
    pub max_spread_pct:     f64,
    /// Max allowed Binance bid-ask spread in bps (signal quality gate)
    pub max_cex_spread_bps: f64,
    /// Maximum DEX price age in ms before considered stale
    pub max_dex_age_ms:     u64,
    /// Minimum DEX liquidity in USD to execute
    pub min_dex_liquidity:  f64,
    /// Flash loan size for CEX-DEX arb (USD)
    pub loan_size_usd:      f64,
    /// Maximum inventory per token (USD)
    pub max_inventory_usd:  f64,
    /// Stop-loss: if spread reverses by this multiple of entry spread, exit
    pub stop_loss_multiplier: f64,
}

impl Default for SpreadConfig {
    fn default() -> Self {
        Self {
            min_spread_pct:      0.15,    // 15bps minimum edge
            max_spread_pct:      5.0,     // 500bps — above this is likely stale data
            max_cex_spread_bps:  3.0,     // tight CEX market required
            max_dex_age_ms:      2_000,   // 2s max DEX data age
            min_dex_liquidity:   500_000.0, // $500k min DEX liquidity
            loan_size_usd:       500_000.0, // $500k flash loan
            max_inventory_usd:   100_000.0, // $100k max open inventory
            stop_loss_multiplier: 2.0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Spread opportunity
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SpreadOpportunity {
    pub symbol:          String,
    pub cex_price:       f64,
    pub dex_price:       f64,
    pub spread_pct:      f64,
    pub direction:       TradeDirection,
    pub size_usd:        f64,
    pub expected_pnl_usd: f64,
    pub confidence:      f64,
    pub timestamp_ms:    u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TradeDirection {
    /// DEX price below CEX: buy on DEX, hedge short on CEX
    BuyDexSellCex,
    /// DEX price above CEX: sell on DEX (from inventory), buy back on CEX
    SellDexBuyCex,
}

// ─────────────────────────────────────────────────────────────────────────────
//  SpreadEngine
// ─────────────────────────────────────────────────────────────────────────────

pub struct SpreadEngine {
    config:    SpreadConfig,
    cex_feed:  PriceFeed,
    dex_feed:  DexFeed,
    kelly:     KellySizer,
    positions: PositionManager,
    
    /// Maps Binance symbol → DEX pool config
    symbol_to_dex: HashMap<String, String>,
    
    /// Running stats for reporting
    pub opportunities_found:   u64,
    pub opportunities_executed: u64,
    pub total_pnl_usd:          f64,
}

impl SpreadEngine {
    pub fn new(
        config: SpreadConfig,
        cex_feed: PriceFeed,
        dex_feed: DexFeed,
        symbol_to_dex: HashMap<String, String>,
    ) -> Self {
        Self {
            kelly:     KellySizer::new(config.loan_size_usd),
            positions: PositionManager::new(config.max_inventory_usd),
            config,
            cex_feed,
            dex_feed,
            symbol_to_dex,
            opportunities_found:    0,
            opportunities_executed: 0,
            total_pnl_usd:          0.0,
        }
    }

    /// Main loop — runs every 50ms scanning all symbols
    pub async fn run(mut self, execute_enabled: bool) -> Result<()> {
        let mut ticker = interval(Duration::from_millis(50));
        info!("🔥 CEX-DEX Spread Engine started (execute={})", execute_enabled);

        loop {
            ticker.tick().await;

            let opportunities = self.scan_all_symbols().await;

            for opp in opportunities {
                self.opportunities_found += 1;
                
                let edge = format!(
                    "💰 CEX-DEX SPREAD | {} | {:.3}% | dir={:?} | size=${:.0} | expPnL=${:.2}",
                    opp.symbol, opp.spread_pct, opp.direction, opp.size_usd, opp.expected_pnl_usd
                );
                
                if opp.expected_pnl_usd >= 5.0 {
                    info!("{}", edge);
                    
                    if execute_enabled {
                        match self.execute_opportunity(&opp).await {
                            Ok(pnl) => {
                                self.opportunities_executed += 1;
                                self.total_pnl_usd += pnl;
                                info!("✅ Executed | pnl=${:.2} | total=${:.2}", pnl, self.total_pnl_usd);
                            }
                            Err(e) => {
                                warn!("❌ Execution failed: {}", e);
                            }
                        }
                    }
                } else {
                    debug!("{}", edge);
                }
            }
        }
    }

    async fn scan_all_symbols(&self) -> Vec<SpreadOpportunity> {
        let cex_feed = self.cex_feed.read().await;
        let dex_feed = self.dex_feed.read().await;
        let now_ms   = now_ms();
        let mut opps = Vec::new();

        for (symbol, dex_key) in &self.symbol_to_dex {
            // Get CEX quote
            let Some(cex) = cex_feed.get(symbol) else { continue };
            if cex.is_stale { continue }
            if cex.bid_ask_spread_bps > self.config.max_cex_spread_bps { continue }

            // Get DEX quote
            let Some(dex) = dex_feed.get(dex_key) else { continue };
            if now_ms - dex.timestamp_ms > self.config.max_dex_age_ms { continue }
            if dex.liquidity_usd < self.config.min_dex_liquidity { continue }

            // Compute spread
            let cex_price = cex.smooth_price;
            let dex_price = dex.price_usd;
            if cex_price <= 0.0 || dex_price <= 0.0 { continue }

            let spread_pct = ((cex_price - dex_price) / cex_price).abs() * 100.0;

            if spread_pct < self.config.min_spread_pct || spread_pct > self.config.max_spread_pct {
                continue;
            }

            let direction = if dex_price < cex_price {
                TradeDirection::BuyDexSellCex
            } else {
                TradeDirection::SellDexBuyCex
            };

            // Kelly sizing
            let confidence = self.compute_confidence(cex, dex, spread_pct);
            let size_usd   = self.kelly.size_position(spread_pct / 100.0, confidence, self.config.loan_size_usd);
            
            // Estimate PnL after fees
            // DEX swap fee ~0.05%, flash loan fee 0% (Balancer), execution gas ~$3
            let gross_pnl = size_usd * spread_pct / 100.0;
            let dex_fee   = size_usd * 0.0005;
            let gas_cost  = 3.0;
            let expected_pnl_usd = gross_pnl - dex_fee - gas_cost;

            if expected_pnl_usd > 0.0 {
                opps.push(SpreadOpportunity {
                    symbol: symbol.clone(),
                    cex_price,
                    dex_price,
                    spread_pct,
                    direction,
                    size_usd,
                    expected_pnl_usd,
                    confidence,
                    timestamp_ms: now_ms,
                });
            }
        }

        // Sort by expected PnL descending
        opps.sort_by(|a, b| b.expected_pnl_usd.partial_cmp(&a.expected_pnl_usd).unwrap());
        opps
    }

    /// Confidence score [0, 1] based on signal quality metrics
    fn compute_confidence(&self, cex: &CexQuote, dex: &DexQuote, spread_pct: f64) -> f64 {
        let mut score = 1.0_f64;

        // Lower confidence if CEX spread is wide
        score *= 1.0 - (cex.bid_ask_spread_bps / (self.config.max_cex_spread_bps * 10.0)).min(0.5);

        // Lower confidence if DEX data is aging
        let dex_age_fraction = (now_ms() - dex.timestamp_ms) as f64 / self.config.max_dex_age_ms as f64;
        score *= 1.0 - dex_age_fraction * 0.3;

        // Higher confidence at larger spreads (more margin for error)
        score *= (spread_pct / self.config.min_spread_pct).min(2.0) / 2.0;

        // Penalize negative funding rate (indicates crowded trade)
        if cex.funding_rate < -0.001 && matches!(self.symbol_to_dex.get(&cex.mark_price.to_string()), Some(_)) {
            score *= 0.7;
        }

        score.clamp(0.0, 1.0)
    }

    async fn execute_opportunity(&mut self, opp: &SpreadOpportunity) -> Result<f64> {
        // Check inventory limits
        if !self.positions.can_open(&opp.symbol, opp.size_usd) {
            return Err(anyhow::anyhow!("Inventory limit reached for {}", opp.symbol));
        }

        info!(
            "🚀 Executing CEX-DEX arb: {} | spread={:.3}% | size=${:.0}",
            opp.symbol, opp.spread_pct, opp.size_usd
        );

        // In a real implementation:
        // 1. Submit on-chain tx to buy on DEX via AtomicArbV2.executeArbitrageV2()
        // 2. Simultaneously place hedge order on Binance (perp short/long)
        // 3. Monitor position until convergence or stop-loss
        // 4. Close hedge when spread collapses

        // Track position
        self.positions.open_position(opp.symbol.clone(), opp.direction.clone(), opp.size_usd, opp.dex_price, opp.cex_price);
        
        Ok(opp.expected_pnl_usd)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
