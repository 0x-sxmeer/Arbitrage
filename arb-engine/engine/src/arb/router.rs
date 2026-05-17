// ─────────────────────────────────────────────────────────────────────────────
//  arb/router.rs — Bellman-Ford Arbitrage Pathfinder (REFACTORED)
//
//  KEY FIXES vs. original:
//
//  1. VIRTUAL-SOURCE BELLMAN-FORD IS MATHEMATICALLY WRONG (CRITICAL)
//     The original initialises all dist[i] = 0.0 (virtual source).  This means
//     every node is "reachable at cost 0" and the algorithm finds phantom
//     negative cycles where none exist — specifically, any two pools with
//     combined neg_log_rate < 0 will trigger a false positive even without
//     forming a true CYCLE.
//     FIX: Classic single-source BF.  We iterate over all tokens as source and
//     stop as soon as we detect a genuine cycle anchored to that source.  Use
//     the "relaxation with predecessor tracking" only once the cycle node is
//     verified to be reachable from the source.
//
//  2. NEG_LOG_RATE IGNORES DEX FEE TIERS (CRITICAL)
//     The original computes `compute_rate` from v2/v3 math (which does subtract
//     the fee), but then the Bellman-Ford ALSO subtracts fees again in
//     `reconstruct_cycle` via `fee = current_amount * fee_bps / 10_000`.
//     Double-counting fees = artificially pessimistic NEV → real opportunities
//     discarded.
//     FIX: Fee is baked into v2::get_amount_out / v3::get_amount_out_v3 already.
//     The reconstruct path uses the same math — do NOT subtract fee separately.
//     The NEV formula now only adds gas cost on top.
//
//  3. CYCLE RECONSTRUCTION CAN LOOP INFINITELY (CRITICAL)
//     If `pred_edge` contains a chain that never closes back to `cycle_entry`
//     (e.g. due to numerical noise in dist), the inner `for` loop in
//     `reconstruct_cycle` will silently collect `max_hops` arbitrary edges that
//     do NOT form a valid cycle, then pass them on as a valid opportunity.
//     FIX: Verify closure: after collecting edges, assert
//     `steps.last().token_out == start_token`.  Discard if not closed.
//
//  4. FLOATING-POINT EPSILON TOO LOOSE
//     The original uses `dist[u] + neg_log_rate < dist[v] - 1e-10` which is
//     adequate, but 1e-10 may be too tight for f64 accumulated error across
//     long chains.  Upgraded to 1e-9 with a comment.
//
//  5. LiquidityGraph MISSING get_pool() ACCESSOR
//     listener.rs fix #1 needs `graph.get_pool(&id)`.  Added here.
//
//  6. RATE COMPUTATION USES low_u128() WHICH TRUNCATES LARGE U256
//     For amounts > 2^128, low_u128() silently wraps.  For typical 1-ETH
//     probe amounts this is fine, but document the assumption and add a guard.
//
//  7. TWO-HOP FINDER (`find_opportunities`) DOESN'T CAP PRICE IMPACT
//     High-impact paths will lose money in practice.  Added configurable
//     max_price_impact_bps guard.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::arb::opportunity::{ArbitrageOpportunity, SwapStep};
use crate::pool::{v2, v3, Pool, PoolType, U256};

// ─────────────────────────────────────────────────────────────────────────────
//  Graph structures
// ─────────────────────────────────────────────────────────────────────────────

/// A directed edge: token_in → token_out via a specific pool.
#[derive(Debug, Clone)]
struct LiquidityEdge {
    pool:         Arc<Pool>,
    token_in:     String,
    token_out:    String,
    /// -ln(effective_rate_after_fees).  Negative when profitable.
    neg_log_rate: f64,
}

/// The full liquidity graph across all monitored pools and chains.
pub struct LiquidityGraph {
    tokens:      Vec<String>,
    token_index: HashMap<String, usize>,
    edges:       Vec<LiquidityEdge>,
    pools:       HashMap<String, Arc<Pool>>,
}

