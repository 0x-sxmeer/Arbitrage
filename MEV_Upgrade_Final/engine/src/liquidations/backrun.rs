// engine/src/liquidations/backrun.rs
//
// Detects large pending swaps in mempool and builds backrun bundles.
// Uses the live Phase 3 token list (600-1000 tokens) for matching.
// Whale alert → score boost → token rises in P3 ranking → more attention.

use std::sync::Arc;
use anyhow::Result;
use tracing::{debug, info, warn};

use super::super::scoring::mega_scorer::{PhaseListsArc, WhaleScores};

#[derive(Debug, Clone)]
pub struct PendingSwap {
    pub tx_hash:      String,
    pub from:         String,
    pub router:       String,
    pub token_in:     String,
    pub token_out:    String,
    pub amount_in_usd: f64,
    pub pool_address:  String,
    pub gas_price:     u128,
    pub chain_id:      u64,
}

#[derive(Debug, Clone)]
pub struct BackrunOpportunity {
    pub target_tx:      String,
    pub token_symbol:   String,
    pub pool_address:   String,
    pub impact_bps:     f64,
    pub exp_profit_usd: f64,
    pub size_usd:       f64,
    pub gas_premium:    u128,  // slightly above victim gas price
}

pub struct BackrunDetector {
    phase_lists:  PhaseListsArc,
    whale_scores: WhaleScores,
    min_size_usd: f64,   // minimum swap size to care about
    min_profit:   f64,
}

impl BackrunDetector {
    pub fn new(phase_lists: PhaseListsArc, whale_scores: WhaleScores) -> Self {
        Self {
            phase_lists,
            whale_scores,
            min_size_usd: 50_000.0,  // ignore swaps < $50K
            min_profit:   10.0,       // min $10 profit
        }
    }

    /// Called from mempool listener for every decoded pending swap
    /// Returns a BackrunOpportunity if the swap is worth following
    pub async fn evaluate(&self, swap: &PendingSwap) -> Option<BackrunOpportunity> {
        if swap.amount_in_usd < self.min_size_usd { return None; }

        // Look up token in Phase 3 list — any token with volume is trackable
        let lists = self.phase_lists.read().await;
        let matched = lists.phase3.iter().find(|t| {
            t.pools.iter().any(|p|
                p.token0_addr.eq_ignore_ascii_case(&swap.token_in) ||
                p.token1_addr.eq_ignore_ascii_case(&swap.token_in) ||
                p.token0_addr.eq_ignore_ascii_case(&swap.token_out) ||
                p.token1_addr.eq_ignore_ascii_case(&swap.token_out)
            )
        });

        let token = matched?;

        // Estimate price impact using simplified constant-product formula
        // impact_bps ≈ (trade_size / pool_tvl) * 10000
        let best_pool = token.pools.first()?;
        if best_pool.tvl_usd < 10_000.0 { return None; }
        let impact_bps = (swap.amount_in_usd / best_pool.tvl_usd * 10_000.0).min(500.0);
        if impact_bps < 15.0 { return None; } // not enough impact to backrun

        // Backrun size: ~30% of victim trade size
        let backrun_size = swap.amount_in_usd * 0.30;
        let gross_profit = backrun_size * (impact_bps / 2.0) / 10_000.0;
        let gas_cost     = 0.15; // ~$0.15 on Arbitrum
        let net_profit   = gross_profit - gas_cost;

        if net_profit < self.min_profit { return None; }

        info!(
            "🎯 BACKRUN | {} | victim=${:.0} | impact={:.1}bps | pnl=${:.2}",
            token.symbol, swap.amount_in_usd, impact_bps, net_profit
        );

        // Boost whale score for this token (decays over time)
        let score_boost = (swap.amount_in_usd / 100_000.0 * 30.0).min(90.0);
        self.whale_scores.write().await
            .insert(token.symbol.clone(), score_boost);

        Some(BackrunOpportunity {
            target_tx:    swap.tx_hash.clone(),
            token_symbol: token.symbol.clone(),
            pool_address: swap.pool_address.clone(),
            impact_bps,
            exp_profit_usd: net_profit,
            size_usd:     backrun_size,
            gas_premium:  swap.gas_price * 1010 / 1000, // 1% above victim
        })
    }

    /// Stats summary
    pub fn log_stats(&self, found: u64, executed: u64) {
        info!("🎯 Backrun stats | found: {} | executed: {}", found, executed);
    }
}
