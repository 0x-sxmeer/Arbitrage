// ─────────────────────────────────────────────────────────────────────────────
//  pool/v2.rs — Uniswap V2 constant-product AMM math (x·y = k)
//  All arithmetic in U256 to avoid float rounding errors.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use anyhow::{bail, Result};

use crate::pool::{Pool, FEE_DENOMINATOR, U256};

// ── Swap output calculation ────────────────────────────────────────────────────

/// Calculate the exact output amount for a given input on a V2 constant-product pool.
///
/// Uniswap V2 formula (after fee deduction):
///   amount_out = (reserve_out × amount_in × (FEE_DENOM - fee_bps)) /
///                (reserve_in × FEE_DENOM  + amount_in × (FEE_DENOM - fee_bps))
///
/// # Arguments
/// * `pool`        — Pool reference (must be ConstantProduct type)
/// * `amount_in`   — Raw token amount in (no decimal scaling)
/// * `zero_for_one`— true: swap token_a → token_b, false: token_b → token_a
pub fn get_amount_out(pool: &Pool, amount_in: U256, zero_for_one: bool) -> Result<U256> {
    let (reserve_in, reserve_out) = if zero_for_one {
        (pool.state.reserve_a, pool.state.reserve_b)
    } else {
        (pool.state.reserve_b, pool.state.reserve_a)
    };

    if reserve_in.is_zero() || reserve_out.is_zero() {
        bail!("Pool {} has zero reserves — skipping", pool.id);
    }
    if amount_in.is_zero() {
        bail!("amount_in must be greater than zero");
    }

    let fee_num = U256::from(FEE_DENOMINATOR - pool.fee_bps);
    let amount_in_with_fee = amount_in * fee_num;
    let numerator = amount_in_with_fee * reserve_out;
    let denominator = reserve_in * U256::from(FEE_DENOMINATOR) + amount_in_with_fee;

    if denominator.is_zero() {
        bail!("Denominator overflow/zero in V2 swap for pool {}", pool.id);
    }

    Ok(numerator / denominator)
}

/// Calculate the exact input required to receive a target output amount.
///
/// Inverse of `get_amount_out`. Useful for "buy exactly X" execution planning.
///
/// Formula:
///   amount_in = (reserve_in × amount_out × FEE_DENOM) /
///               ((reserve_out - amount_out) × fee_num) + 1   (ceiling)
pub fn get_amount_in(pool: &Pool, amount_out: U256, zero_for_one: bool) -> Result<U256> {
    let (reserve_in, reserve_out) = if zero_for_one {
        (pool.state.reserve_a, pool.state.reserve_b)
    } else {
        (pool.state.reserve_b, pool.state.reserve_a)
    };

    if reserve_in.is_zero() || reserve_out.is_zero() {
        bail!("Pool {} has zero reserves", pool.id);
    }
    if amount_out >= reserve_out {
        bail!(
            "amount_out ({}) exceeds reserve_out ({})",
            amount_out,
            reserve_out
        );
    }

    let fee_num = U256::from(FEE_DENOMINATOR - pool.fee_bps);
    let numerator = reserve_in * amount_out * U256::from(FEE_DENOMINATOR);
    let denominator = (reserve_out - amount_out) * fee_num;

    if denominator.is_zero() {
        bail!("Denominator zero in V2 get_amount_in for pool {}", pool.id);
    }

    // Ceiling division: add 1 to avoid underpaying by 1 wei
    Ok(numerator / denominator + U256::one())
}

// ── Price impact ──────────────────────────────────────────────────────────────

/// Calculate price impact in basis points for a given trade.
///
/// Approximation: impact ≈ amount_in / (reserve_in + amount_in)
/// This is the fraction by which the marginal price shifts.
///
/// Returns 10_000 (100%) if reserves are zero (degenerate pool).
pub fn price_impact_bps(pool: &Pool, amount_in: U256, zero_for_one: bool) -> u32 {
    let reserve_in = if zero_for_one {
        pool.state.reserve_a
    } else {
        pool.state.reserve_b
    };

    if reserve_in.is_zero() {
        return 10_000;
    }

    let impact = amount_in * U256::from(10_000u32) / (reserve_in + amount_in);
    // safe: impact < 10_000 always (< 100%)
    impact.low_u32()
}

// ── Optimal input size ────────────────────────────────────────────────────────

