// ─────────────────────────────────────────────────────────────────────────────
//  liquidations/backrun.rs — Mempool Backrunning Strategy
//
//  Backrunning = detecting a large pending swap that will move the price,
//  then immediately following it with our own trade to capture the movement.
//
//  Example:
//    1. Whale tx detected: swap 500 ETH → USDC on Uniswap (will push price down)
//    2. We detect it with 0.5-block advantage via private mempool feed
//    3. We submit a bundle: [whale_tx, our_backrun_tx]
//    4. our_backrun_tx: sell ETH on pool that hasn't repriced yet, buy USDC
//    5. Net: ~$150 profit for a well-sized backrun
//
//  This is legal and encouraged by MEV researchers — it adds price discovery.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// ─────────────────────────────────────────────────────────────────────────────
//  Pending transaction representation
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PendingSwap {
    pub tx_hash:      String,
    pub from:         String,
    pub to:           String,       // router address
    pub token_in:     String,
    pub token_out:    String,
    pub amount_in:    u128,
    pub min_out:      u128,
    pub slippage_bps: u32,
    pub gas_price:    u128,
    pub pool_address: String,
    pub dex:          DexProtocol,
}

#[derive(Debug, Clone)]
pub enum DexProtocol { UniswapV2, UniswapV3, Aerodrome, AerodromeSlipstream }

// ─────────────────────────────────────────────────────────────────────────────
//  Price impact estimation
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PriceImpact {
    /// Estimated price change in basis points
    pub impact_bps:       f64,
    /// Estimated price after swap (in token_out per token_in)
    pub price_after:      f64,
    /// USD value of the trade
    pub trade_size_usd:   f64,
    /// Estimated backrun profit in USD
    pub backrun_profit_usd: f64,
}

// ─────────────────────────────────────────────────────────────────────────────
//  BackrunDetector
// ─────────────────────────────────────────────────────────────────────────────

pub struct BackrunDetector {
    /// Minimum price impact to consider backrunning (bps)
    min_impact_bps: f64,
    /// Minimum expected profit in USD
    min_profit_usd: f64,
    /// Pool states: pool_address → (reserve0, reserve1, fee)
    pool_states: Arc<RwLock<HashMap<String, PoolState>>>,
    /// Token prices in USD
    token_prices: Arc<RwLock<HashMap<String, f64>>>,
}

#[derive(Debug, Clone)]
pub struct PoolState {
    pub reserve0:     u128,
    pub reserve1:     u128,
    pub token0:       String,
    pub token1:       String,
    pub fee_bps:      u32,
    pub sqrt_price:   Option<u128>,  // V3 only
    pub liquidity:    Option<u128>,  // V3 only
    pub tick_current: Option<i32>,   // V3 only
}

impl BackrunDetector {
    pub fn new(
        min_impact_bps: f64,
        min_profit_usd: f64,
        pool_states: Arc<RwLock<HashMap<String, PoolState>>>,
        token_prices: Arc<RwLock<HashMap<String, f64>>>,
    ) -> Self {
        Self { min_impact_bps, min_profit_usd, pool_states, token_prices }
    }

    /// Evaluate a pending swap for backrun opportunity
    pub async fn evaluate(&self, swap: &PendingSwap) -> Option<BackrunOpportunity> {
        let pool_states = self.pool_states.read().await;
        let token_prices = self.token_prices.read().await;

        let pool = pool_states.get(&swap.pool_address)?;
        let token_in_price  = token_prices.get(&swap.token_in)?;
        let token_out_price = token_prices.get(&swap.token_out)?;

        let trade_size_usd = swap.amount_in as f64 / 1e18 * token_in_price;

        // Estimate price impact using constant product formula
        let impact = match swap.dex {
            DexProtocol::UniswapV2 | DexProtocol::Aerodrome => {
                self.estimate_v2_impact(pool, swap.amount_in, token_in_price, token_out_price)
            }
            DexProtocol::UniswapV3 | DexProtocol::AerodromeSlipstream => {
                self.estimate_v3_impact(pool, swap.amount_in, token_in_price, token_out_price)
            }
        }?;

        if impact.impact_bps < self.min_impact_bps { return None; }
        if impact.backrun_profit_usd < self.min_profit_usd { return None; }

        Some(BackrunOpportunity {
            target_tx_hash:  swap.tx_hash.clone(),
            pool_address:    swap.pool_address.clone(),
            token_in:        swap.token_out.clone(),  // we trade in opposite direction post-swap
            token_out:       swap.token_in.clone(),
            optimal_amount:  self.compute_optimal_backrun_size(pool, &impact),
            expected_profit: impact.backrun_profit_usd,
            impact_bps:      impact.impact_bps,
            gas_premium_bps: 10,  // pay 10bps more than the target tx
            dex:             swap.dex.clone(),
        })
    }

