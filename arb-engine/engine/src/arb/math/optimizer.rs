// ─────────────────────────────────────────────────────────────────────────────
//  arb/math/optimizer.rs — Optimal Flash Loan Size Calculator
//
//  Uses Golden Section Search to find the input size that maximises
//  the net profit of an arbitrage cycle.  The profit function is:
//
//    P(x) = output(x) - x - gas_cost
//
//  where output(x) is the amount received after multi-hop swap of input x.
//  This is a unimodal concave function (price impact grows quadratically),
//  so Golden Section Search converges in O(log(1/ε)) evaluations.
//
//  The algorithm avoids floating-point entirely — all comparisons are done
//  in U256 wei space.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use crate::pool::U256;
use primitive_types::U512;

/// Golden ratio conjugate: φ = (√5 - 1) / 2 ≈ 0.6180339887
/// Represented as 618034 / 1_000_000 for integer arithmetic.
const PHI_NUM: u64 = 618034;
const PHI_DEN: u64 = 1_000_000;

/// Maximum iterations for convergence (sufficient for <1 wei precision).
const MAX_ITERS: u32 = 80;

/// Minimum interval width in wei before we stop (convergence threshold).
/// Below this, the profit difference is negligible.
const MIN_INTERVAL_WEI: u64 = 1_000; // 1000 wei

/// Result of the optimization.
#[derive(Debug, Clone)]
pub struct OptimalInput {
    /// The input amount (in wei) that maximises profit.
    pub amount: U256,
    /// The estimated gross output at the optimal input.
    pub gross_output: U256,
    /// Net profit = gross_output - amount (before gas deduction).
    pub net_profit: U256,
    /// Number of iterations taken.
    pub iterations: u32,
}

/// A function that, given an input amount, returns the gross output.
///
/// Implementations should simulate the full multi-hop swap path:
///   pool1 → pool2 → ... → poolN
/// and return the final token amount received.
pub type SwapSimulator = dyn Fn(U256) -> U256;

/// Find the optimal flash loan input that maximises profit.
///
/// # Arguments
/// * `simulate` — Closure that maps input → gross output (full path simulation).
/// * `lower` — Minimum input to search (e.g. 0.001 ETH = 1e15 wei).
/// * `upper` — Maximum input (e.g. 2% of smallest pool reserve).
/// * `gas_cost_wei` — Fixed gas cost in the same token denomination.
///
/// # Returns
/// * `Some(OptimalInput)` if a profitable input was found.
/// * `None` if no input in [lower, upper] yields positive profit.
pub fn find_optimal_input<F: Fn(U256) -> U256>(
    simulate: &F,
    lower: U256,
    upper: U256,
    gas_cost_wei: U256,
) -> Option<OptimalInput> {
    if lower >= upper {
        return None;
    }

    let mut a = lower;
    let mut b = upper;
    let min_interval = U256::from(MIN_INTERVAL_WEI);

    let mut x1 = golden_subtract(a, b);
    let mut x2 = golden_add(a, b);

    let mut f1 = profit(simulate, x1, gas_cost_wei);
    let mut f2 = profit(simulate, x2, gas_cost_wei);

    let mut iters = 0u32;

    while iters < MAX_ITERS && (b - a) > min_interval {
        iters += 1;

        if f1 > f2 {
            // Maximum is in [a, x2]
            b = x2;
            x2 = x1;
            f2 = f1;
            x1 = golden_subtract(a, b);
            f1 = profit(simulate, x1, gas_cost_wei);
        } else {
            // Maximum is in [x1, b]
            a = x1;
            x1 = x2;
            f1 = f2;
            x2 = golden_add(a, b);
            f2 = profit(simulate, x2, gas_cost_wei);
        }
    }

    // Best point is the midpoint of the final interval (overflow-safe)
    let optimal = a + (b - a) / U256::from(2u64);
    let gross_output = simulate(optimal);

    if gross_output <= optimal {
        return None; // Not profitable after gas
    }
    let gross_profit = gross_output - optimal;
    if gross_profit <= gas_cost_wei {
        return None; // Not profitable after gas
    }
    let net_profit = gross_profit - gas_cost_wei;

    Some(OptimalInput {
        amount: optimal,
        gross_output,
        net_profit,
        iterations: iters,
    })
}