/// Find the trade size that maximises profit for a two-pool arbitrage.
///
/// Given two pools A (buy) and B (sell) with the same token pair, the optimal
/// input amount is:
///
///   x* = √(r_a_in · r_b_out · r_a_out · r_b_in) − r_a_in · r_b_in
///        ──────────────────────────────────────────────────────────
///                    r_a_out + r_b_in
///
/// This is derived by solving dProfit/dx = 0. We approximate using integer
/// square root from primitive-types.
///
/// Returns `None` if no profitable amount exists.
pub fn optimal_input(buy_pool: &Pool, sell_pool: &Pool, zero_for_one_buy: bool) -> Option<U256> {
    let (r_a_in, r_a_out) = if zero_for_one_buy {
        (buy_pool.state.reserve_a, buy_pool.state.reserve_b)
    } else {
        (buy_pool.state.reserve_b, buy_pool.state.reserve_a)
    };

    // On sell pool we go the opposite direction
    let (r_b_in, r_b_out) = if zero_for_one_buy {
        (sell_pool.state.reserve_b, sell_pool.state.reserve_a)
    } else {
        (sell_pool.state.reserve_a, sell_pool.state.reserve_b)
    };

    if r_a_in.is_zero() || r_a_out.is_zero() || r_b_in.is_zero() || r_b_out.is_zero() {
        return None;
    }

    // Use u128 arithmetic to avoid overflow on typical pool sizes
    let ra_in = r_a_in.low_u128();
    let ra_out = r_a_out.low_u128();
    let rb_in = r_b_in.low_u128();
    let rb_out = r_b_out.low_u128();

    // numerator of optimal formula: √(ra_in · rb_out · ra_out · rb_in)
    // Use u128 integer sqrt approximation
    let product = (ra_in as u128)
        .checked_mul(rb_out)?
        .checked_mul(ra_out)?
        .checked_mul(rb_in)?;

    let sqrt_product = integer_sqrt(product);
    let denominator = ra_out.checked_add(rb_in)?;

    if denominator == 0 {
        return None;
    }

    let cross = ra_in.checked_mul(rb_in).unwrap_or(u128::MAX) / denominator;
    if sqrt_product <= cross {
        return None; // no profitable amount
    }

    let numerator = sqrt_product.saturating_sub(cross);
    let x_star = numerator.checked_div(denominator)?;

    if x_star == 0 {
        return None;
    }

    Some(U256::from(x_star))
}

/// Integer square root using Newton's method (u128).
fn integer_sqrt(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{ChainId, DexProtocol, Pool, PoolState, PoolType, Token};

    fn make_pool(reserve_a: u128, reserve_b: u128, fee_bps: u32) -> Pool {
        Pool {
            id: "test_pool".into(),
            chain: ChainId::Ethereum,
            dex: DexProtocol::UniswapV2,
            token_a: Token {
                address: "0xWETH".into(),
                symbol: "WETH".into(),
                decimals: 18,
            },
            token_b: Token {
                address: "0xUSDC".into(),
                symbol: "USDC".into(),
                decimals: 6,
            },
            pool_type: PoolType::ConstantProduct,
            fee_bps,
            state: PoolState {
                reserve_a: U256::from(reserve_a),
                reserve_b: U256::from(reserve_b),
                sqrt_price_x96: None,
                tick: None,
                liquidity: None,
                amp_coeff: None,
            },
            last_updated_block: 1,
            last_updated_ts: 0,
        }
    }

    #[test]
    fn test_amount_out_basic() {
        // Pool: 100 token_a / 300 token_b (both 18 decimals), 0.3% fee (30 bps)
        // Spot price: 1 A = 3 B. After fee + impact, expect slightly less.
        let pool = make_pool(
            100_000_000_000_000_000_000, // 100 token_a (18 dec)
            300_000_000_000_000_000_000, // 300 token_b (18 dec)
            30,
        );
        let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 token_a
        let out = get_amount_out(&pool, amount_in, true).unwrap();
        // Expect ~2.96 token_b (slightly less than 3.0 due to 0.3% fee + ~1% impact)
        let out_decimal = out.low_u128() as f64 / 1e18;
        println!("1 TKA → {} TKB (0.3% fee pool)", out_decimal);
        assert!(
            out_decimal > 2.9 && out_decimal < 3.0,
            "out = {}",
            out_decimal
        );
    }

    #[test]
    fn test_amount_in_inverse() {
        // Verify get_amount_in is approximately inverse of get_amount_out
        // Using same-decimal tokens for clean arithmetic
        let pool = make_pool(
            100_000_000_000_000_000_000, // 100 token_a (18 dec)
            300_000_000_000_000_000_000, // 300 token_b (18 dec)
            30,
        );
        let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 token_a
        let out = get_amount_out(&pool, amount_in, true).unwrap();
        let required_in = get_amount_in(&pool, out, true).unwrap();
        // Should be close to original (ceiling adds 1 wei)
        let diff = if required_in > amount_in {
            required_in - amount_in
        } else {
            amount_in - required_in
        };
        // Allow 1 wei tolerance from ceiling division
        assert!(diff <= U256::from(2u32), "diff = {}", diff);
    }

    #[test]
    fn test_price_impact() {
        // Large trade: 10 token_a into a 100 token_a pool = ~9% impact
        let pool = make_pool(100_000_000_000_000_000_000, 300_000_000_000_000_000_000, 30);
        let amount_in = U256::from(10_000_000_000_000_000_000u128);
        let impact = price_impact_bps(&pool, amount_in, true);
        println!("Price impact: {} bps", impact);
        assert!(impact > 800 && impact < 1000, "impact = {}", impact);
    }

    #[test]
    fn test_zero_reserves_errors() {
        let pool = make_pool(0, 300_000_000_000_000_000_000, 30);
        let result = get_amount_out(&pool, U256::from(1u32), true);
        assert!(result.is_err());
    }

    #[test]
    fn test_zero_amount_in_errors() {
        let pool = make_pool(100_000_000_000_000_000_000, 300_000_000_000_000_000_000, 30);
        let result = get_amount_out(&pool, U256::zero(), true);
        assert!(result.is_err());
    }

    #[test]
    fn test_integer_sqrt() {
        assert_eq!(integer_sqrt(0), 0);
        assert_eq!(integer_sqrt(1), 1);
        assert_eq!(integer_sqrt(4), 2);
        assert_eq!(integer_sqrt(9), 3);
        assert_eq!(integer_sqrt(16), 4);
        assert_eq!(integer_sqrt(100), 10);
        assert_eq!(integer_sqrt(1_000_000), 1000);
    }
}
