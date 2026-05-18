// arb/latency_hedger.rs
//
// ─── Phase 4: Cross-Chain Bridge Arbitrage & Inventory ──────────────────────────
//
// Models statistical spread decay, block finality risk, and execution latency.
// Since cross-chain execution takes time (block inclusion + bridge relay), the price
// spreads between chains decay rapidly. This module calculates the probability of
// transaction reversion or slippage before both legs are mined.

use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyRiskParameters {
    pub average_block_time_ms: u64,
    pub target_inclusion_probability: f64,
    pub spread_decay_coefficient: f64, // Exponential decay rate of arbitrage spread
}

pub struct LatencyHedger {
    pub params: LatencyRiskParameters,
}

impl LatencyHedger {
    pub fn new(average_block_time_ms: u64, spread_decay_coefficient: f64) -> Self {
        Self {
            params: LatencyRiskParameters {
                average_block_time_ms,
                target_inclusion_probability: 0.95,
                spread_decay_coefficient,
            },
        }
    }

    /// Evaluates if a cross-chain opportunity is statistically profitable after accounting
    /// for expected spread decay over the transaction inclusion latency.
    ///
    /// Formula:
    ///   Net Expected Profit = Gross Spread * e^(-decay_coefficient * (latency_ms / 1000)) - fixed_gas_fee
    pub fn evaluate_execution_risk(
        &self,
        gross_spread_usd: f64,
        total_latency_ms: u64,
        gas_cost_usd: f64,
        min_net_profit_threshold: f64,
    ) -> (bool, f64) {
        let latency_seconds = total_latency_ms as f64 / 1000.0;
        
        // Exponential decay model
        let expected_spread_usd = gross_spread_usd * (-self.params.spread_decay_coefficient * latency_seconds).exp();
        let net_expected_profit = expected_spread_usd - gas_cost_usd;

        let should_execute = net_expected_profit > min_net_profit_threshold;

        (should_execute, net_expected_profit)
    }

    /// Calculates the probability of price reversion (price moving against us) on the target chain.
    /// Uses standard geometric brownian motion simplified probability function.
    pub fn calculate_reversion_probability(
        &self,
        latency_ms: u64,
        volatility_daily: f64, // e.g. 0.08 for 8% daily volatility
        spread_bps: u32,
    ) -> f64 {
        let t = latency_ms as f64 / (1000.0 * 60.0 * 60.0 * 24.0); // time in days
        let volatility_t = volatility_daily * t.sqrt();
        let spread_pct = spread_bps as f64 / 10000.0;

        if volatility_t == 0.0 {
            return 0.0;
        }

        // Standard Normal Cumulative Distribution Approximation (simplified)
        let z = spread_pct / volatility_t;
        
        // Simple sigmoid/logistic approximation of 1 - N(z) for reversion probability
        let reversion_prob = 1.0 / (1.0 + (1.702 * z).exp());
        reversion_prob
    }
}