/// Evaluate the net profit at a given input amount.
/// Returns a signed-like value using a (profit, is_positive) tuple encoded in U256.
/// We use saturating subtraction so negative profits → 0.
#[inline]
fn profit<F: Fn(U256) -> U256>(simulate: &F, input: U256, gas_cost_wei: U256) -> U256 {
    let output = simulate(input);
    if output <= input {
        return U256::zero();
    }
    let gross_profit = output - input;
    if gross_profit <= gas_cost_wei {
        U256::zero()
    } else {
        gross_profit - gas_cost_wei
    }
}

/// Compute the left interior point: a + (1 - φ)(b - a)
#[inline]
fn golden_subtract(a: U256, b: U256) -> U256 {
    if b < a {
        return a;
    }
    let diff = b - a;
    let complement = PHI_DEN - PHI_NUM;
    
    let diff_512 = U512::from(diff);
    let complement_512 = U512::from(complement);
    let phi_den_512 = U512::from(PHI_DEN);
    
    let quotient = (diff_512 * complement_512) / phi_den_512;
    
    let mut quotient_bytes = [0u8; 64];
    quotient.to_big_endian(&mut quotient_bytes);
    let quotient_256 = U256::from_big_endian(&quotient_bytes[32..64]);
    
    a + quotient_256
}

/// Compute the right interior point: a + φ(b - a)
#[inline]
fn golden_add(a: U256, b: U256) -> U256 {
    if b < a {
        return a;
    }
    let diff = b - a;
    
    let diff_512 = U512::from(diff);
    let phi_num_512 = U512::from(PHI_NUM);
    let phi_den_512 = U512::from(PHI_DEN);
    
    let quotient = (diff_512 * phi_num_512) / phi_den_512;
    
    let mut quotient_bytes = [0u8; 64];
    quotient.to_big_endian(&mut quotient_bytes);
    let quotient_256 = U256::from_big_endian(&quotient_bytes[32..64]);
    
    a + quotient_256
}

