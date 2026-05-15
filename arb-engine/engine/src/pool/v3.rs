// ─────────────────────────────────────────────────────────────────────────────
//  pool/v3.rs — Uniswap V3 concentrated liquidity math
//
//  Key concepts:
//  - Prices are stored as sqrt(price) in Q64.96 fixed-point format
//  - Liquidity is only active within the current tick range [tickLower, tickUpper]
//  - Swaps within a single tick range simplify to a closed-form solution
//
//  Fee handling:
//  - Pool.fee_bps is stored in canonical basis points (30 = 0.3%)
//  - This file converts bps → fractional fee for swap simulation
//
//  References:
//  - Uniswap V3 whitepaper: https://uniswap.org/whitepaper-v3.pdf
//  - tick_math.rs port from: https://github.com/shuhuiluo/uniswap-v3-sdk-rs
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use anyhow::{bail, Result};

use crate::pool::{Pool, FEE_DENOMINATOR, U256};

// ── Constants ──────────────────────────────────────────────────────────────────

/// Q96 = 2^96 — the denominator for sqrt price fixed-point
const Q96: u128 = 1u128 << 96;

/// Minimum tick (corresponds to minimum sqrt price)
pub const MIN_TICK: i32 = -887272;
/// Maximum tick (corresponds to maximum sqrt price)
pub const MAX_TICK: i32 = 887272;

/// Minimum sqrt ratio (Q64.96): corresponds to tick MIN_TICK
/// = sqrt(1.0001^MIN_TICK) × 2^96
pub const MIN_SQRT_RATIO: u128 = 4295128739;
/// Maximum sqrt ratio (Q64.96): corresponds to tick MAX_TICK

// ── Price conversion ──────────────────────────────────────────────────────────

/// Convert a Uniswap V3 sqrtPriceX96 (Q64.96 fixed-point) to a human-readable
/// price of token0 denominated in token1, adjusted for decimals.
///
/// Formula: price = (sqrtPriceX96 / 2^96)² × 10^(decimals0 - decimals1)
pub fn sqrt_price_x96_to_price(sqrt_price_x96: U256, decimals0: u8, decimals1: u8) -> f64 {
    // (sqrtPrice / 2^96)^2
    let sq = sqrt_price_x96.low_u128() as f64 / Q96 as f64;
    let raw_price = sq * sq;

    // Adjust for token decimal difference
    let decimal_adj = 10f64.powi(decimals0 as i32 - decimals1 as i32);
    raw_price * decimal_adj
}

/// Convert a tick index to its corresponding sqrt price ratio (Q64.96).
///
/// Formula: sqrtPriceX96 = sqrt(1.0001^tick) × 2^96
///
/// We use the bit-shift trick from Uniswap's TickMath.getSqrtRatioAtTick.
pub fn tick_to_sqrt_price_x96(tick: i32) -> Result<U256> {
    if tick < MIN_TICK || tick > MAX_TICK {
        bail!("Tick {} out of range [{}, {}]", tick, MIN_TICK, MAX_TICK);
    }

    // Use floating point for now — production would use fixed-point integer
    // replication of Uniswap's getSqrtRatioAtTick for exact bit-for-bit match
    let price = 1.0001f64.powi(tick);
    let sqrt_price = price.sqrt();
    let sqrt_price_x96 = sqrt_price * Q96 as f64;

    Ok(U256::from(sqrt_price_x96 as u128))
}

/// Convert a sqrtPriceX96 value to the nearest tick.
///
/// Formula: tick = floor(log(price) / log(1.0001))
///        where price = (sqrtPriceX96 / 2^96)²
pub fn sqrt_price_x96_to_tick(sqrt_price_x96: U256) -> i32 {
    let sq = sqrt_price_x96.low_u128() as f64 / Q96 as f64;
    let price = sq * sq;
    if price <= 0.0 {
        return MIN_TICK;
    }
    let tick = (price.ln() / 1.0001f64.ln()).floor() as i32;
    tick.clamp(MIN_TICK, MAX_TICK)
}

// ── Swap simulation ───────────────────────────────────────────────────────────

