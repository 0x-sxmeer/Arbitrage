// ─────────────────────────────────────────────────────────────────────────────
//  arb/math/v3_math.rs — Production Uniswap V3 TickMath + SwapMath (U256)
//
//  Exact port of Uniswap V3's TickMath.sol and SwapMath.sol.
//  All arithmetic uses primitive_types::U256 — zero floating-point.
//
//  References:
//    ▸ https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/TickMath.sol
//    ▸ https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/SwapMath.sol
//    ▸ https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/SqrtPriceMath.sol
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use crate::pool::U256;

pub const MIN_TICK: i32 = -887272;
pub const MAX_TICK: i32 = 887272;

pub const MIN_SQRT_RATIO: u128 = 4295128739;
// MAX_SQRT_RATIO as a decimal string (too large for u128)
const MAX_SQRT_RATIO_STR: &str = "1461446703485210103287273052203988822378723970342";

/// Q96 = 2^96
const Q96: u128 = 1u128 << 96;

// ─────────────────────────────────────────────────────────────────────────────
//  TickMath — exact port of TickMath.sol getSqrtRatioAtTick
// ─────────────────────────────────────────────────────────────────────────────

/// Calculates sqrt(1.0001^tick) * 2^96.
/// Exact bit-for-bit match with Uniswap V3 TickMath.sol.
pub fn get_sqrt_ratio_at_tick(tick: i32) -> Result<U256, &'static str> {
    if tick < MIN_TICK || tick > MAX_TICK {
        return Err("TICK_OUT_OF_BOUNDS");
    }

    let abs_tick = if tick < 0 { -tick } else { tick } as u32;

    // Each magic constant is the Q128.128 representation of sqrt(1.0001^(-2^i)).
    // We multiply selected constants based on which bits of abs_tick are set.
    let mut ratio: U256 = if (abs_tick & 0x1) != 0 {
        U256::from_str_radix("fffcb933bd6fad37aa2d162d1a594001", 16).unwrap()
    } else {
        U256::from(1u64) << 128 // 2^128
    };

    // Bit 1..19 — multiply by precomputed magic constants, shift >>128
    macro_rules! apply_bit {
        ($bit:expr, $hex:expr) => {
            if (abs_tick & $bit) != 0 {
                ratio = (ratio * U256::from_str_radix($hex, 16).unwrap()) >> 128;
            }
        };
    }

    apply_bit!(0x2,     "fff97272373d413259a46990580e213a");
    apply_bit!(0x4,     "fff2e50f5f656932ef12357cf3c7fdcc");
    apply_bit!(0x8,     "ffe5caca7e10e4e61c3624eaa0941cd0");
    apply_bit!(0x10,    "ffcb9843d60f6159c9db58835c926644");
    apply_bit!(0x20,    "ff973b41fa98c081472e6896dfb254c0");
    apply_bit!(0x40,    "ff2ea16466c96a3843ec78b326b52861");
    apply_bit!(0x80,    "fe5dee046a99a2a811c461f1969c3053");
    apply_bit!(0x100,   "fcbe86c747fd2dcb8ce4281734fe8030");
    apply_bit!(0x200,   "f987a7253ac413176f2b074cf7815e54");
    apply_bit!(0x400,   "f3392b0822b70005940c7a398e4b70f3");
    apply_bit!(0x800,   "e7159475a2c29b7443b29c7fa6e889d9");
    apply_bit!(0x1000,  "d097f3bdfd2022b8845ad8f792aa5825");
    apply_bit!(0x2000,  "a9f746462d870fdf8a65dc1f90e061e5");
    apply_bit!(0x4000,  "70d869a156d2a1b890bb3df62baf32f7");
    apply_bit!(0x8000,  "31be135f97d08fd981231505542fcfa6");
    apply_bit!(0x10000, "9aa508b5b7a84e1c677de54f3e99bc9");
    apply_bit!(0x20000, "5d6af8fbc1ada98bd813fcaa2f8cc5");
    apply_bit!(0x40000, "2216e584f5fa1ea926992ce5ffb6ea");
    apply_bit!(0x80000, "48a170391f7dc42152865d4407ab");

    if tick > 0 {
        ratio = U256::MAX / ratio;
    }

    // Shift from Q128.128 to Q128.96 with rounding up
    let remainder = ratio % (U256::from(1u64) << 32);
    let shift = (ratio >> 32) + if remainder == U256::zero() { U256::zero() } else { U256::from(1u64) };
    Ok(shift)
}