impl Default for LiquidityGraph {
    fn default() -> Self { Self::new() }
}

impl LiquidityGraph {
    pub fn new() -> Self {
        Self {
            tokens:      Vec::new(),
            token_index: HashMap::new(),
            edges:       Vec::new(),
            pools:       HashMap::new(),
        }
    }

    /// Purge all edges/pools — keeps token index to avoid re-allocating.
    pub fn clear_edges(&mut self) {
        self.edges.clear();
        self.pools.clear();
    }

    /// Insert or update a pool.  Adds two directed edges (both directions).
    pub fn upsert_pool(&mut self, pool: Pool) {
        let pool_arc = Arc::new(pool);
        self.ensure_token(&pool_arc.token_a.address);
        self.ensure_token(&pool_arc.token_b.address);
        self.edges.retain(|e| e.pool.id != pool_arc.id);

        // FIX #6: use 1 ETH as probe — document u128 assumption
        let unit = U256::from(10u64.pow(18)); // 1 token in 18-decimal units (< 2^60, safe)

        if let Some(rate_ab) = self.compute_rate(&pool_arc, unit, true) {
            self.edges.push(LiquidityEdge {
                pool:         Arc::clone(&pool_arc),
                token_in:     pool_arc.token_a.address.clone(),
                token_out:    pool_arc.token_b.address.clone(),
                neg_log_rate: -rate_ab.ln(),
            });
        }

        if let Some(rate_ba) = self.compute_rate(&pool_arc, unit, false) {
            self.edges.push(LiquidityEdge {
                pool:         Arc::clone(&pool_arc),
                token_in:     pool_arc.token_b.address.clone(),
                token_out:    pool_arc.token_a.address.clone(),
                neg_log_rate: -rate_ba.ln(),
            });
        }

        self.pools.insert(pool_arc.id.clone(), pool_arc);
    }

    pub fn remove_pool(&mut self, pool_id: &str) {
        self.edges.retain(|e| e.pool.id != pool_id);
        self.pools.remove(pool_id);
    }

    pub fn pool_count(&self)  -> usize { self.pools.len()  }
    pub fn token_count(&self) -> usize { self.tokens.len() }

