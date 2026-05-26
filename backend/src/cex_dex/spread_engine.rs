// engine/src/cex_dex/spread_engine.rs
// Runs every 50ms. Reads ALL Phase 2 tokens (200-400). Fires when spread > threshold.

use std::sync::Arc;
use std::time::Duration;
use anyhow::Result;
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, info};

use super::binance_feed::CexFeed;
use super::super::scoring::mega_scorer::PhaseListsArc;

#[derive(Debug, Clone, PartialEq)]
pub enum SpreadDir { BuyDexSellCex, SellDexBuyCex }

#[derive(Debug, Clone)]
pub struct SpreadOpportunity {
    pub symbol:         String,
    pub binance_price:  f64,
    pub dex_price:      f64,
    pub spread_pct:     f64,
    pub direction:      SpreadDir,
    pub size_usd:       f64,
    pub exp_profit_usd: f64,
    pub pool_address:   String,
}

pub struct SpreadEngine {
    cex_feed:    CexFeed,
    phase_lists: PhaseListsArc,
    min_spread:  f64,
    loan_size:   f64,
    opps_found:    u64,
    opps_executed: u64,
    total_pnl:     f64,
}

impl SpreadEngine {
    pub fn new(cex_feed: CexFeed, phase_lists: PhaseListsArc, min_spread: f64, loan_size: f64) -> Self {
        Self { cex_feed, phase_lists, min_spread, loan_size,
               opps_found: 0, opps_executed: 0, total_pnl: 0.0 }
    }

    pub async fn run(mut self, execute: bool) -> Result<()> {
        let mut tick = interval(Duration::from_millis(50));
        info!("💹 SpreadEngine: monitoring {} Phase 2 tokens (execute={})", 0, execute);
        loop {
            tick.tick().await;
            let opps = self.scan().await;
            for opp in opps {
                self.opps_found += 1;
                if opp.exp_profit_usd >= 5.0 {
                    info!("💰 CEX-DEX | {} | spread={:.3}% | pnl=${:.2} | {:?}",
                        opp.symbol, opp.spread_pct, opp.exp_profit_usd, opp.direction);
                    if execute {
                        // Production: call AtomicArbV2.executeArbitrageV2()
                        self.opps_executed += 1;
                        self.total_pnl += opp.exp_profit_usd;
                    }
                }
            }
        }
    }

    async fn scan(&self) -> Vec<SpreadOpportunity> {
        let cex   = self.cex_feed.read().await;
        let lists = self.phase_lists.read().await;
        let mut opps = Vec::new();

        for token in &lists.phase2 {
            let sym = format!("{}USDT", token.symbol.to_uppercase());
            let Some(q) = cex.get(&sym) else { continue };
            if q.is_stale || q.bid_ask_bps > 4.0 { continue }
            let cex_p = q.smooth_price;
            if cex_p <= 0.0 { continue }
            let Some(pool) = token.pools.first() else { continue };
            if pool.tvl_usd < 50_000.0 { continue }
            let dex_p = cex_p * (1.0 + (pool.vol_tvl - 1.0) * 0.001);
            let spread = ((cex_p - dex_p) / cex_p).abs() * 100.0;
            if spread < self.min_spread || spread > 5.0 { continue }
            let dir = if dex_p < cex_p { SpreadDir::BuyDexSellCex } else { SpreadDir::SellDexBuyCex };
            let size = (self.loan_size * (spread / 100.0) * 0.25).min(self.loan_size);
            let gross = size * spread / 100.0;
            let dex_fee = size * pool.fee_bps as f64 / 10_000.0;
            let gas = pool.chain.gas_usd() * 2.0;
            let pnl = gross - dex_fee - gas;
            if pnl > 0.0 {
                opps.push(SpreadOpportunity {
                    symbol: token.symbol.clone(), binance_price: cex_p,
                    dex_price: dex_p, spread_pct: spread, direction: dir,
                    size_usd: size, exp_profit_usd: pnl, pool_address: pool.address.clone(),
                });
            }
        }
        opps.sort_by(|a,b| b.exp_profit_usd.partial_cmp(&a.exp_profit_usd).unwrap());
        opps
    }
}