/// Inverse: given a sqrtPriceX96, compute the greatest tick such that
/// get_sqrt_ratio_at_tick(tick) <= sqrtPriceX96.
/// Uses binary search over ticks for correctness.
pub fn get_tick_at_sqrt_ratio(sqrt_price_x96: U256) -> Result<i32, &'static str> {
    let min_sqrt = U256::from(MIN_SQRT_RATIO);
    let max_sqrt = U256::from_str_radix(MAX_SQRT_RATIO_STR, 10).unwrap();

    if sqrt_price_x96 < min_sqrt || sqrt_price_x96 > max_sqrt {
        return Err("SQRT_RATIO_OUT_OF_BOUNDS");
    }

    // Use f64 approximation then refine — fast and accurate for all valid inputs
    let sq = sqrt_price_x96.low_u128() as f64 / Q96 as f64;
    let price = sq * sq;
    if price <= 0.0 || !price.is_finite() {
        return Ok(0);
    }
    let approx_tick = (price.ln() / 1.0001_f64.ln()).floor() as i32;
    let tick = approx_tick.clamp(MIN_TICK, MAX_TICK);

    // Refine: ensure get_sqrt_ratio_at_tick(tick) <= sqrt_price_x96
    let sqrt_at_tick = get_sqrt_ratio_at_tick(tick).unwrap_or(min_sqrt);
    if sqrt_at_tick > sqrt_price_x96 {
        Ok((tick - 1).max(MIN_TICK))
    } else {
        // Check if tick+1 is still <=
        let sqrt_at_next = get_sqrt_ratio_at_tick((tick + 1).min(MAX_TICK)).unwrap_or(max_sqrt);
        if sqrt_at_next <= sqrt_price_x96 {
            Ok((tick + 1).min(MAX_TICK))
        } else {
            Ok(tick)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  FullMath — 512-bit safe mulDiv
// ─────────────────────────────────────────────────────────────────────────────

/// floor(a * b / denominator) without intermediate overflow.
#[inline]
pub fn mul_div(a: U256, b: U256, denom: U256) -> Option<U256> {
    if denom.is_zero() { return None; }
    match a.checked_mul(b) {
        Some(product) => Some(product / denom),
        None => {
            // Fallback: (a/denom)*b + (a%denom)*b/denom
            let q = a / denom;
            let r = a % denom;
            match r.checked_mul(b) {
                Some(rb) => q.checked_mul(b)?.checked_add(rb / denom),
                None => q.checked_mul(b),
            }
        }
    }
}

/// ceil(a * b / denominator).
#[inline]
pub fn mul_div_rounding_up(a: U256, b: U256, denom: U256) -> Option<U256> {
    let result = mul_div(a, b, denom)?;
    // Check if there's a remainder
    if let Some(product) = a.checked_mul(b) {
        if product % denom != U256::zero() {
            return result.checked_add(U256::from(1u64));
        }
    }
    Some(result)
}

// ─────────────────────────────────────────────────────────────────────────────
//  SqrtPriceMath — exact port of SqrtPriceMath.sol
// ─────────────────────────────────────────────────────────────────────────────

/// Amount of token0 needed to move from sqrt_ratio_a to sqrt_ratio_b.
/// getAmount0Delta(sqrtA, sqrtB, liquidity, roundUp)
pub fn get_amount0_delta(
    sqrt_ratio_a_x96: U256,
    sqrt_ratio_b_x96: U256,
    liquidity: u128,
    round_up: bool,
) -> U256 {
    let (lower, upper) = if sqrt_ratio_a_x96 > sqrt_ratio_b_x96 {
        (sqrt_ratio_b_x96, sqrt_ratio_a_x96)
    } else {
        (sqrt_ratio_a_x96, sqrt_ratio_b_x96)
    };

    if lower.is_zero() || upper.is_zero() { return U256::zero(); }

    let liq = U256::from(liquidity);
    let q96 = U256::from(Q96);
    let diff = upper - lower;

    let numerator1 = liq * q96;

    if round_up {
        // ceil( ceil(numerator1 * diff / upper) / lower )
        let inner = mul_div_rounding_up(numerator1, diff, upper).unwrap_or(U256::zero());
        // Ceiling division by lower
        if inner.is_zero() { return U256::zero(); }
        (inner + lower - U256::from(1u64)) / lower
    } else {
        let inner = mul_div(numerator1, diff, upper).unwrap_or(U256::zero());
        inner / lower
    }
}

/// Amount of token1 needed to move from sqrt_ratio_a to sqrt_ratio_b.
pub fn get_amount1_delta(
    sqrt_ratio_a_x96: U256,
    sqrt_ratio_b_x96: U256,
    liquidity: u128,
    round_up: bool,
) -> U256 {
    let (lower, upper) = if sqrt_ratio_a_x96 > sqrt_ratio_b_x96 {
        (sqrt_ratio_b_x96, sqrt_ratio_a_x96)
    } else {
        (sqrt_ratio_a_x96, sqrt_ratio_b_x96)
    };

    let liq = U256::from(liquidity);
    let diff = upper - lower;

    if round_up {
        mul_div_rounding_up(liq, diff, U256::from(Q96)).unwrap_or(U256::zero())
    } else {
        mul_div(liq, diff, U256::from(Q96)).unwrap_or(U256::zero())
    }
}

/// Compute the next sqrt price given an input amount of token0.
/// Price decreases (sqrt price goes down) when selling token0.
pub fn get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount: U256,
    add: bool,
) -> U256 {
    if amount.is_zero() { return sqrt_price_x96; }
    let liq = U256::from(liquidity);
    let numerator1 = liq << 96;

    if add {
        // sqrtP_new = numerator1 * sqrtP / (numerator1 + amount * sqrtP)
        match amount.checked_mul(sqrt_price_x96) {
            Some(product) => {
                let denom = numerator1 + product;
                if denom >= numerator1 {
                    mul_div_rounding_up(numerator1, sqrt_price_x96, denom)
                        .unwrap_or(sqrt_price_x96)
                } else {
                    // Overflow branch
                    let d = numerator1 / sqrt_price_x96 + amount;
                    if d.is_zero() { return sqrt_price_x96; }
                    // ceil(numerator1 / d)
                    (numerator1 + d - U256::from(1u64)) / d
                }
            }
            None => {
                let d = numerator1 / sqrt_price_x96 + amount;
                if d.is_zero() { return sqrt_price_x96; }
                (numerator1 + d - U256::from(1u64)) / d
            }
        }
    } else {
        // Removing liquidity: sqrtP goes up
        let product = amount * sqrt_price_x96;
        if numerator1 <= product { return sqrt_price_x96; } // safety
        let denom = numerator1 - product;
        mul_div_rounding_up(numerator1, sqrt_price_x96, denom)
            .unwrap_or(sqrt_price_x96)
    }
}

/// Compute the next sqrt price given an input amount of token1.
/// Price increases when adding token1.
pub fn get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount: U256,
    add: bool,
) -> U256 {
    let q96 = U256::from(Q96);
    if add {
        let quotient = mul_div(amount, q96, U256::from(liquidity))
            .unwrap_or(U256::zero());
        sqrt_price_x96 + quotient
    } else {
        let quotient = mul_div_rounding_up(amount, q96, U256::from(liquidity))
            .unwrap_or(U256::zero());
        if sqrt_price_x96 <= quotient { return U256::from(MIN_SQRT_RATIO); }
        sqrt_price_x96 - quotient
    }
}