/// Quick helper: estimate the upper bound for a swap path based on
/// the smallest pool's reserve and the max trade size fraction.
pub fn estimate_upper_bound(smallest_reserve: U256, max_trade_pct: f64) -> U256 {
    let pct_bps = (max_trade_pct * 10_000.0) as u64;
    smallest_reserve * U256::from(pct_bps) / U256::from(10_000u64)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_golden_section_concave() {
        // Simulate a simple concave profit function: output = 2*input - input^2/K
        // Peaks at input = K (when derivative = 0)
        let k = U256::from(1_000_000u64); // Peak at 1M
        let sim = move |x: U256| -> U256 {
            let x2_over_k = (x * x) / k;
            if U256::from(2u64) * x > x2_over_k {
                U256::from(2u64) * x - x2_over_k
            } else {
                U256::zero()
            }
        };

        let result = find_optimal_input(
            &sim,
            U256::from(1_000u64),
            U256::from(2_000_000u64),
            U256::zero(), // no gas for test
        );

        assert!(result.is_some(), "Should find optimal");
        let opt = result.unwrap();
        // Profit P(x) = output - x - gas = (2x - x²/K) - x = x - x²/K
        // P'(x) = 1 - 2x/K = 0 → peak at x = K/2 = 500,000
        let expected_peak = k / U256::from(2u64);
        let diff = if opt.amount > expected_peak { opt.amount - expected_peak } else { expected_peak - opt.amount };
        assert!(diff < U256::from(2_000u64), "Should converge near peak K/2, diff = {:?}", diff);
    }

    #[test]
    fn test_no_profit() {
        // Output is always less than input (losing trade)
        let sim = |x: U256| -> U256 {
            x / U256::from(2u64) // 50% loss
        };

        let result = find_optimal_input(
            &sim,
            U256::from(1_000u64),
            U256::from(1_000_000u64),
            U256::zero(),
        );

        assert!(result.is_none(), "Should find no profit");
    }

    #[test]
    fn test_gas_eats_profit() {
        // Barely profitable before gas, but gas makes it unprofitable
        let sim = |x: U256| -> U256 {
            x + U256::from(100u64) // 100 wei profit
        };

        let result = find_optimal_input(
            &sim,
            U256::from(1_000u64),
            U256::from(100_000u64),
            U256::from(1_000u64), // 1000 wei gas > 100 wei profit
        );

        assert!(result.is_none());
    }

    #[test]
    fn test_estimate_upper_bound() {
        let reserve = U256::from(1_000_000_000u64);
        let bound = estimate_upper_bound(reserve, 0.02); // 2%
        assert_eq!(bound, U256::from(20_000_000u64));
    }

    #[test]
    fn test_fuzz_optimizer() {
        let sim = |x: U256| -> U256 {
            if x > U256::from(500_000u64) && x < U256::from(600_000u64) {
                x + U256::from(50_000u64)
            } else {
                U256::zero()
            }
        };

        let _ = find_optimal_input(
            &sim,
            U256::from(1_000u64),
            U256::from(1_000_000u64),
            U256::from(10_000u64),
        );
    }

    #[test]
    fn test_golden_add_less_than_x2() {
        // Try to find if golden_add(x1, b) < x2 is ever true due to integer rounding
        for diff in 1..100_000u64 {
            let a = U256::from(0u64);
            let b = U256::from(diff);
            let x1 = golden_subtract(a, b);
            let x2 = golden_add(a, b);
            let next_x2 = golden_add(x1, b);
            if next_x2 < x2 {
                panic!("Found violation! diff={}, x1={}, x2={}, next_x2={}", diff, x1, x2, next_x2);
            }
        }
    }

    #[test]
    fn test_fuzz_gss_invariants() {
        // We will simulate the GSS loop directly with random/extreme step functions
        // to see if we can trigger b < a or other invariant violations.
        use std::cell::Cell;

        for seed in 0..10_000 {
            let peak = U256::from(seed * 100);
            let sim = |x: U256| -> U256 {
                // A highly chaotic, non-monotonic function
                let val = x.low_u64();
                let hash = val.wrapping_mul(1103515245).wrapping_add(12345);
                U256::from(hash % 100_000)
            };

            let gas_cost = U256::from(seed);
            let lower = U256::from(100);
            let upper = U256::from(1_000_000);

            // Let's trace GSS step by step
            let mut a = lower;
            let mut b = upper;
            let min_interval = U256::from(MIN_INTERVAL_WEI);

            if a >= b { continue; }

            let mut x1 = golden_subtract(a, b);
            let mut x2 = golden_add(a, b);

            let mut f1 = profit(&sim, x1, gas_cost);
            let mut f2 = profit(&sim, x2, gas_cost);

            let mut iters = 0;
            while iters < MAX_ITERS && (b - a) > min_interval {
                iters += 1;

                assert!(b >= a, "Violation: b < a! a={}, b={}", a, b);
                assert!(x1 >= a, "Violation: x1 < a! a={}, x1={}", a, x1);
                assert!(x2 <= b, "Violation: x2 > b! b={}, x2={}", b, x2);
                assert!(x1 <= x2, "Violation: x1 > x2! x1={}, x2={}", x1, x2);

                if f1 > f2 {
                    b = x2;
                    x2 = x1;
                    f2 = f1;
                    x1 = golden_subtract(a, b);
                    f1 = profit(&sim, x1, gas_cost);
                } else {
                    a = x1;
                    x1 = x2;
                    f1 = f2;
                    x2 = golden_add(a, b);
                    f2 = profit(&sim, x2, gas_cost);
                }
            }
        }
    }
}
