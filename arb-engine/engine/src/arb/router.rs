// ─────────────────────────────────────────────────────────────────────────────
//  arb/router.rs — Bellman-Ford Arbitrage Pathfinder
//
//  Strategy: Model the token swap graph as a directed weighted graph where:
//    - Nodes  = tokens
//    - Edges  = pool swaps (pool P allows swapping token A → token B)
//    - Weight = -log(exchange_rate)   (negative log-price)
//
//  A negative-weight CYCLE in this graph = an arbitrage opportunity.
//  Bellman-Ford detects negative cycles in O(V × E) time.
//
//  Why -log(rate)?
//    Profit from a cycle = product of exchange rates.
//    Product > 1 (profitable) ⟺ Σ log(rates) > 0 ⟺ Σ -log(rates) < 0.
//    So a profitable cycle = a negative-weight cycle in the -log graph.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use tracing::{debug, info};

use crate::arb::opportunity::{ArbitrageOpportunity, SwapStep};
use crate::pool::{v2, v3, Pool, PoolType, U256};

// ─────────────────────────────────────────────────────────────────────────────
//  Graph structures
// ─────────────────────────────────────────────────────────────────────────────

/// A directed edge in the liquidity graph: token_in → token_out via pool.
#[derive(Debug, Clone)]
struct LiquidityEdge {
    pool:        Arc<Pool>,
    token_in:    String,
    token_out:   String,
    /// -log(effective exchange rate after fees)
    neg_log_rate: f64,
}

/// The full liquidity graph across all monitored pools and chains.
pub struct LiquidityGraph {
    /// All unique token addresses (nodes)
    tokens: Vec<String>,
    /// Map: token_address → node index
    token_index: HashMap<String, usize>,
    /// All directed swap edges
    edges: Vec<LiquidityEdge>,
    /// Pools keyed by ID for fast lookup
    pools: HashMap<String, Arc<Pool>>,
}

impl Default for LiquidityGraph {
    fn default() -> Self {
        Self::new()
    }
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

    /// Drop all edges and pool entries, keeping the token-node index.
    /// Call periodically to purge stale pools without losing address mappings.
    pub fn clear_edges(&mut self) {
        self.edges.clear();
        self.pools.clear();
    }

    /// Insert or update a pool in the graph.
    /// Adds two directed edges (both swap directions).
    pub fn upsert_pool(&mut self, pool: Pool) {
        let pool_arc = Arc::new(pool);

        // Ensure both token nodes exist
        self.ensure_token(&pool_arc.token_a.address);
        self.ensure_token(&pool_arc.token_b.address);

        // Remove stale edges from this pool
        self.edges.retain(|e| e.pool.id != pool_arc.id);

        // Compute effective exchange rates
        let unit = U256::from(10u64.pow(18)); // 1 token in base units (18 dec normalised)

        // Edge A → B
        if let Some(rate_ab) = self.compute_rate(&pool_arc, unit, true) {
            self.edges.push(LiquidityEdge {
                pool:         Arc::clone(&pool_arc),
                token_in:     pool_arc.token_a.address.clone(),
                token_out:    pool_arc.token_b.address.clone(),
                neg_log_rate: -rate_ab.ln(),
            });
        }

        // Edge B → A
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

    /// Remove a pool from the graph.
    pub fn remove_pool(&mut self, pool_id: &str) {
        self.edges.retain(|e| e.pool.id != pool_id);
        self.pools.remove(pool_id);
    }

    /// Number of pools currently in the graph.
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Number of token nodes.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn ensure_token(&mut self, addr: &str) {
        if !self.token_index.contains_key(addr) {
            let idx = self.tokens.len();
            self.tokens.push(addr.to_string());
            self.token_index.insert(addr.to_string(), idx);
        }
    }

    /// Simulate a unit swap and return the effective exchange rate (output/input).
    /// Returns None if the pool cannot be swapped.
    fn compute_rate(&self, pool: &Pool, amount_in: U256, zero_for_one: bool) -> Option<f64> {
        let amount_out = match pool.pool_type {
            PoolType::ConstantProduct => {
                v2::get_amount_out(pool, amount_in, zero_for_one).ok()?
            }
            PoolType::ConcentratedLiquidity => {
                v3::get_amount_out_v3(pool, amount_in, zero_for_one).ok()?
            }
            PoolType::StableSwap => {
                // StableSwap: approximate as V2 for now
                v2::get_amount_out(pool, amount_in, zero_for_one).ok()?
            }
        };

        if amount_in.is_zero() { return None; }
        let rate = amount_out.low_u128() as f64 / amount_in.low_u128() as f64;
        if rate <= 0.0 { None } else { Some(rate) }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Bellman-Ford Arbitrage Finder
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the arbitrage router.
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Maximum number of hops in an arbitrage cycle (2 = two-pool, 3 = three-pool)
    pub max_hops: usize,
    /// Minimum cycle profit rate to consider (e.g. 0.001 = 0.1%)
    pub min_profit_rate: f64,
    /// Reference trade size (normalized, in 18-decimal units)
    pub reference_amount: U256,
    /// ETH price in USD for NEV calculation
    pub eth_price_usd: f64,
    /// Current gas price in gwei
    pub gas_price_gwei: f64,
    /// Gas units per hop (rough estimate)
    pub gas_per_hop: u64,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            max_hops:         3,
            min_profit_rate:  0.001,  // 0.1%
            reference_amount: U256::from(1_000_000_000_000_000_000u128), // 1 ETH
            eth_price_usd:    3000.0,
            gas_price_gwei:   20.0,
            gas_per_hop:      150_000,
        }
    }
}

