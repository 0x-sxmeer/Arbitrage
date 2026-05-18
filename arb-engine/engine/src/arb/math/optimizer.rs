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

    // Interior probe points using golden ratio
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

    // Best point is the midpoint of the final interval
    let optimal = (a + b) / U256::from(2u64);
    let gross_output = simulate(optimal);

    if gross_output <= optimal + gas_cost_wei {
        return None; // Not profitable after gas
    }

    let net_profit = gross_output - optimal - gas_cost_wei;

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
    let total_cost = input + gas_cost_wei;
    if output > total_cost {
        output - total_cost
    } else {
        U256::zero()
    }
}

/// Compute the left interior point: a + (1 - φ)(b - a)
#[inline]
fn golden_subtract(a: U256, b: U256) -> U256 {
    let diff = b - a;
    let complement = U256::from(PHI_DEN - PHI_NUM);
    a + (diff * complement) / U256::from(PHI_DEN)
}

/// Compute the right interior point: a + φ(b - a)
#[inline]
fn golden_add(a: U256, b: U256) -> U256 {
    let diff = b - a;
    a + (diff * U256::from(PHI_NUM)) / U256::from(PHI_DEN)
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
}
