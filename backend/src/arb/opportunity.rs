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
    /// Maximum gas price we can bid (in gwei) while still achieving min_profit_usd
    pub optimal_gas_price_gwei: f64,
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
            optimal_gas_price_gwei: gas_price_gwei,
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
    pub fn calculate_nev(
        &mut self,
        eth_price_usd: f64,
        btc_price_usd: f64,
        aave_fee_bps: u32,
        min_profit_usd: f64,
    ) {
        let decimals: u32 = match self.start_token.to_lowercase().as_str() {
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            | "0xfde4c96c8593536e31f229ea8f37b2ada2699bb2" => 6, // USDC / USDT
            "0x0555e30da8f98308edb960aa94c0db47230d2b9c" => 8, // WBTC
            _ => 18,                                           // WETH / DAI / etc.
        };

        let token_price = match self.start_token.to_lowercase().as_str() {
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            | "0xfde4c96c8593536e31f229ea8f37b2ada2699bb2"
            | "0x50c5725949a6f0c72e6c4a641f24049a917db0cb" => 1.0,
            "0x0555e30da8f98308edb960aa94c0db47230d2b9c" => btc_price_usd,
            _ => eth_price_usd,
        };

        // Use f64 throughout for NEV — precision is sufficient for USD comparisons
        // and avoids i128 overflow on scaled values.
        let scale = 10f64.powi(decimals as i32);

        // Safely convert U256 → f64 without truncation via to_string parsing
        let gross_out_raw = self.gross_output.to_string().parse::<f64>().unwrap_or(0.0);
        let input_amt_raw = self.input_amount.to_string().parse::<f64>().unwrap_or(0.0);
        let gross_out = gross_out_raw / scale;
        let input_amt = input_amt_raw / scale;

        // ── Sanity guards: reject simulation artifacts ────────────────────────
        // Guard 1: Input amount must be < $100,000 USD (hard cap on flash loan size)
        let input_usd = input_amt * token_price;
        if input_usd > 100_000.0 {
            tracing::debug!(id = %self.id, input_usd, "❌ Rejected: input > $100k (simulation artifact)");
            self.net_expected_value = 0;
            self.is_executable = false;
            return;
        }
        // Guard 2: Gross profit percentage > 5% is unrealistic for liquid on-chain pools
        // (2% was too tight — real 3-hop arbs can legitimately show 2-4% spread during high volatility)
        let gross_profit_pct = if input_amt > 0.0 {
            (gross_out / input_amt - 1.0) * 100.0
        } else {
            0.0
        };
        if gross_profit_pct > 5.0 {
            // Silently drop simulation artifacts without spamming the logs
            self.net_expected_value = 0;
            self.is_executable = false;
            return;
        }
        // ─────────────────────────────────────────────────────────────────────

        let gross_profit_token = gross_out - input_amt;

        // Gas cost in token units (gas_wei × eth_price / token_price / 1e18)
        let gas_cost_eth = self.estimated_gas_units as f64 * self.gas_price_gwei * 1e-9; // ETH
        let gas_cost_token = gas_cost_eth * eth_price_usd / token_price;

        // Note: swap_fees_token and impact_loss_token are already accounted for
        // inside the AMM simulation (gross_out). We ONLY deduct external costs here.
        let aave_fee_token = input_amt * aave_fee_bps as f64 / 10_000.0;

        // CORRECTED NEV CALCULATION
        let nev_token = gross_profit_token - gas_cost_token - aave_fee_token;

        let nev_usd = nev_token * token_price;

        // CORRECTED PGA GAS (Bribe) CALCULATION
        // Max USD we can spend on gas while preserving the minimum required profit
        let max_gas_spend_usd =
            (gross_profit_token - aave_fee_token) * token_price - min_profit_usd;
        if max_gas_spend_usd > 0.0 && self.estimated_gas_units > 0 {
            let max_gas_spend_eth = max_gas_spend_usd / eth_price_usd;
            self.optimal_gas_price_gwei =
                max_gas_spend_eth * 1e9 / (self.estimated_gas_units as f64);
        } else {
            self.optimal_gas_price_gwei = self.gas_price_gwei;
        }

        // Sanity guard 3: nev_usd > $10,000 is almost certainly a simulation artifact
        if nev_usd > 10_000.0 {
            tracing::debug!(id = %self.id, nev_usd, "❌ Rejected: NEV > $10k (simulation artifact)");
            self.net_expected_value = 0;
            self.is_executable = false;
            return;
        }

        // Store as wei-equivalent i128 for DB (always in 18-dec regardless of token)
        let nev_wei_f64 = nev_token * token_price / eth_price_usd * 1e18;
        self.net_expected_value = nev_wei_f64.clamp(i128::MIN as f64, i128::MAX as f64) as i128;
        self.is_executable = nev_usd >= min_profit_usd;

        // Logging
        if self.is_executable {
            tracing::info!(
                id = %self.id,
                nev_usd = format!("${:.4}", nev_usd),
                gross_usd = format!("${:.4}", gross_profit_token * token_price),
                gas_usd = format!("${:.4}", gas_cost_token * token_price),
                optimal_gas = format!("{:.1} gwei", self.optimal_gas_price_gwei),
                input_usd = format!("${:.2}", input_usd),
                hops = self.route.len(),
                impact_bps = self.price_impact_bps,
                "✅ EXECUTABLE arbitrage opportunity found"
            );
        } else {
            tracing::info!(
                id = %self.id,
                nev_usd = format!("${:.4}", nev_usd),
                reason = if gross_profit_token <= 0.0 { "no spread" }
                         else if gas_cost_token > gross_profit_token { "gas > spread" }
                         else { "below threshold" },
                "❌ Non-profitable opportunity (math arb found, but rejected due to slippage/gas)"
            );
        }
    }

    /// Gross profit as a percentage of input (pre-cost return).
    pub fn gross_return_bps(&self) -> u32 {
        let gross = self.gross_output.low_u128() as f64;
        let input = self.input_amount.low_u128() as f64;
        if input == 0.0 {
            return 0;
        }
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

    /// Deterministic signature for deduplication in Redis.
    /// Format: "route:{hop1_pool}:{hop1_in}:{hop1_out}|...:{input_amount}"
    pub fn route_dedup_key(&self) -> String {
        let mut keys = Vec::new();
        for step in &self.route {
            keys.push(format!(
                "{}:{}:{}",
                step.pool_id, step.token_in, step.token_out
            ));
        }
        format!("route:{}:{}", keys.join("|"), self.input_amount)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn make_opportunity(
        input: u128,
        output: u128,
        gas_units: u64,
        gas_gwei: f64,
    ) -> ArbitrageOpportunity {
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
            1_000_000_000_000_000_000, // 1 ETH
            1_010_000_000_000_000_000, // 1.01 ETH
            200_000,
            20.0, // 20 gwei
        );
        opp.calculate_nev(3000.0, 95_000.0, 5, 0.50);
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
        opp.calculate_nev(3000.0, 95_000.0, 5, 0.50);
        assert!(
            !opp.is_executable,
            "High gas should make this non-executable"
        );
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