/// Runs Bellman-Ford on the liquidity graph to find negative-weight cycles
/// (arbitrage opportunities).
///
/// Returns a list of profitable opportunities sorted by NEV descending.
pub fn find_arbitrage_cycles(
    graph: &LiquidityGraph,
    config: &RouterConfig,
) -> Vec<ArbitrageOpportunity> {
    if graph.tokens.is_empty() || graph.edges.is_empty() {
        return Vec::new();
    }

    let n = graph.tokens.len();

    // Bellman-Ford: relax all edges (n-1) times, then one more pass to detect
    // negative cycles. We track the predecessor edge to reconstruct paths.
    let inf = f64::INFINITY;
    let mut dist = vec![inf; n];
    let mut pred_edge: Vec<Option<usize>> = vec![None; n];

    // Start from every node (handles disconnected components)
    // For efficiency we use a virtual source with 0-cost edges
    let virtual_dist = 0.0;
    for i in 0..n {
        dist[i] = virtual_dist; // treat all nodes as equally reachable
    }

    // n-1 relaxation passes
    for _ in 0..(n.saturating_sub(1)) {
        let mut updated = false;
        for (ei, edge) in graph.edges.iter().enumerate() {
            let u = match graph.token_index.get(&edge.token_in) {
                Some(&i) => i,
                None => continue,
            };
            let v = match graph.token_index.get(&edge.token_out) {
                Some(&i) => i,
                None => continue,
            };
            if dist[u] + edge.neg_log_rate < dist[v] {
                dist[v] = dist[u] + edge.neg_log_rate;
                pred_edge[v] = Some(ei);
                updated = true;
            }
        }
        if !updated { break; }
    }

    // One more pass: any node whose distance still decreases is on a negative cycle
    let mut cycle_starts: Vec<usize> = Vec::new();
    for edge in graph.edges.iter() {
        let u = match graph.token_index.get(&edge.token_in) {
            Some(&i) => i,
            None => continue,
        };
        let v = match graph.token_index.get(&edge.token_out) {
            Some(&i) => i,
            None => continue,
        };
        if dist[u] + edge.neg_log_rate < dist[v] - 1e-10 {
            if !cycle_starts.contains(&v) {
                cycle_starts.push(v);
            }
        }
    }

    debug!("Bellman-Ford found {} potential negative cycles", cycle_starts.len());

    // Reconstruct cycles and build ArbitrageOpportunity for each
    let mut opportunities: Vec<ArbitrageOpportunity> = Vec::new();

    for start_node in cycle_starts {
        if let Some(opp) = reconstruct_cycle(graph, config, start_node, &pred_edge) {
            opportunities.push(opp);
        }
    }

    // Sort by NEV descending
    opportunities.sort_by(|a, b| b.net_expected_value.cmp(&a.net_expected_value));

    if !opportunities.is_empty() {
        info!("Router found {} profitable cycles", opportunities.len());
    }

    opportunities
}