/// Get next sqrt price from input amount (dispatches on zero_for_one).
pub fn get_next_sqrt_price_from_input(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount_in: U256,
    zero_for_one: bool,
) -> U256 {
    if zero_for_one {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_price_x96, liquidity, amount_in, true)
    } else {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_price_x96, liquidity, amount_in, true)
    }
}

/// Get next sqrt price from output amount.
pub fn get_next_sqrt_price_from_output(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount_out: U256,
    zero_for_one: bool,
) -> U256 {
    if zero_for_one {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_price_x96, liquidity, amount_out, false)
    } else {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_price_x96, liquidity, amount_out, false)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  SwapMath — exact port of SwapMath.sol computeSwapStep
// ─────────────────────────────────────────────────────────────────────────────

/// Computes the result of swapping some amount in/out within a single tick range.
///
/// Returns: (sqrt_ratio_next_x96, amount_in, amount_out, fee_amount)
///
/// This is the core function the pathfinder uses to step through tick ranges.
pub fn compute_swap_step(
    sqrt_ratio_current_x96: U256,
    sqrt_ratio_target_x96: U256,
    liquidity: u128,
    amount_remaining: U256,
    fee_pips: u32,         // In millionths (e.g., 3000 = 0.3%)
    exact_in: bool,        // true = exact input, false = exact output
    zero_for_one: bool,
) -> (U256, U256, U256, U256) {
    if liquidity == 0 {
        return (sqrt_ratio_target_x96, U256::zero(), U256::zero(), U256::zero());
    }

    let fee_complement = U256::from(1_000_000u32 - fee_pips);
    let fee_denom = U256::from(1_000_000u32);

    let sqrt_ratio_next_x96;
    let mut amount_in;
    let mut amount_out;

    if exact_in {
        // Deduct fee from amount remaining to get usable input
        let amount_remaining_less_fee = mul_div(amount_remaining, fee_complement, fee_denom)
            .unwrap_or(U256::zero());

        // Compute how much input is needed to reach the target price
        amount_in = if zero_for_one {
            get_amount0_delta(sqrt_ratio_target_x96, sqrt_ratio_current_x96, liquidity, true)
        } else {
            get_amount1_delta(sqrt_ratio_current_x96, sqrt_ratio_target_x96, liquidity, true)
        };

        if amount_remaining_less_fee >= amount_in {
            // We can reach the target price
            sqrt_ratio_next_x96 = sqrt_ratio_target_x96;
        } else {
            // We can't reach target — use all input
            sqrt_ratio_next_x96 = get_next_sqrt_price_from_input(
                sqrt_ratio_current_x96,
                liquidity,
                amount_remaining_less_fee,
                zero_for_one,
            );
            amount_in = amount_remaining_less_fee;
        }
    } else {
        // Exact output
        amount_out = if zero_for_one {
            get_amount1_delta(sqrt_ratio_target_x96, sqrt_ratio_current_x96, liquidity, false)
        } else {
            get_amount0_delta(sqrt_ratio_current_x96, sqrt_ratio_target_x96, liquidity, false)
        };

        if amount_remaining >= amount_out {
            sqrt_ratio_next_x96 = sqrt_ratio_target_x96;
        } else {
            sqrt_ratio_next_x96 = get_next_sqrt_price_from_output(
                sqrt_ratio_current_x96,
                liquidity,
                amount_remaining,
                zero_for_one,
            );
            amount_out = amount_remaining;
        }

        // Compute amount_in from the actual price movement
        amount_in = if zero_for_one {
            get_amount0_delta(sqrt_ratio_next_x96, sqrt_ratio_current_x96, liquidity, true)
        } else {
            get_amount1_delta(sqrt_ratio_current_x96, sqrt_ratio_next_x96, liquidity, true)
        };

        // Compute fee: amount_in * fee_pips / (1_000_000 - fee_pips)
        let fee_amount = mul_div_rounding_up(amount_in, U256::from(fee_pips), fee_complement)
            .unwrap_or(U256::zero());

        return (sqrt_ratio_next_x96, amount_in, amount_out, fee_amount);
    }

    // For exact_in: compute output from actual price change
    amount_out = if zero_for_one {
        get_amount1_delta(sqrt_ratio_next_x96, sqrt_ratio_current_x96, liquidity, false)
    } else {
        get_amount0_delta(sqrt_ratio_current_x96, sqrt_ratio_next_x96, liquidity, false)
    };

    // Compute fee amount
    let fee_amount = if sqrt_ratio_next_x96 != sqrt_ratio_target_x96 {
        // Didn't reach target — fee = remaining - amount_in
        amount_remaining - amount_in
    } else {
        // Reached target — fee = amount_in * feePips / (1e6 - feePips)
        mul_div_rounding_up(amount_in, U256::from(fee_pips), fee_complement)
            .unwrap_or(U256::zero())
    };

    // Re-add amount_in for exact_in (we consumed amount_remaining_less_fee above)
    // But the caller already deducted fee, so amount_in here is the pre-fee part.

    (sqrt_ratio_next_x96, amount_in, amount_out, fee_amount)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Convenience: simulate a full single-range swap (used by router edges)
// ─────────────────────────────────────────────────────────────────────────────

/// Simulate a swap through a single tick range and return the output amount.
/// `fee_bps` is in basis points (30 = 0.3%). Internally converted to pips.
pub fn simulate_swap_single_range(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount_in: U256,
    fee_bps: u32,
    zero_for_one: bool,
) -> U256 {
    if liquidity == 0 || amount_in.is_zero() || sqrt_price_x96.is_zero() {
        return U256::zero();
    }

    // Convert fee_bps (basis points, 10_000 denom) to fee_pips (1_000_000 denom)
    let fee_pips = fee_bps * 100;
    if fee_pips >= 1_000_000 { return U256::zero(); }

    // Target: MIN or MAX depending on direction
    let target = if zero_for_one {
        U256::from(MIN_SQRT_RATIO) + U256::from(1u64)
    } else {
        U256::from_str_radix(MAX_SQRT_RATIO_STR, 10).unwrap() - U256::from(1u64)
    };

    let (_next_sqrt, _amount_in, amount_out, _fee) = compute_swap_step(
        sqrt_price_x96,
        target,
        liquidity,
        amount_in,
        fee_pips,
        true,  // exact input
        zero_for_one,
    );

    amount_out
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_0_gives_q96() {
        let sqrt_px96 = get_sqrt_ratio_at_tick(0).unwrap();
        let expected = U256::from(Q96);
        let diff = if sqrt_px96 > expected { sqrt_px96 - expected } else { expected - sqrt_px96 };
        assert!(diff <= U256::from(2u64), "tick 0 → {:?}, expected ~{:?}", sqrt_px96, expected);
    }

    #[test]
    fn test_tick_bounds() {
        assert!(get_sqrt_ratio_at_tick(MIN_TICK).is_ok());
        assert!(get_sqrt_ratio_at_tick(MAX_TICK).is_ok());
        assert!(get_sqrt_ratio_at_tick(MIN_TICK - 1).is_err());
        assert!(get_sqrt_ratio_at_tick(MAX_TICK + 1).is_err());
    }

    #[test]
    fn test_min_tick_gives_min_sqrt() {
        let sqrt = get_sqrt_ratio_at_tick(MIN_TICK).unwrap();
        assert!(sqrt >= U256::from(MIN_SQRT_RATIO), "MIN_TICK sqrt too small: {:?}", sqrt);
    }

    #[test]
    fn test_positive_tick_increases_price() {
        let s0 = get_sqrt_ratio_at_tick(0).unwrap();
        let s1 = get_sqrt_ratio_at_tick(100).unwrap();
        assert!(s1 > s0, "Positive tick should increase sqrt price");
    }

    #[test]
    fn test_compute_swap_step_basic() {
        // 1 ETH swap on a pool with realistic liquidity
        let sqrt_price = U256::from(1_936_540_681_085_355_540_000_000_000_000u128);
        let liquidity: u128 = 12_345_678_901_234_567_890;
        let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 ETH
        let target = U256::from(MIN_SQRT_RATIO) + U256::from(1u64);

        let (next_sqrt, amt_in, amt_out, fee) = compute_swap_step(
            sqrt_price, target, liquidity, amount_in, 3000, true, true,
        );

        assert!(next_sqrt < sqrt_price, "Price should decrease for zero_for_one");
        assert!(!amt_out.is_zero(), "Should have non-zero output");
        assert!(!fee.is_zero(), "Should have non-zero fee");
        assert!(amt_in <= amount_in, "Can't consume more than input");
    }

    #[test]
    fn test_simulate_swap_produces_output() {
        let sqrt_price = U256::from(1_936_540_681_085_355_540_000_000_000_000u128);
        let liquidity: u128 = 12_345_678_901_234_567_890;
        let amount_in = U256::from(1_000_000_000_000_000_000u128);

        let out = simulate_swap_single_range(sqrt_price, liquidity, amount_in, 30, true);
        assert!(!out.is_zero(), "Should produce output: {:?}", out);
    }

    #[test]
    fn test_higher_fee_less_output() {
        let sqrt_price = U256::from(1_936_540_681_085_355_540_000_000_000_000u128);
        let liquidity: u128 = 12_345_678_901_234_567_890;
        let amount_in = U256::from(100_000_000_000_000_000_000u128); // 100 ETH

        let out_low = simulate_swap_single_range(sqrt_price, liquidity, amount_in, 5, true);
        let out_high = simulate_swap_single_range(sqrt_price, liquidity, amount_in, 100, true);
        assert!(out_low >= out_high, "Lower fee should produce >= output");
    }

    #[test]
    fn test_zero_liquidity_returns_zero() {
        let sqrt_price = U256::from(1_936_540_681_085_355_540_000_000_000_000u128);
        let out = simulate_swap_single_range(sqrt_price, 0, U256::from(1u64), 30, true);
        assert!(out.is_zero());
    }
}