/// Estimate the output of a V3 swap within the CURRENT active tick range only.
///
/// This is a single-tick approximation. A production implementation would
/// iterate across multiple tick ranges if the trade crosses tick boundaries.
/// For NEV calculation purposes, this approximation is sufficient when
/// `amount_in` is small relative to the total liquidity.
///
/// Fee handling: `pool.fee_bps` is canonical basis points (30 = 0.3%).
///   fee_factor = 1.0 - fee_bps / FEE_DENOMINATOR
///
/// # Arguments
/// * `pool`        — Pool with ConcentratedLiquidity type
/// * `amount_in`   — Raw token amount (no decimal scaling)  
/// * `zero_for_one`— Direction: token_a → token_b (true) or token_b → token_a (false)
///
/// Returns estimated output amount.
pub fn get_amount_out_v3(pool: &Pool, amount_in: U256, zero_for_one: bool) -> Result<U256> {
    let sqrt_price_x96 = pool.state.sqrt_price_x96
        .ok_or_else(|| anyhow::anyhow!("V3 pool {} missing sqrtPriceX96", pool.id))?;
    let liquidity = pool.state.liquidity
        .ok_or_else(|| anyhow::anyhow!("V3 pool {} missing liquidity", pool.id))?;

    if liquidity == 0 {
        bail!("V3 pool {} has zero liquidity", pool.id);
    }
    if amount_in.is_zero() {
        bail!("amount_in must be greater than zero");
    }

    // Work in f64 for the simulation — acceptable for NEV estimation
    // Production execution would use exact integer math from the Uniswap SDK
    let sqrt_p = sqrt_price_x96.low_u128() as f64 / Q96 as f64;
    let l = liquidity as f64;
    let dx = amount_in.low_u128() as f64;

    // Fee adjustment: pool.fee_bps is canonical bps (30 = 0.3%)
    let fee_factor = 1.0 - (pool.fee_bps as f64 / FEE_DENOMINATOR as f64);
    let dx_after_fee = dx * fee_factor;

    let amount_out = if zero_for_one {
        // Buying token_b with token_a: price decreases (sqrt_p decreases)
        // Δy = L × (√P_current - √P_new)
        // √P_new = L × √P_current / (L + dx_after_fee × √P_current)
        let denominator = l + dx_after_fee * sqrt_p;
        if denominator == 0.0 {
            bail!("V3 swap denominator is zero for pool {}", pool.id);
        }
        let sqrt_p_new = l * sqrt_p / denominator;
        let delta_y = l * (sqrt_p - sqrt_p_new);
        delta_y.max(0.0) as u128
    } else {
        // Buying token_a with token_b: price increases (sqrt_p increases)
        // Δx = L × (1/√P_current - 1/√P_new)
        // √P_new = √P_current + dy_after_fee / L
        if sqrt_p == 0.0 || l == 0.0 {
            bail!("V3 pool {} has zero sqrt price or liquidity", pool.id);
        }
        let sqrt_p_new = sqrt_p + dx_after_fee / l;
        let delta_x = l * (1.0 / sqrt_p - 1.0 / sqrt_p_new);
        delta_x.max(0.0) as u128
    };

    Ok(U256::from(amount_out))
}

/// Calculate V3 price impact in basis points.
///
/// For V3, impact is roughly proportional to the trade size relative to virtual
/// reserves at the current tick: virtual_x = L / sqrt_P, virtual_y = L × sqrt_P.
pub fn price_impact_bps_v3(pool: &Pool, amount_in: U256, zero_for_one: bool) -> u32 {
    let sqrt_price_x96 = match pool.state.sqrt_price_x96 {
        Some(p) => p,
        None    => return 10_000,
    };
    let liquidity = match pool.state.liquidity {
        Some(l) if l > 0 => l,
        _ => return 10_000,
    };

    let sqrt_p = sqrt_price_x96.low_u128() as f64 / Q96 as f64;
    let l = liquidity as f64;
    let dx = amount_in.low_u128() as f64;

    // Virtual reserve at current tick
    let virtual_reserve = if zero_for_one {
        l / sqrt_p // virtual token_a reserve
    } else {
        l * sqrt_p // virtual token_b reserve
    };

    if virtual_reserve == 0.0 {
        return 10_000;
    }

    let impact = dx / (virtual_reserve + dx);
    (impact * 10_000.0).min(10_000.0) as u32
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_conversion_roundtrip() {
        // Tick 0 → price = 1.0 (token0 = token1 when both 18 decimals)
        let sqrt_px96 = tick_to_sqrt_price_x96(0).unwrap();
        let price = sqrt_price_x96_to_price(sqrt_px96, 18, 18);
        println!("Tick 0 price: {}", price);
        assert!((price - 1.0).abs() < 0.001, "Expected ~1.0, got {}", price);
    }

    #[test]
    fn test_tick_to_price_positive() {
        // Tick 60000 ≈ price 403 (roughly 1 ETH = 403 token1)
        let sqrt_px96 = tick_to_sqrt_price_x96(60000).unwrap();
        let price = sqrt_price_x96_to_price(sqrt_px96, 18, 18);
        println!("Tick 60000 price: {}", price);
        assert!(price > 300.0 && price < 500.0, "price = {}", price);
    }

    #[test]
    fn test_tick_out_of_range() {
        assert!(tick_to_sqrt_price_x96(MIN_TICK - 1).is_err());
        assert!(tick_to_sqrt_price_x96(MAX_TICK + 1).is_err());
    }

    #[test]
    fn test_sqrt_to_tick_roundtrip() {
        let tick = 12345;
        let sqrt_px96 = tick_to_sqrt_price_x96(tick).unwrap();
        let back_tick = sqrt_price_x96_to_tick(sqrt_px96);
        // Allow ±1 tick rounding error from float precision
        assert!((back_tick - tick).abs() <= 1, "tick={} back_tick={}", tick, back_tick);
    }

    #[test]
    fn test_fee_denominator_consistency() {
        // 30 bps should give fee_factor = 0.997
        let fee_factor = 1.0 - (30.0 / FEE_DENOMINATOR as f64);
        assert!((fee_factor - 0.997).abs() < 0.0001, "fee_factor = {}", fee_factor);
    }
}
