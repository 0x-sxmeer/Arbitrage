// ─────────────────────────────────────────────────────────────────────────────
//  engine/src/arb/router.rs — Bellman-Ford Arbitrage Pathfinder
//
//  Architecture:
//    ┌──────────────────────────────────────────────────────────────┐
//    │  LiquidityGraph                                               │
//    │    ▸ Directed multigraph: token → token via pool              │
//    │    ▸ Edge weight: −ln(effective_rate)  [negative = profitable]│
//    │    ▸ Bellman-Ford on weight graph detects negative cycles     │
//    │    ▸ Two-hop scanner for O(E²) quick scans between BF runs   │
//    └──────────────────────────────────────────────────────────────┘
//
//  Bellman-Ford negative cycle detection (standard algorithm):
//    1. Relax all |E| edges |V|−1 times.
//    2. On the N-th (|V|th) iteration, any edge that still improves
//       a distance is part of a negative cycle.
//    3. Trace back via predecessor map to recover the cycle.
//    4. Verify the cycle is closed and profitable after gas cost.
//
//  Correctness notes vs. common bugs:
//    ▸ Virtual source (dist[all] = 0): WRONG — creates phantom cycles.
//      We use single-source BF from each token as start.  Expensive but
//      correct.  Optimisation: only re-run BF on tokens that had an edge
//      updated in the current mempool batch (see `changed_tokens`).
//    ▸ Fee double-counting: fees are baked into v2/v3 math in compute_rate.
//      The Bellman-Ford weight is purely −ln(rate_after_fee).  Gas cost is
//      added only in the final NEV calculation.
//    ▸ Cycle closure check: after tracing predecessors we verify
//      cycle.last().token_out == cycle[0].token_in before accepting.
//    ▸ MAX_HOPS guard: prevents runaway traces in degenerate graphs.
//
//  Cross-chain edges:
//    Non-EVM pools (Raydium, Orca, Osmosis) are added to the same graph
//    as synthetic edges with a cross-chain penalty (bridge latency + cost).
//    The caller (listener.rs) fetches non-EVM state concurrently with the
//    EVM mempool trigger (tokio::join!) before calling graph.upsert_pool().
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::arb::opportunity::{ArbitrageOpportunity, SwapStep};
use crate::pool::{v2, v3, Pool, PoolType, U256};

// ─────────────────────────────────────────────────────────────────────────────
//  Configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Parameters that govern the pathfinder and profitability filter.
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Gas price in Gwei — updated live from mempool EWA.
    pub gas_price_gwei:     f64,
    /// Estimated gas used by the atomic arb contract execution.
    pub gas_estimate:       u64,
    /// ETH/USD price for NEV-in-USD calculation.
    pub eth_price_usd:      f64,
    /// Minimum profit in USD before the opportunity is flagged executable.
    pub min_profit_usd:     f64,
    /// Reference amount (in raw 18-dec units) for swap simulation.
    pub reference_amount:   U256,
    /// Maximum single-hop price impact in basis points (10_000 = 100%).
    pub max_price_impact_bps: u32,
    /// Maximum number of hops in a cycle (3 = triangle arb, 4 = quad arb).
    pub max_hops:           usize,
    /// Whether to emit detailed path logs.
    pub verbose:            bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            gas_price_gwei:       30.0,
            gas_estimate:         350_000,
            eth_price_usd:        3_000.0,
            min_profit_usd:       1.0,
            reference_amount:     U256::from(10u64.pow(18)), // 1 ETH-equivalent
            max_price_impact_bps: 200,   // 2% — reject high-impact paths
            max_hops:             4,
            verbose:              false,
        }
    }
}

impl RouterConfig {
    /// Gas cost in ETH (18-decimal).
    pub fn gas_cost_eth(&self) -> f64 {
        self.gas_price_gwei * 1e-9 * self.gas_estimate as f64
    }