/// Walk back predecessor edges to extract the cycle, then build an `ArbitrageOpportunity`.
fn reconstruct_cycle(
    graph: &LiquidityGraph,
    config: &RouterConfig,
    start: usize,
    pred_edge: &[Option<usize>],
) -> Option<ArbitrageOpportunity> {
    // Walk predecessor chain to find the actual cycle node
    let mut visited = vec![false; graph.tokens.len()];
    let mut cur = start;

    for _ in 0..graph.tokens.len() {
        if visited[cur] { break; }
        visited[cur] = true;
        cur = match pred_edge[cur] {
            Some(ei) => match graph.token_index.get(&graph.edges[ei].token_in) {
                Some(&i) => i,
                None => return None,
            },
            None => return None,
        };
    }

    let cycle_entry = cur;
    let mut cycle_edges: Vec<&LiquidityEdge> = Vec::new();
    let mut node = cycle_entry;

    // Extract the cycle edges (max_hops)
    for _ in 0..config.max_hops {
        let ei = pred_edge[node]?;
        let edge = &graph.edges[ei];
        cycle_edges.push(edge);
        node = graph.token_index.get(&edge.token_in).copied()?;
        if node == cycle_entry && !cycle_edges.is_empty() {
            break;
        }
    }

    if cycle_edges.len() < 2 {
        return None; // need at least 2 hops to form a cycle
    }

    // Reverse so edges are in execution order
    cycle_edges.reverse();

    // Simulate actual amounts through the cycle
    let start_token = cycle_edges[0].token_in.clone();
    let chain = cycle_edges[0].pool.chain;
    let mut current_amount = config.reference_amount;
    let mut steps: Vec<SwapStep> = Vec::new();
    let mut total_fees = U256::zero();

    for edge in &cycle_edges {
        let pool = &edge.pool;
        let zero_for_one = edge.token_in == pool.token_a.address;

        let out = match pool.pool_type {
            PoolType::ConstantProduct => v2::get_amount_out(pool, current_amount, zero_for_one).ok()?,
            PoolType::ConcentratedLiquidity => v3::get_amount_out_v3(pool, current_amount, zero_for_one).ok()?,
            PoolType::StableSwap => v2::get_amount_out(pool, current_amount, zero_for_one).ok()?,
        };

        let impact_bps = match pool.pool_type {
            PoolType::ConstantProduct => v2::price_impact_bps(pool, current_amount, zero_for_one),
            PoolType::ConcentratedLiquidity => v3::price_impact_bps_v3(pool, current_amount, zero_for_one),
            PoolType::StableSwap => v2::price_impact_bps(pool, current_amount, zero_for_one),
        };

        // Fee cost for this hop (fee_bps / 10000 * amount_in)
        let fee = current_amount * U256::from(pool.fee_bps) / U256::from(10_000u32);
        total_fees = total_fees + fee;

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
    let gas_units    = config.gas_per_hop * cycle_edges.len() as u64;
    let max_impact   = steps.iter().map(|s| s.step_price_impact_bps).max().unwrap_or(0);

    // Only proceed if there is a positive gross spread
    if gross_output <= config.reference_amount {
        return None;
    }

    let mut opp = ArbitrageOpportunity::new(
        steps,
        start_token,
        chain,
        config.reference_amount,
        gross_output,
        gas_units,
        config.gas_price_gwei,
        total_fees,
        max_impact,
        0, // block number filled in by caller
    );

    opp.calculate_nev(config.eth_price_usd);

    Some(opp)
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
                reserve_a: U256::from(ra),
                reserve_b: U256::from(rb),
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
    fn test_graph_upsert() {
        let mut graph = LiquidityGraph::new();
        graph.upsert_pool(make_v2_pool(
            "pool1", 1_000_000e18 as u128, 2_000_000e18 as u128,
            "0xWETH", "0xUSDC", 30
        ));
        assert_eq!(graph.pool_count(), 1);
        assert_eq!(graph.token_count(), 2);
        assert_eq!(graph.edges.len(), 2); // both directions
    }

    #[test]
    fn test_no_cycle_balanced_pools() {
        // Two pools with IDENTICAL rates: no arb opportunity should exist
        // Both pools: WETH→USDC at 1:2 ratio
        let mut graph = LiquidityGraph::new();
        graph.upsert_pool(make_v2_pool(
            "pool1", 1_000_000_000_000_000_000_000u128, 2_000_000_000_000_000_000_000u128,
            "WETH", "USDC", 30
        ));
        // pool2: same pair, same direction, same ratio — no discrepancy
        graph.upsert_pool(make_v2_pool(
            "pool2", 1_000_000_000_000_000_000_000u128, 2_000_000_000_000_000_000_000u128,
            "WETH", "USDC", 30
        ));
        let config = RouterConfig::default();
        let opps = find_arbitrage_cycles(&graph, &config);
        // Balanced pools should yield no profitable cycles after fees
        for opp in &opps {
            assert!(!opp.is_executable, "Balanced pools should not be executable");
        }
    }
}
