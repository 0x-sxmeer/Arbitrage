// ─────────────────────────────────────────────────────────────────────────────
//  arb/opportunity.rs — Arbitrage opportunity + Net Expected Value calculator
//
//  NEV = gross_spread
//      - gas_cost_wei           (EIP-1559 base fee + priority tip × gas units)
//      - Σ swap_fees_wei        (protocol fees on each hop)
//      - price_impact_loss      (market impact of our own trade)
//
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::pool::{ChainId, U256};

// ─────────────────────────────────────────────────────────────────────────────
//  Core types
// ─────────────────────────────────────────────────────────────────────────────

/// A discovered arbitrage opportunity between two or more pools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    /// Unique ID for logging and deduplication
    pub id: Uuid,
    /// Ordered list of swap steps that form the arbitrage route
    pub route: Vec<SwapStep>,
    /// Starting token address
    pub start_token: String,
    /// Chain where execution begins (for gas estimation)
    pub chain: ChainId,

    // ── Trade sizing ──────────────────────────────────────────────────────────
    /// Raw input amount (in start_token wei)
    pub input_amount: U256,
    /// Expected output after all swaps (in start_token wei)
    pub gross_output: U256,

    // ── Cost breakdown ────────────────────────────────────────────────────────
    /// Estimated EVM gas units (typical arb tx: 200k–500k gas)
    pub estimated_gas_units: u64,
    /// EIP-1559 effective gas price in gwei (base fee + priority tip)
    pub gas_price_gwei: f64,
    /// Total swap protocol fees across all hops (in input token wei)
    pub total_swap_fees_wei: U256,
    /// Aggregate price impact in basis points
    pub price_impact_bps: u32,

    // ── Final verdict ─────────────────────────────────────────────────────────
    /// Net Expected Value: positive = profitable. Signed i128 (wei)
    pub net_expected_value: i128,
    /// Whether NEV exceeds the minimum profit threshold
    pub is_executable: bool,
    /// ETH block number when this opportunity was found
    pub discovered_at_block: u64,
    /// Wall-clock time when discovered
    pub discovered_at: DateTime<Utc>,
}

/// A single swap leg within an arbitrage route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapStep {
    pub pool_id: String,
    pub dex: String,
    pub chain: ChainId,
    pub token_in: String,
    pub token_out: String,
    /// Raw input amount (wei / lamports / uatom)
    pub amount_in: U256,
    /// Expected output (simulated, not guaranteed)
    pub expected_amount_out: U256,
    /// Protocol fee in basis points (e.g. 30 = 0.3%)
    pub fee_bps: u32,
    /// Price impact for this individual hop (basis points)
    pub step_price_impact_bps: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
//  NEV Calculator
// ─────────────────────────────────────────────────────────────────────────────

impl ArbitrageOpportunity {
    /// Minimum profit in wei to justify execution.
    /// ~$0.50 at ETH = $3,000 → 0.5/3000 × 10^18 ≈ 1.67 × 10^14 wei
    pub const MIN_PROFIT_WEI: i128 = 166_666_666_666_666; // $0.50 at $3k ETH