    /// FIX #5: Accessor needed by listener for staleness check.
    pub fn get_pool(&self, pool_id: &str) -> Option<&Arc<Pool>> {
        self.pools.get(pool_id)
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn ensure_token(&mut self, addr: &str) {
        if !self.token_index.contains_key(addr) {
            let idx = self.tokens.len();
            self.tokens.push(addr.to_string());
            self.token_index.insert(addr.to_string(), idx);
        }
    }

    /// Simulate a unit swap and return effective rate (output/input, post-fee).
    /// FIX #2: fee IS already baked into v2/v3 math.  No separate deduction.
    fn compute_rate(&self, pool: &Pool, amount_in: U256, zero_for_one: bool) -> Option<f64> {
        let amount_out = match pool.pool_type {
            PoolType::ConstantProduct    => v2::get_amount_out(pool, amount_in, zero_for_one).ok()?,
            PoolType::ConcentratedLiquidity => v3::get_amount_out_v3(pool, amount_in, zero_for_one).ok()?,
            PoolType::StableSwap         => v2::get_amount_out(pool, amount_in, zero_for_one).ok()?,
        };

        if amount_in.is_zero() { return None; }

        // FIX #6: guard against amounts that exceed u128 (shouldn't happen at 1 ETH)
        let in_u128  = amount_in.low_u128();
        let out_u128 = amount_out.low_u128();
        if amount_in  != U256::from(in_u128)
        || amount_out != U256::from(out_u128) {
            warn!("compute_rate: U256 overflow in low_u128 — skipping edge");
            return None;
        }

        let rate = out_u128 as f64 / in_u128 as f64;
        if rate <= 0.0 || !rate.is_finite() { None } else { Some(rate) }
    }

    // ── Two-hop scanner ───────────────────────────────────────────────────────

    /// FIX #7: Added max_price_impact_bps guard.
    pub fn find_opportunities(
        &self,
        start_token: &str,
        config: &RouterConfig,
    ) -> Vec<ArbitrageOpportunity> {
        let mut opportunities = Vec::new();

        let first_hops: Vec<&LiquidityEdge> = self.edges.iter()
            .filter(|e| e.token_in == start_token)
            .collect();

        for edge_1 in &first_hops {
            let mid = &edge_1.token_out;

            let second_hops: Vec<&LiquidityEdge> = self.edges.iter()
                .filter(|e| e.token_in == *mid && e.token_out == start_token)
                .collect();

            for edge_2 in &second_hops {
                if edge_1.pool.id == edge_2.pool.id { continue; }

                if let Some(mut arb) = self.evaluate_two_hop(
                    start_token, mid, &edge_1.pool, &edge_2.pool, config,
                ) {
                    arb.calculate_nev(config.eth_price_usd);
                    if arb.is_executable {
                        opportunities.push(arb);
                    }
                }
            }
        }
        opportunities
    }

    fn evaluate_two_hop(
        &self,
        start_token: &str,
        intermediate_token: &str,
        pool1: &Pool,
        pool2: &Pool,
        config: &RouterConfig,
    ) -> Option<ArbitrageOpportunity> {
        let input = config.reference_amount;

        let zfo1 = start_token == pool1.token_a.address;
        let out1 = sim_out(pool1, input, zfo1)?;
        let impact1 = sim_impact(pool1, input, zfo1);

        // FIX #7: reject paths with extreme single-hop impact
        if impact1 > config.max_price_impact_bps {
            return None;
        }

        let zfo2 = intermediate_token == pool2.token_a.address;
        let out2 = sim_out(pool2, out1, zfo2)?;
        let impact2 = sim_impact(pool2, out1, zfo2);

        if impact2 > config.max_price_impact_bps {
            return None;
        }

        let step1 = SwapStep {
            pool_id:               pool1.id.clone(),
            dex:                   pool1.dex.name().to_string(),
            chain:                 pool1.chain,
            token_in:              start_token.to_string(),
            token_out:             intermediate_token.to_string(),
            amount_in:             input,
            expected_amount_out:   out1,
            fee_bps:               pool1.fee_bps,
            step_price_impact_bps: impact1,
        };

        let step2 = SwapStep {
            pool_id:               pool2.id.clone(),
            dex:                   pool2.dex.name().to_string(),
            chain:                 pool2.chain,
            token_in:              intermediate_token.to_string(),
            token_out:             start_token.to_string(),
            amount_in:             out1,
            expected_amount_out:   out2,
            fee_bps:               pool2.fee_bps,
            step_price_impact_bps: impact2,
        };

        // FIX #2: do NOT add fee_bps-derived fee here — it's already inside out1/out2.
        // total_swap_fees_wei passed to ArbitrageOpportunity::new() is an informational
        // figure only (used for display).  Pass 0 so NEV isn't double-counted.
        let max_impact = impact1.max(impact2);

        Some(ArbitrageOpportunity::new(
            vec![step1, step2],
            start_token.to_string(),
            pool1.chain,
            input,
            out2,
            config.gas_per_hop * 2,
            config.gas_price_gwei,
            U256::zero(), // fees already baked into swap math
            max_impact,
            0,
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  RouterConfig
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub max_hops:              usize,
    pub min_profit_rate:       f64,
    pub reference_amount:      U256,
    pub eth_price_usd:         f64,
    pub gas_price_gwei:        f64,
    pub gas_per_hop:           u64,
    /// FIX #7: reject legs with more than this many bps price impact
    pub max_price_impact_bps:  u32,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            max_hops:             3,
            min_profit_rate:      0.001,
            reference_amount:     U256::from(1_000_000_000_000_000_000u128), // 1 ETH
            eth_price_usd:        3000.0,
            gas_price_gwei:       20.0,
            gas_per_hop:          150_000,
            max_price_impact_bps: 200, // 2% maximum per hop
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Bellman-Ford Cycle Finder
// ─────────────────────────────────────────────────────────────────────────────

/// FIX #1: Proper single-source Bellman-Ford.
///
/// We run BF from each token individually (handles disconnected components).
/// "Virtual source" initialisation was REMOVED because it produced false
/// positives — any path with total_neg_log < 0 was flagged as a cycle.
pub fn find_arbitrage_cycles(
    graph: &LiquidityGraph,
    config: &RouterConfig,
) -> Vec<ArbitrageOpportunity> {
    if graph.tokens.is_empty() || graph.edges.is_empty() {
        return Vec::new();
    }

    let n = graph.tokens.len();
    let mut opportunities: Vec<ArbitrageOpportunity> = Vec::new();
    let mut seen_cycle_ids: std::collections::HashSet<String> = Default::default();

    // Run BF from each token as source to cover disconnected components.
    // This is O(V² × E) worst case; in practice token count is small (< 200).
    for src_idx in 0..n {
        let inf = f64::INFINITY;
        let mut dist: Vec<f64>            = vec![inf; n];
        let mut pred_edge: Vec<Option<usize>> = vec![None; n];

        dist[src_idx] = 0.0;

        // n-1 relaxation rounds
        let mut updated = true;
        for _round in 0..(n.saturating_sub(1)) {
            if !updated { break; }
            updated = false;
            for (ei, edge) in graph.edges.iter().enumerate() {
                let u = match graph.token_index.get(&edge.token_in)  { Some(&i) => i, None => continue };
                let v = match graph.token_index.get(&edge.token_out) { Some(&i) => i, None => continue };
                if dist[u] == inf { continue; }
                let new_dist = dist[u] + edge.neg_log_rate;
                if new_dist < dist[v] {
                    dist[v] = new_dist;
                    pred_edge[v] = Some(ei);
                    updated = true;
                }
            }
        }

        // Nth round: any node that still relaxes is on a negative cycle
        let mut cycle_nodes: Vec<usize> = Vec::new();
        for edge in graph.edges.iter() {
            let u = match graph.token_index.get(&edge.token_in)  { Some(&i) => i, None => continue };
            let v = match graph.token_index.get(&edge.token_out) { Some(&i) => i, None => continue };
            if dist[u] == inf { continue; }
            // FIX #4: 1e-9 epsilon for accumulated float error
            if dist[u] + edge.neg_log_rate < dist[v] - 1e-9 {
                if !cycle_nodes.contains(&v) {
                    cycle_nodes.push(v);
                }
            }
        }

        for start_node in cycle_nodes {
            if let Some(opp) = reconstruct_cycle(graph, config, start_node, &pred_edge) {
                // Dedup by route fingerprint
                let fingerprint = opp.route_description();
                if seen_cycle_ids.insert(fingerprint) {
                    opportunities.push(opp);
                }
            }
        }
    }

    opportunities.sort_by(|a, b| b.net_expected_value.cmp(&a.net_expected_value));

    if !opportunities.is_empty() {
        info!("Bellman-Ford found {} profitable cycles", opportunities.len());
    }

    opportunities
}

/// FIX #3: Verify cycle closure before returning the opportunity.
fn reconstruct_cycle(
    graph: &LiquidityGraph,
    config: &RouterConfig,
    start: usize,
    pred_edge: &[Option<usize>],
) -> Option<ArbitrageOpportunity> {
    // Walk predecessor chain to find the actual entry of the cycle
    let mut visited = vec![false; graph.tokens.len()];
    let mut cur = start;

    for _ in 0..graph.tokens.len() {
        if visited[cur] { break; }
        visited[cur] = true;
        cur = match pred_edge[cur] {
            Some(ei) => match graph.token_index.get(&graph.edges[ei].token_in) {
                Some(&i) => i,
                None     => return None,
            },
            None => return None,
        };
    }

    let cycle_entry = cur;
    let mut cycle_edges: Vec<&LiquidityEdge> = Vec::new();
    let mut node = cycle_entry;

    for _ in 0..config.max_hops {
        let ei   = pred_edge[node]?;
        let edge = &graph.edges[ei];
        cycle_edges.push(edge);
        node = graph.token_index.get(&edge.token_in).copied()?;
        if node == cycle_entry { break; }
    }

    if cycle_edges.len() < 2 { return None; }

    cycle_edges.reverse(); // now in execution order

    // ── FIX #3: Verify the path truly closes back to cycle_entry ─────────────
    let start_token = cycle_edges.first()?.token_in.clone();
    let end_token   = cycle_edges.last()?.token_out.clone();
    if start_token != end_token {
        debug!(
            "Cycle not closed: {} → {} — skipping (pred-chain artefact)",
            start_token, end_token
        );
        return None;
    }

    let chain = cycle_edges[0].pool.chain;
    let mut current_amount = config.reference_amount;
    let mut steps: Vec<SwapStep> = Vec::new();
    let mut max_impact: u32 = 0;

    for edge in &cycle_edges {
        let pool       = &edge.pool;
        let zfo        = edge.token_in == pool.token_a.address;
        let out        = sim_out(pool, current_amount, zfo)?;
        let impact_bps = sim_impact(pool, current_amount, zfo);

        // FIX #7: reject paths with extreme impact inside the BF result too
        if impact_bps > config.max_price_impact_bps {
            return None;
        }
        max_impact = max_impact.max(impact_bps);

        steps.push(SwapStep {
            pool_id:               pool.id.clone(),
            dex:                   pool.dex.name().to_string(),
            chain:                 pool.chain,
            token_in:              edge.token_in.clone(),
            token_out:             edge.token_out.clone(),
            amount_in:             current_amount,
            expected_amount_out:   out,
            fee_bps:               pool.fee_bps,
            step_price_impact_bps: impact_bps,
        });

        current_amount = out;
    }

    let gross_output = current_amount;

    // Only proceed if gross spread is positive (fees already deducted by sim)
    if gross_output <= config.reference_amount {
        return None;
    }

    let gas_units = config.gas_per_hop * cycle_edges.len() as u64;

    // FIX #2: pass U256::zero() for total_swap_fees_wei — fees are inside the
    // simulated outputs; passing fee_bps-derived amounts would double-count.
    let mut opp = ArbitrageOpportunity::new(
        steps,
        start_token,
        chain,
        config.reference_amount,
        gross_output,
        gas_units,
        config.gas_price_gwei,
        U256::zero(),
        max_impact,
        0,
    );

    opp.calculate_nev(config.eth_price_usd);
    Some(opp)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Pool simulation helpers (DRY wrappers)
// ─────────────────────────────────────────────────────────────────────────────

#[inline]
fn sim_out(pool: &Pool, amount_in: U256, zero_for_one: bool) -> Option<U256> {
    match pool.pool_type {
        PoolType::ConstantProduct       => v2::get_amount_out(pool, amount_in, zero_for_one).ok(),
        PoolType::ConcentratedLiquidity => v3::get_amount_out_v3(pool, amount_in, zero_for_one).ok(),
        PoolType::StableSwap            => v2::get_amount_out(pool, amount_in, zero_for_one).ok(),
    }
}

#[inline]
fn sim_impact(pool: &Pool, amount_in: U256, zero_for_one: bool) -> u32 {
    match pool.pool_type {
        PoolType::ConstantProduct       => v2::price_impact_bps(pool, amount_in, zero_for_one),
        PoolType::ConcentratedLiquidity => v3::price_impact_bps_v3(pool, amount_in, zero_for_one),
        PoolType::StableSwap            => v2::price_impact_bps(pool, amount_in, zero_for_one),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{ChainId, DexProtocol, Pool, PoolState, PoolType, Token};

    fn make_v2_pool(id: &str, ra: u128, rb: u128, token_a: &str, token_b: &str, fee: u32) -> Pool {
        Pool {
            id: id.into(),
            chain: ChainId::Ethereum,
            dex: DexProtocol::UniswapV2,
            token_a: Token { address: token_a.into(), symbol: "TKA".into(), decimals: 18 },
            token_b: Token { address: token_b.into(), symbol: "TKB".into(), decimals: 18 },
            pool_type: PoolType::ConstantProduct,
            fee_bps: fee,
            state: PoolState {
                reserve_a:      U256::from(ra),
                reserve_b:      U256::from(rb),
                sqrt_price_x96: None,
                tick:           None,
                liquidity:      None,
                amp_coeff:      None,
            },
            last_updated_block: 1,
            last_updated_ts:    0,
        }
    }

    #[test]
    fn test_graph_upsert() {
        let mut graph = LiquidityGraph::new();
        graph.upsert_pool(make_v2_pool(
            "pool1", 1_000_000_000_000_000_000_000, 2_000_000_000_000_000_000_000,
            "0xWETH", "0xUSDC", 30,
        ));
        assert_eq!(graph.pool_count(), 1);
        assert_eq!(graph.token_count(), 2);
        assert_eq!(graph.edges.len(), 2);
    }

    #[test]
    fn test_balanced_pools_no_opportunity() {
        // Two pools with identical rates: fees ensure no arb after BF.
        let mut graph = LiquidityGraph::new();
        // WETH → USDC at 1:2 on pool1
        graph.upsert_pool(make_v2_pool(
            "p1", 1_000_000_000_000_000_000_000, 2_000_000_000_000_000_000_000,
            "WETH", "USDC", 30,
        ));
        // USDC → WETH at 2:1 on pool2 (perfectly symmetric — should break even)
        graph.upsert_pool(make_v2_pool(
            "p2", 2_000_000_000_000_000_000_000, 1_000_000_000_000_000_000_000,
            "USDC", "WETH", 30,
        ));
        let config = RouterConfig::default();
        let opps = find_arbitrage_cycles(&graph, &config);
        for opp in &opps {
            assert!(
                !opp.is_executable,
                "Balanced pools should not be executable (NEV = {}, fee eats all profit)",
                opp.net_expected_value
            );
        }
    }

    #[test]
    fn test_imbalanced_pools_finds_opportunity() {
        let mut graph = LiquidityGraph::new();
        // Pool A: WETH → USDC at 1:2100 (slightly above market)
        graph.upsert_pool(make_v2_pool(
            "p1", 1_000_000_000_000_000_000_000, 2_100_000_000_000_000_000_000_000,
            "WETH", "USDC", 30,
        ));
        // Pool B: USDC → WETH at 2000:1 (below market — WETH is cheap here)
        graph.upsert_pool(make_v2_pool(
            "p2", 2_000_000_000_000_000_000_000_000, 1_000_000_000_000_000_000_000,
            "USDC", "WETH", 30,
        ));
        let config = RouterConfig::default();
        let opps = find_arbitrage_cycles(&graph, &config);
        // At least one cycle should show positive gross spread
        let any_profitable = opps.iter().any(|o| o.gross_output > o.input_amount);
        assert!(any_profitable, "Imbalanced pools should yield at least one profitable cycle");
    }

    #[test]
    fn test_cycle_closure_required() {
        // Single pool — cannot form a closed cycle by itself
        let mut graph = LiquidityGraph::new();
        graph.upsert_pool(make_v2_pool(
            "p1", 1_000_000_000_000_000_000_000, 2_000_000_000_000_000_000_000,
            "WETH", "USDC", 30,
        ));
        let config = RouterConfig::default();
        let opps = find_arbitrage_cycles(&graph, &config);
        assert!(opps.is_empty(), "Single pool cannot form a cycle");
    }

    #[test]
    fn test_get_pool_accessor() {
        let mut graph = LiquidityGraph::new();
        graph.upsert_pool(make_v2_pool(
            "pool99", 1_000, 2_000, "A", "B", 30,
        ));
        assert!(graph.get_pool("pool99").is_some());
        assert!(graph.get_pool("pool00").is_none());
    }
}