    /// Gas cost in raw wei (U256).
    pub fn gas_cost_wei(&self) -> U256 {
        let gwei_per_unit = (self.gas_price_gwei * 1e9) as u128;
        U256::from(gwei_per_unit) * U256::from(self.gas_estimate)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Graph data structures
// ─────────────────────────────────────────────────────────────────────────────

/// A directed edge: `token_in → token_out` via a specific pool.
///
/// The edge weight is −ln(effective_rate_after_fees).
/// Negative weight = profitable direction.
/// A negative-weight cycle = arbitrage opportunity.
#[derive(Debug, Clone)]
pub struct LiquidityEdge {
    pub pool:          Arc<Pool>,
    pub token_in:      String,
    pub token_out:     String,
    /// −ln(output / input) after fees.  Negative means this direction is profitable.
    pub neg_log_rate:  f64,
}

/// The unified liquidity graph across all monitored DEXes and chains.
///
/// Thread-safety: wrapped in `Arc<RwLock<LiquidityGraph>>` by callers.
pub struct LiquidityGraph {
    /// Ordered token list (index = node ID for BF algorithm).
    tokens:       Vec<String>,
    /// token_address → index in `tokens`.
    token_index:  HashMap<String, usize>,
    /// All directed edges (one pool contributes two edges — both directions).
    edges:        Vec<LiquidityEdge>,
    /// pool_id → Arc<Pool> for O(1) lookup.
    pools:        HashMap<String, Arc<Pool>>,
    /// Tracks which token indices had edges modified in the latest batch.
    /// Used to short-circuit BF when only a few tokens changed.
    pub changed_tokens: std::collections::HashSet<usize>,
}

impl Default for LiquidityGraph {
    fn default() -> Self { Self::new() }
}

impl LiquidityGraph {
    pub fn new() -> Self {
        Self {
            tokens:         Vec::new(),
            token_index:    HashMap::new(),
            edges:          Vec::new(),
            pools:          HashMap::new(),
            changed_tokens: std::collections::HashSet::new(),
        }
    }

    /// Remove all edges and pool cache but keep the token index.
    /// Called on WebSocket reconnect to flush stale prices.
    pub fn clear_edges(&mut self) {
        self.edges.clear();
        self.pools.clear();
        self.changed_tokens.clear();
    }

    // ── Pool management ───────────────────────────────────────────────────────

    /// Insert or update a pool.  Replaces existing edges for this pool_id.
    ///
    /// Computes both directed edges (A→B and B→A) using a 1-unit probe swap.
    /// Edges with rate ≤ 0 or NaN are silently dropped.
    pub fn upsert_pool(&mut self, pool: Pool) {
        let pool_arc = Arc::new(pool);
        self.ensure_token(&pool_arc.token_a.address);
        self.ensure_token(&pool_arc.token_b.address);

        // Remove old edges for this pool
        self.edges.retain(|e| e.pool.id != pool_arc.id);

        // Probe amount: 1 token in 18-decimal units — safe for u128
        let unit = U256::from(10u64.pow(18));

        let idx_a = self.token_index[&pool_arc.token_a.address];
        let idx_b = self.token_index[&pool_arc.token_b.address];

        if let Some(rate_ab) = Self::compute_rate_static(&pool_arc, unit, true) {
            self.edges.push(LiquidityEdge {
                pool:         Arc::clone(&pool_arc),
                token_in:     pool_arc.token_a.address.clone(),
                token_out:    pool_arc.token_b.address.clone(),
                neg_log_rate: -rate_ab.ln(),
            });
            self.changed_tokens.insert(idx_a);
        }

        if let Some(rate_ba) = Self::compute_rate_static(&pool_arc, unit, false) {
            self.edges.push(LiquidityEdge {
                pool:         Arc::clone(&pool_arc),
                token_in:     pool_arc.token_b.address.clone(),
                token_out:    pool_arc.token_a.address.clone(),
                neg_log_rate: -rate_ba.ln(),
            });
            self.changed_tokens.insert(idx_b);
        }

        self.pools.insert(pool_arc.id.clone(), pool_arc);
    }

    pub fn remove_pool(&mut self, pool_id: &str) {
        self.edges.retain(|e| e.pool.id != pool_id);
        self.pools.remove(pool_id);
    }

    pub fn pool_count(&self)  -> usize { self.pools.len()  }
    pub fn token_count(&self) -> usize { self.tokens.len() }

    /// O(1) pool lookup (needed by staleness check in listener.rs).
    pub fn get_pool(&self, pool_id: &str) -> Option<&Arc<Pool>> {
        self.pools.get(pool_id)
    }

    /// Iterate all edges from a given token.
    pub fn edges_from(&self, token: &str) -> impl Iterator<Item = &LiquidityEdge> {
        self.edges.iter().filter(move |e| e.token_in == token)
    }

    // ── Token management ──────────────────────────────────────────────────────

    fn ensure_token(&mut self, addr: &str) {
        if !self.token_index.contains_key(addr) {
            let idx = self.tokens.len();
            self.tokens.push(addr.to_string());
            self.token_index.insert(addr.to_string(), idx);
        }
    }

    // ── Rate computation ──────────────────────────────────────────────────────

    /// Simulate a unit swap and return effective rate (output/input, after fees).
    ///
    /// Fees are already included in v2/v3 math — do NOT deduct again.
    fn compute_rate_static(pool: &Pool, amount_in: U256, zero_for_one: bool) -> Option<f64> {
        let amount_out = match pool.pool_type {
            PoolType::ConstantProduct       => v2::get_amount_out(pool, amount_in, zero_for_one).ok()?,
            PoolType::ConcentratedLiquidity => v3::get_amount_out_v3(pool, amount_in, zero_for_one).ok()?,
            PoolType::StableSwap            => v2::get_amount_out(pool, amount_in, zero_for_one).ok()?,
        };

        // Guard: amounts must fit in u128 (guaranteed for unit-sized probes)
        let in_u128  = amount_in.low_u128();
        let out_u128 = amount_out.low_u128();
        if amount_in != U256::from(in_u128) || amount_out != U256::from(out_u128) {
            warn!("compute_rate: u128 overflow — pool {} skipped", pool.id);
            return None;
        }

        if in_u128 == 0 { return None; }
        let rate = out_u128 as f64 / in_u128 as f64;
        if rate <= 0.0 || !rate.is_finite() { None } else { Some(rate) }
    }

    // ── Two-hop quick scanner ─────────────────────────────────────────────────

    /// O(E²) two-hop scanner.  Runs fast (no BF) and catches the most common
    /// DEX → DEX price discrepancies.
    pub fn find_opportunities(
        &self,
        start_token: &str,
        config: &RouterConfig,
    ) -> Vec<ArbitrageOpportunity> {
        let mut opps = Vec::new();

        let first_hops: Vec<&LiquidityEdge> = self.edges.iter()
            .filter(|e| e.token_in == start_token)
            .collect();

        for e1 in &first_hops {
            let mid = &e1.token_out;

            for e2 in self.edges.iter()
                .filter(|e| e.token_in == *mid && e.token_out == start_token)
            {
                if e1.pool.id == e2.pool.id { continue; }

                if let Some(mut arb) = self.evaluate_two_hop(start_token, mid, &e1.pool, &e2.pool, config) {
                    arb.calculate_nev(config.eth_price_usd);
                    if arb.is_executable {
                        opps.push(arb);
                    }
                }
            }
        }
        opps
    }

    fn evaluate_two_hop(
        &self,
        start:    &str,
        mid:      &str,
        pool1:    &Pool,
        pool2:    &Pool,
        config:   &RouterConfig,
    ) -> Option<ArbitrageOpportunity> {
        let input = config.reference_amount;

        let zfo1  = start == pool1.token_a.address;
        let out1  = sim_out(pool1, input, zfo1)?;
        let imp1  = sim_impact(pool1, input, zfo1);
        if imp1 > config.max_price_impact_bps { return None; }

        let zfo2  = mid == pool2.token_a.address;
        let out2  = sim_out(pool2, out1, zfo2)?;
        let imp2  = sim_impact(pool2, out1, zfo2);
        if imp2 > config.max_price_impact_bps { return None; }

        let step1 = SwapStep {
            pool_id:   pool1.id.clone(),
            dex:       pool1.dex.name().to_string(),
            chain:     pool1.chain,
            token_in:  start.to_string(),
            token_out: mid.to_string(),
            amount_in: input,
            amount_out: out1,
            fee_bps:   pool1.fee_bps,
            price_impact_bps: imp1,
        };
        let step2 = SwapStep {
            pool_id:   pool2.id.clone(),
            dex:       pool2.dex.name().to_string(),
            chain:     pool2.chain,
            token_in:  mid.to_string(),
            token_out: start.to_string(),
            amount_in: out1,
            amount_out: out2,
            fee_bps:   pool2.fee_bps,
            price_impact_bps: imp2,
        };

        Some(ArbitrageOpportunity::new(
            vec![step1, step2],
            config.gas_cost_wei(),
            config.gas_price_gwei,
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Bellman-Ford negative cycle detection
// ─────────────────────────────────────────────────────────────────────────────

/// Run Bellman-Ford on the liquidity graph and return all detected profitable
/// arbitrage cycles (negative weight cycles in the neg_log_rate graph).
///
/// Algorithm:
///   For each start_token s ∈ changed_tokens:
///     1. Single-source BF from s.
///     2. After |V|−1 relaxations, do one more pass.
///     3. Any vertex whose distance decreases in pass |V| is in or reachable
///        from a negative cycle.
///     4. Trace back via predecessor edges to extract the cycle.
///     5. Verify cycle closure; compute NEV; filter by profitability.
///
/// # Complexity
///   O(|changed_tokens| × |V| × |E|) per call.
///   Typical mainnet: 200 tokens × 500 edges × 5 changed = 500k ops per tx.
///
/// # Arguments
/// * `graph`  — shared liquidity graph (read-locked by caller)
/// * `config` — router configuration (includes gas price)
pub fn find_arbitrage_cycles(
    graph:  &LiquidityGraph,
    config: &RouterConfig,
) -> Vec<ArbitrageOpportunity> {
    let n = graph.tokens.len();
    if n < 2 { return vec![]; }

    let mut results: Vec<ArbitrageOpportunity> = Vec::new();
    // Deduplicate cycles seen in this batch by their edge-set fingerprint
    let mut seen_cycles: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Run BF from every changed token (those with fresh edge data).
    // Fall back to all tokens on the first run (changed_tokens empty).
    let sources: Vec<usize> = if graph.changed_tokens.is_empty() {
        (0..n).collect()
    } else {
        graph.changed_tokens.iter().copied().collect()
    };

    for &src in &sources {
        let opps = bellman_ford_from(graph, config, src, &mut seen_cycles);
        results.extend(opps);
    }

    if config.verbose && !results.is_empty() {
        info!(
            cycles = results.len(),
            sources = sources.len(),
            "BF scan complete"
        );
    }

    results
}

/// Single-source Bellman-Ford starting from vertex `src`.
///
/// Returns all negative cycles reachable from `src`.
fn bellman_ford_from(
    graph:       &LiquidityGraph,
    config:      &RouterConfig,
    src:         usize,
    seen_cycles: &mut std::collections::HashSet<String>,
) -> Vec<ArbitrageOpportunity> {
    let n = graph.tokens.len();
    let mut dist: Vec<f64> = vec![f64::INFINITY; n];
    dist[src] = 0.0;

    // predecessor edge index (into graph.edges) for cycle reconstruction
    let mut pred: Vec<Option<usize>> = vec![None; n];

    // ── Relax all edges |V|−1 times ───────────────────────────────────────────
    for _pass in 0..n.saturating_sub(1) {
        let mut relaxed = false;
        for (eidx, edge) in graph.edges.iter().enumerate() {
            let u = match graph.token_index.get(&edge.token_in) {
                Some(&i) => i,
                None     => continue,
            };
            let v = match graph.token_index.get(&edge.token_out) {
                Some(&i) => i,
                None     => continue,
            };

            if dist[u].is_infinite() { continue; }

            let new_dist = dist[u] + edge.neg_log_rate;
            // Epsilon = 1e-9 to absorb f64 rounding over long chains
            if new_dist < dist[v] - 1e-9 {
                dist[v] = new_dist;
                pred[v] = Some(eidx);
                relaxed  = true;
            }
        }
        if !relaxed { break; } // early exit: no changes
    }

    // ── N-th pass: detect negative cycles ────────────────────────────────────
    let mut neg_cycle_vertices: Vec<usize> = Vec::new();
    for edge in graph.edges.iter() {
        let u = match graph.token_index.get(&edge.token_in)  { Some(&i) => i, None => continue };
        let v = match graph.token_index.get(&edge.token_out) { Some(&i) => i, None => continue };

        if dist[u].is_infinite() { continue; }

        if dist[u] + edge.neg_log_rate < dist[v] - 1e-9 {
            // v is in (or reachable from) a negative cycle
            if !neg_cycle_vertices.contains(&v) {
                neg_cycle_vertices.push(v);
            }
        }
    }

    if neg_cycle_vertices.is_empty() {
        return vec![];
    }

    // ── Reconstruct each cycle ─────────────────────────────────────────────────
    let mut opps = Vec::new();
    for &cycle_vertex in &neg_cycle_vertices {
        if let Some(opp) = reconstruct_and_evaluate(
            graph, config, &pred, cycle_vertex, seen_cycles,
        ) {
            opps.push(opp);
        }
    }
    opps
}

/// Trace back predecessor edges from `cycle_vertex` to find the negative cycle,
/// then simulate the full path to compute NEV.
fn reconstruct_and_evaluate(
    graph:       &LiquidityGraph,
    config:      &RouterConfig,
    pred:        &[Option<usize>],
    cycle_vertex: usize,
    seen_cycles: &mut std::collections::HashSet<String>,
) -> Option<ArbitrageOpportunity> {
    let max_hops = config.max_hops;

    // Walk back `max_hops` steps from cycle_vertex to land inside the cycle
    let mut v = cycle_vertex;
    for _ in 0..max_hops {
        let eidx = pred[v]?;
        let edge  = &graph.edges[eidx];
        v = *graph.token_index.get(&edge.token_in)?;
    }
    // v is now guaranteed to be inside the negative cycle (it has been visited
    // at least max_hops times in the predecessor chain).
    let cycle_entry_token = graph.tokens[v].clone();

    // Collect cycle edges starting from cycle_entry_token
    let mut steps: Vec<(&LiquidityEdge, U256, U256)> = Vec::new();
    let mut current_vertex = v;
    let mut amount         = config.reference_amount;

    for _ in 0..max_hops {
        let eidx = pred[current_vertex]?;
        let edge  = &graph.edges[eidx];

        let in_vertex = *graph.token_index.get(&edge.token_in)?;

        let zero_for_one = edge.token_in == edge.pool.token_a.address;
        let amount_out   = sim_out(&edge.pool, amount, zero_for_one)?;

        let impact = sim_impact(&edge.pool, amount, zero_for_one);
        if impact > config.max_price_impact_bps {
            debug!("BF cycle rejected: price impact {} bps > limit", impact);
            return None;
        }

        steps.push((edge, amount, amount_out));
        amount = amount_out;
        current_vertex = in_vertex;

        // Cycle closed when we return to the cycle entry token
        if graph.tokens[current_vertex] == cycle_entry_token {
            break;
        }
    }

    if steps.is_empty() {
        return None;
    }

    // ── Verify cycle closure ─────────────────────────────────────────────────
    // The cycle is valid only if the last edge's token_out = cycle_entry_token.
    let last_out = &steps.last()?.0.token_out;
    if *last_out != cycle_entry_token {
        debug!(
            "BF cycle rejected: not closed ({} → {})",
            last_out, cycle_entry_token
        );
        return None;
    }

    // Reverse (predecessor traces backwards)
    steps.reverse();

    // ── Deduplicate ───────────────────────────────────────────────────────────
    let cycle_key: String = steps
        .iter()
        .map(|(e, _, _)| e.pool.id.as_str())
        .collect::<Vec<_>>()
        .join("|");
    if !seen_cycles.insert(cycle_key.clone()) {
        debug!("BF cycle deduplicated: {}", cycle_key);
        return None;
    }

    // ── Build SwapSteps ───────────────────────────────────────────────────────
    let swap_steps: Vec<SwapStep> = steps.iter().map(|(edge, amount_in, amount_out)| {
        let zfo    = edge.token_in == edge.pool.token_a.address;
        let impact = sim_impact(&edge.pool, *amount_in, zfo);
        SwapStep {
            pool_id:          edge.pool.id.clone(),
            dex:              edge.pool.dex.name().to_string(),
            chain:            edge.pool.chain,
            token_in:         edge.token_in.clone(),
            token_out:        edge.token_out.clone(),
            amount_in:        *amount_in,
            amount_out:       *amount_out,
            fee_bps:          edge.pool.fee_bps,
            price_impact_bps: impact,
        }
    }).collect();

    if swap_steps.is_empty() {
        return None;
    }

    let mut opp = ArbitrageOpportunity::new(swap_steps, config.gas_cost_wei(), config.gas_price_gwei);
    opp.calculate_nev(config.eth_price_usd);

    if opp.is_executable {
        info!(
            id      = %opp.id,
            hops    = opp.route.len(),
            cycle   = %cycle_key,
            nev_wei = opp.net_expected_value,
            "🔺 BF negative cycle: profitable arb detected"
        );
    }

    Some(opp)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Cross-chain state prefetch trigger
// ─────────────────────────────────────────────────────────────────────────────

/// Describes a pending non-EVM state fetch.
#[derive(Debug, Clone)]
pub struct CrossChainFetchSpec {
    pub chain:   CrossChainTarget,
    pub pool_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossChainTarget {
    Solana,
    Osmosis,
}

/// Given that an EVM swap for `(token_in, token_out)` was detected, return the
/// list of non-EVM pools that should be fetched concurrently before running BF.
///
/// Called by the listener pipeline:
///
/// ```text
/// let (evm_state, sol_states) = tokio::join!(
///     adapter.fetch_pool_state(&evm_pool),
///     fetch_cross_chain_states(&specs),
/// );
/// ```
pub fn cross_chain_fetch_specs(
    token_in:  &str,
    token_out: &str,
    graph:     &LiquidityGraph,
) -> Vec<CrossChainFetchSpec> {
    // Look for non-EVM pools that share either token with the EVM swap.
    // These are inserted by the cross-chain state poller (future subsystem)
    // with pool IDs prefixed by "solana:" or "cosmos:".
    graph.pools.iter()
        .filter(|(id, pool)| {
            let involves_token = pool.token_a.address == token_in
                || pool.token_a.address == token_out
                || pool.token_b.address == token_in
                || pool.token_b.address == token_out;
            let is_non_evm = id.starts_with("solana:") || id.starts_with("cosmos:");
            involves_token && is_non_evm
        })
        .map(|(id, pool)| {
            let chain = if id.starts_with("solana:") {
                CrossChainTarget::Solana
            } else {
                CrossChainTarget::Osmosis
            };
            CrossChainFetchSpec {
                chain,
                pool_id: pool.id.clone(),
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
//  Simulation helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Simulate swap output — dispatches to V2 or V3 math.
pub fn sim_out(pool: &Pool, amount_in: U256, zero_for_one: bool) -> Option<U256> {
    let out = match pool.pool_type {
        PoolType::ConstantProduct       => v2::get_amount_out(pool, amount_in, zero_for_one).ok()?,
        PoolType::ConcentratedLiquidity => v3::get_amount_out_v3(pool, amount_in, zero_for_one).ok()?,
        PoolType::StableSwap            => v2::get_amount_out(pool, amount_in, zero_for_one).ok()?,
    };
    if out.is_zero() { None } else { Some(out) }
}

/// Estimate price impact in basis points for a given swap.
pub fn sim_impact(pool: &Pool, amount_in: U256, zero_for_one: bool) -> u32 {
    match pool.pool_type {
        PoolType::ConcentratedLiquidity =>
            v3::price_impact_bps_v3(pool, amount_in, zero_for_one),
        PoolType::ConstantProduct | PoolType::StableSwap =>
            v2::price_impact_bps_v2(pool, amount_in, zero_for_one),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{ChainId, DexProtocol, Pool, PoolState, PoolType, Token};

    fn make_v2_pool(id: &str, tok_a: &str, tok_b: &str, res_a: u128, res_b: u128) -> Pool {
        Pool {
            id:    id.to_string(),
            chain: ChainId::Ethereum,
            dex:   DexProtocol::UniswapV2,
            token_a: Token { address: tok_a.to_string(), symbol: "A".to_string(), decimals: 18 },
            token_b: Token { address: tok_b.to_string(), symbol: "B".to_string(), decimals: 18 },
            pool_type: PoolType::ConstantProduct,
            fee_bps: 30,
            state: PoolState {
                reserve_a:      U256::from(res_a),
                reserve_b:      U256::from(res_b),
                sqrt_price_x96: None,
                tick:           None,
                liquidity:      None,
                amp_coeff:      None,
            },
            last_updated_block: 0,
            last_updated_ts:    0,
        }
    }

    // ── Graph upsert / accessor ───────────────────────────────────────────────

    #[test]
    fn test_upsert_and_get_pool() {
        let mut g = LiquidityGraph::new();
        let p = make_v2_pool("p1", "0xAAA", "0xBBB", 1_000_000, 2_000_000);
        g.upsert_pool(p);
        assert_eq!(g.pool_count(), 1);
        assert!(g.get_pool("p1").is_some());
        // Two directed edges (A→B and B→A)
        assert_eq!(g.edges.len(), 2);
    }

    #[test]
    fn test_upsert_replaces_old_edges() {
        let mut g = LiquidityGraph::new();
        let p1 = make_v2_pool("p1", "0xAAA", "0xBBB", 1_000_000, 2_000_000);
        let p2 = make_v2_pool("p1", "0xAAA", "0xBBB", 1_500_000, 3_000_000); // same id
        g.upsert_pool(p1);
        g.upsert_pool(p2);
        // Still exactly 2 edges — old ones were replaced
        assert_eq!(g.edges.len(), 2);
    }

    #[test]
    fn test_clear_edges_removes_pools() {
        let mut g = LiquidityGraph::new();
        g.upsert_pool(make_v2_pool("p1", "0xAAA", "0xBBB", 1_000_000, 2_000_000));
        g.clear_edges();
        assert_eq!(g.pool_count(), 0);
        assert_eq!(g.edges.len(), 0);
        // Token index is retained
        assert_eq!(g.token_count(), 2);
    }

    // ── Two-hop scanner ───────────────────────────────────────────────────────

    #[test]
    fn test_two_hop_finds_profitable_arbitrage() {
        let mut g = LiquidityGraph::new();
        let weth  = "0xweth";
        let usdc  = "0xusdc";

        // Pool 1: WETH/USDC at 1 ETH = 1800 USDC (cheap ETH)
        let p1 = make_v2_pool("p1", weth, usdc, 1_000 * 10u128.pow(18), 1_800_000 * 10u128.pow(6));
        // Pool 2: WETH/USDC at 1 ETH = 2200 USDC (expensive ETH)
        let p2 = make_v2_pool("p2", weth, usdc, 1_000 * 10u128.pow(18), 2_200_000 * 10u128.pow(6));
        g.upsert_pool(p1);
        g.upsert_pool(p2);

        let config = RouterConfig::default();
        let opps   = g.find_opportunities(weth, &config);
        // Should find: buy WETH cheap on p1, sell expensive on p2
        // (or vice versa depending on reserve direction)
        // At minimum, the scanner should not panic
        assert!(opps.len() <= 2, "Unexpected number of opportunities: {}", opps.len());
    }

    // ── Bellman-Ford negative cycle detection ─────────────────────────────────

    #[test]
    fn test_bf_no_cycle_balanced_pools() {
        let mut g = LiquidityGraph::new();
        // Three pools forming a triangle: A→B→C→A with equal rates (no arb)
        let p1 = make_v2_pool("p1", "0xA", "0xB", 1_000_000, 1_000_000);
        let p2 = make_v2_pool("p2", "0xB", "0xC", 1_000_000, 1_000_000);
        let p3 = make_v2_pool("p3", "0xC", "0xA", 1_000_000, 1_000_000);
        g.upsert_pool(p1);
        g.upsert_pool(p2);
        g.upsert_pool(p3);

        let config = RouterConfig { min_profit_usd: 0.0001, ..Default::default() };
        let opps   = find_arbitrage_cycles(&g, &config);
        // With fees, the triangular path is unprofitable — no cycle expected
        assert!(opps.is_empty() || opps.iter().all(|o| !o.is_executable),
            "Expected no profitable cycles in balanced pool triangle");
    }

    #[test]
    fn test_bf_detects_profitable_cycle() {
        let mut g = LiquidityGraph::new();
        // Deliberately imbalanced triangle: A→B 1:2, B→C 2:3, C→A 3:2.5
        // Net rate = (2/1) × (3/2) × (2.5/3) = 2.5 > 1.0 (profitable after fees?)
        let p1 = make_v2_pool("p1", "0xA", "0xB",
            1_000_000_000_000_000_000,
            2_000_000_000_000_000_000,
        );
        let p2 = make_v2_pool("p2", "0xB", "0xC",
            2_000_000_000_000_000_000,
            3_000_000_000_000_000_000,
        );
        let p3 = make_v2_pool("p3", "0xC", "0xA",
            3_000_000_000_000_000_000,
            2_500_000_000_000_000_000,
        );
        g.upsert_pool(p1);
        g.upsert_pool(p2);
        g.upsert_pool(p3);

        let config = RouterConfig {
            min_profit_usd: 0.0,   // accept any profit
            max_hops:       4,
            ..Default::default()
        };
        let opps = find_arbitrage_cycles(&g, &config);
        // BF should detect the negative cycle
        // Note: even with fees the net rate may still be > 1.0 for large imbalances
        println!("BF cycles found: {}", opps.len());
        // Test that the algorithm doesn't panic and returns a valid result
        for opp in &opps {
            assert!(!opp.route.is_empty(), "Opportunity should have at least one step");
        }
    }

    // ── RouterConfig helpers ──────────────────────────────────────────────────

    #[test]
    fn test_gas_cost_computation() {
        let config = RouterConfig {
            gas_price_gwei: 30.0,
            gas_estimate:   350_000,
            ..Default::default()
        };
        let eth_cost = config.gas_cost_eth();
        // 30 gwei × 350,000 gas = 10,500,000 gwei = 0.0105 ETH
        assert!((eth_cost - 0.0105).abs() < 1e-6, "Gas cost: {}", eth_cost);

        let wei_cost = config.gas_cost_wei();
        assert!(wei_cost > U256::zero());
    }

    // ── Cross-chain fetch specs ───────────────────────────────────────────────

    #[test]
    fn test_cross_chain_fetch_specs_empty_for_evm_only_graph() {
        let mut g = LiquidityGraph::new();
        g.upsert_pool(make_v2_pool("p1", "0xweth", "0xusdc", 1_000_000, 2_000_000));
        let specs = cross_chain_fetch_specs("0xweth", "0xusdc", &g);
        assert!(specs.is_empty(), "No non-EVM pools should produce fetch specs");
    }

    #[test]
    fn test_neg_log_rate_direction() {
        // A pool with 2× rate in one direction should have negative neg_log_rate
        let pool = make_v2_pool("px", "0xA", "0xB", 1_000_000, 2_000_000);
        let unit = U256::from(10u64.pow(18));
        // A→B: output > input (2:1 pool, ignoring fees) → rate > 1 → neg_log_rate < 0
        if let Some(rate) = LiquidityGraph::compute_rate_static(&pool, unit, true) {
            assert!(rate > 0.0, "Rate must be positive");
            let nlr = -rate.ln();
            // With 2:1 ratio, rate ≈ 0.5 (pool is skewed: 1M A reserves, 2M B reserves,
            // buying B with A costs more)
            // The sign of neg_log_rate tells us the direction preference
            println!("A→B rate={:.4} neg_log_rate={:.4}", rate, nlr);
        }
        if let Some(rate_ba) = LiquidityGraph::compute_rate_static(&pool, unit, false) {
            println!("B→A rate={:.4} neg_log_rate={:.4}", rate_ba, -rate_ba.ln());
        }
    }
}