    /// Construct a new opportunity shell — call `calculate_nev()` to populate verdict.
    pub fn new(
        route: Vec<SwapStep>,
        start_token: String,
        chain: ChainId,
        input_amount: U256,
        gross_output: U256,
        estimated_gas_units: u64,
        gas_price_gwei: f64,
        total_swap_fees_wei: U256,
        price_impact_bps: u32,
        discovered_at_block: u64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            route,
            start_token,
            chain,
            input_amount,
            gross_output,
            estimated_gas_units,
            gas_price_gwei,
            total_swap_fees_wei,
            price_impact_bps,
            net_expected_value: 0,
            is_executable: false,
            discovered_at_block,
            discovered_at: Utc::now(),
        }
    }

    /// Calculate NEV and set `is_executable`.
    ///
    /// All costs must be denominated in the same unit as `input_amount` (wei).
    /// Pass `eth_price_usd` for USD-denominated logging only.
    pub fn calculate_nev(&mut self, eth_price_usd: f64, aave_fee_bps: u32) {
        let decimals = match self.start_token.to_lowercase().as_str() {
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913" | "0xfde4c96c8593536e31f229ea8f37b2ada2699bb2" => 6, // USDC / USDT
            "0x0555e30da8f98308edb960aa94c0db47230d2b9c" => 8, // WBTC
            _ => 18, // WETH / DAI / etc.
        };
        let scaling_factor = 10i128.pow((18 - decimals) as u32);
        let token_price = match self.start_token.to_lowercase().as_str() {
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913" | "0xfde4c96c8593536e31f229ea8f37b2ada2699bb2" | "0x50c5725949a6f0c72e6c4a641f24049a917db0cb" => 1.0,
            "0x0555e30da8f98308edb960aa94c0db47230d2b9c" => eth_price_usd * 20.0,
            _ => eth_price_usd,
        };

        // 1. Gross spread: how much more we get back vs what we put in
        let gross_profit = self.gross_output.low_u128() as i128
            - self.input_amount.low_u128() as i128;
        let gross_profit_scaled = gross_profit * scaling_factor;

        // 2. Gas cost: gas_units × effective_gas_price_gwei × 10^9 (gwei → wei)
        let gas_cost_wei = (self.estimated_gas_units as f64
            * self.gas_price_gwei
            * 1_000_000_000.0) as i128;
        let gas_cost_scaled = (gas_cost_wei as f64 * eth_price_usd / token_price) as i128;

        // 3. Protocol swap fees (aggregated across all hops)
        let swap_fees = self.total_swap_fees_wei.low_u128() as i128;
        let swap_fees_scaled = swap_fees * scaling_factor;

        // 4. Price impact loss: our trade moves the price against us
        //    Approximation: impact_bps/10000 × input_amount
        let impact_loss = (self.input_amount.low_u128() as f64
            * self.price_impact_bps as f64
            / 10_000.0) as i128;
        let impact_loss_scaled = impact_loss * scaling_factor;

        // 5. Aave flashloan fee: borrowing cost
        let aave_fee_loss = (self.input_amount.low_u128() as f64
            * aave_fee_bps as f64
            / 10_000.0) as i128;
        let aave_fee_loss_scaled = aave_fee_loss * scaling_factor;

        // 6. Net Expected Value
        self.net_expected_value = gross_profit_scaled - gas_cost_scaled - swap_fees_scaled - impact_loss_scaled - aave_fee_loss_scaled;
        let nev_usd  = (self.net_expected_value as f64 / 1e18) * token_price;
        self.is_executable = nev_usd >= 0.50;

        // Logging
        let gross_usd = gross_profit_scaled as f64 / 1e18 * token_price;
        let gas_usd   = gas_cost_scaled as f64 / 1e18 * token_price;

        if self.is_executable {
            tracing::info!(
                id = %self.id,
                nev_usd = format!("${:.4}", nev_usd),
                gross_usd = format!("${:.4}", gross_usd),
                gas_usd = format!("${:.4}", gas_usd),
                hops = self.route.len(),
                impact_bps = self.price_impact_bps,
                block = self.discovered_at_block,
                "✅ EXECUTABLE arbitrage opportunity found"
            );
        } else {
            tracing::debug!(
                id = %self.id,
                nev_usd = format!("${:.4}", nev_usd),
                gross_usd = format!("${:.4}", gross_usd),
                reason = if gross_profit_scaled <= 0 { "no spread" }
                         else if gas_cost_wei > gross_profit_scaled { "gas > spread" }
                         else { "below threshold" },
                "❌ Non-profitable opportunity"
            );
        }
    }

    /// Gross profit as a percentage of input (pre-cost return).
    pub fn gross_return_bps(&self) -> u32 {
        let gross = self.gross_output.low_u128() as f64;
        let input = self.input_amount.low_u128() as f64;
        if input == 0.0 { return 0; }
        ((gross / input - 1.0) * 10_000.0).max(0.0) as u32
    }

    /// Route description for logging: e.g. "WETH → USDC (UniV3) → WETH (SushiSwap)"
    pub fn route_description(&self) -> String {
        self.route
            .iter()
            .map(|s| format!("{} → {} ({})", s.token_in, s.token_out, s.dex))
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn make_opportunity(input: u128, output: u128, gas_units: u64, gas_gwei: f64) -> ArbitrageOpportunity {
        ArbitrageOpportunity::new(
            vec![],
            "WETH".into(),
            ChainId::Ethereum,
            U256::from(input),
            U256::from(output),
            gas_units,
            gas_gwei,
            U256::zero(),
            5, // 0.05% price impact
            19_000_000,
        )
    }

    #[test]
    fn test_profitable_opportunity() {
        // Profitable: 1 ETH in, 1.01 ETH out, 200k gas at 20 gwei
        // Gas cost = 200_000 × 20 × 10^9 = 4 × 10^15 wei = $0.012 at $3k
        // Gross profit = 0.01 ETH = 10^16 wei = $30
        let mut opp = make_opportunity(
            1_000_000_000_000_000_000,  // 1 ETH
            1_010_000_000_000_000_000,  // 1.01 ETH
            200_000,
            20.0, // 20 gwei
        );
        opp.calculate_nev(3000.0, 5);
        assert!(opp.is_executable, "Should be executable");
        assert!(opp.net_expected_value > 0);
    }

    #[test]
    fn test_gas_exceeds_profit() {
        // Not profitable: tiny spread but high gas
        // Gross profit: 0.0005 ETH = 5 × 10^14 wei
        // Gas: 300k × 50 gwei = 1.5 × 10^16 wei >> profit
        let mut opp = make_opportunity(
            1_000_000_000_000_000_000,
            1_000_500_000_000_000_000,
            300_000,
            50.0,
        );
        opp.calculate_nev(3000.0, 5);
        assert!(!opp.is_executable, "High gas should make this non-executable");
        assert!(opp.net_expected_value < 0);
    }

    #[test]
    fn test_gross_return_bps() {
        let opp = make_opportunity(
            1_000_000_000_000_000_000,
            1_010_000_000_000_000_000,
            200_000,
            20.0,
        );
        // 1% return = 100 bps
        assert_eq!(opp.gross_return_bps(), 100);
    }
}