    fn estimate_v2_impact(
        &self,
        pool: &PoolState,
        amount_in: u128,
        token_in_price: &f64,
        token_out_price: &f64,
    ) -> Option<PriceImpact> {
        // x*y=k constant product
        let r_in  = pool.reserve0 as f64;
        let r_out = pool.reserve1 as f64;
        let fee   = 1.0 - pool.fee_bps as f64 / 10_000.0;

        let amount_in_f = amount_in as f64;
        let amount_out  = (r_out * amount_in_f * fee) / (r_in + amount_in_f * fee);

        let price_before = r_out / r_in;
        let price_after  = (r_out - amount_out) / (r_in + amount_in_f);
        let impact_bps   = ((price_after - price_before) / price_before).abs() * 10_000.0;

        // Backrun profit: trade in opposite direction, capture half the impact
        let backrun_size_usd  = amount_in_f / 1e18 * token_in_price * 0.3;
        let backrun_profit_usd = backrun_size_usd * (impact_bps / 2.0) / 10_000.0;

        Some(PriceImpact {
            impact_bps,
            price_after,
            trade_size_usd: amount_in_f / 1e18 * token_in_price,
            backrun_profit_usd,
        })
    }

    fn estimate_v3_impact(
        &self,
        pool: &PoolState,
        amount_in: u128,
        token_in_price: &f64,
        token_out_price: &f64,
    ) -> Option<PriceImpact> {
        // Simplified V3 impact estimation using sqrt price
        // Full tick-crossing math would require the complete tick bitmap
        let sqrt_price = pool.sqrt_price? as f64;
        let liquidity  = pool.liquidity? as f64;

        // Current price: (sqrt_price / 2^96)^2
        let current_price = (sqrt_price / 2f64.powi(96)).powi(2);

        // Approximate impact for in-range swap
        let amount_f = amount_in as f64;
        let delta_sqrt = amount_f / (liquidity * 2f64.powi(96) / sqrt_price);
        let new_sqrt   = sqrt_price + delta_sqrt;
        let new_price  = (new_sqrt / 2f64.powi(96)).powi(2);
        let impact_bps = ((new_price - current_price) / current_price).abs() * 10_000.0;

        let backrun_profit_usd = amount_f / 1e18 * token_in_price * (impact_bps / 2.0) / 10_000.0;

        Some(PriceImpact {
            impact_bps,
            price_after: new_price,
            trade_size_usd: amount_f / 1e18 * token_in_price,
            backrun_profit_usd,
        })
    }

    fn compute_optimal_backrun_size(&self, pool: &PoolState, impact: &PriceImpact) -> u128 {
        // Optimal size ≈ sqrt(k) / 4 derived from derivative of profit function
        // In practice, we size to 30% of the original trade for safety
        let pool_tvl = pool.reserve0 as f64 * 2.0;
        let optimal_pct = 0.30_f64.min(impact.impact_bps / 200.0);  // scale with impact
        (pool_tvl * optimal_pct) as u128
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  BackrunOpportunity — passed to bundle builder
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BackrunOpportunity {
    /// Hash of the transaction we're backrunning
    pub target_tx_hash:  String,
    pub pool_address:    String,
    pub token_in:        String,
    pub token_out:       String,
    pub optimal_amount:  u128,
    pub expected_profit: f64,
    pub impact_bps:      f64,
    /// Gas premium over target tx to ensure ordering
    pub gas_premium_bps: u32,
    pub dex:             DexProtocol,
}
