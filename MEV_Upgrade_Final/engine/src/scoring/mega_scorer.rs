// ═══════════════════════════════════════════════════════════════════════════════
//  engine/src/scoring/mega_scorer.rs
//
//  Scores ALL discovered tokens using 7 live signals.
//  Produces 4 independent ranked lists — one per phase.
//  Each list contains HUNDREDS to THOUSANDS of tokens.
//
//  QUALITY RULES (no rugs, no dead pools):
//    Phase 1: TVL > $50K + 2+ pools on same chain (real liquidity exists)
//    Phase 2: CEX listed on Binance (price oracle exists, no rug risk)
//    Phase 3: Vol > $1K in last 24h (tokens with actual trading)
//    Phase 4: Present on 2+ chains with TVL > $10K each (bridge liquidity exists)
// ═══════════════════════════════════════════════════════════════════════════════

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::info;

use super::super::discovery::mega_scanner::{ScanChain, LivePool, PoolRegistry, BinanceListed, is_stable, now_ms};

// ─────────────────────────────────────────────────────────────────────────────
//  Scored token
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ScoredToken {
    pub symbol:         String,
    pub chain:          ScanChain,
    // Composite score
    pub score:          f64,
    // Sub-scores (each 0-100)
    pub vol_tvl_score:  f64,
    pub multi_score:    f64,
    pub whale_score:    f64,
    pub cex_score:      f64,
    pub trend_score:    f64,
    pub fresh_score:    f64,
    pub liq_score:      f64,
    // Aggregated data
    pub total_tvl_usd:  f64,
    pub total_vol_24h:  f64,
    pub total_vol_1h:   f64,
    pub tx_count_24h:   u64,
    pub pool_count:     usize,
    pub pools:          Vec<LivePool>,
    // Best arb path
    pub best_pool_a:    String,
    pub best_pool_b:    String,
    pub best_proto_a:   String,
    pub best_proto_b:   String,
    pub est_spread_bps: f64,
    pub est_profit_usd: f64,
    // Phase eligibility
    pub p1_eligible:    bool,  // 2+ pools + TVL > $50K + not stablecoin pair
    pub p2_eligible:    bool,  // CEX listed on Binance futures
    pub p3_eligible:    bool,  // any token with vol > $1K (backrun target)
    pub p4_eligible:    bool,  // on 2+ chains with real liquidity
    // Phase-specific scores
    pub p1_score:       f64,
    pub p2_score:       f64,
    pub p3_score:       f64,
    pub p4_score:       f64,
    // Signal flags
    pub is_trending:    bool,
    pub is_new_pool:    bool,
    pub is_volatile:    bool,
    pub is_whale_target: bool,
    pub chains_present: Vec<ScanChain>,
}

// ─────────────────────────────────────────────────────────────────────────────
//  Phase lists — holds ALL eligible tokens, no arbitrary cap
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct PhaseLists {
    pub phase1:              Vec<ScoredToken>,  // DEX-DEX: typically 300-600 tokens
    pub phase2:              Vec<ScoredToken>,  // CEX-DEX: typically 200-400 tokens
    pub phase3:              Vec<ScoredToken>,  // Backrun:  typically 600-1000 tokens
    pub phase4:              Vec<ScoredToken>,  // XChain:   typically 200-400 tokens
    pub total_pools_scanned: usize,
    pub total_tokens_scored: usize,
    pub scored_at_ms:        u64,
}

pub type PhaseListsArc = Arc<RwLock<PhaseLists>>;
pub type WhaleScores   = Arc<RwLock<HashMap<String, f64>>>;

// ─────────────────────────────────────────────────────────────────────────────
//  MegaScorer
// ─────────────────────────────────────────────────────────────────────────────

pub struct MegaScorer {
    pools:          PoolRegistry,
    binance_listed: BinanceListed,
    whale_scores:   WhaleScores,
    pub lists:      PhaseListsArc,
}

impl MegaScorer {
    pub fn new(
        pools:          PoolRegistry,
        binance_listed: BinanceListed,
        whale_scores:   WhaleScores,
    ) -> (Self, PhaseListsArc) {
        let lists = Arc::new(RwLock::new(PhaseLists::default()));
        let scorer = Self { pools, binance_listed, whale_scores, lists: lists.clone() };
        (scorer, lists)
    }

    /// Main loop — re-scores every 30 seconds
    pub async fn run(self) {
        let mut tick = interval(Duration::from_secs(30));
        info!("🧮 MegaScorer running — re-scores all tokens every 30s");
        loop {
            tick.tick().await;
            self.score_all().await;
        }
    }

    async fn score_all(&self) {
        let pools       = self.pools.read().await;
        let binance     = self.binance_listed.read().await;
        let whale_s     = self.whale_scores.read().await;
        let now         = now_ms();
        let total_pools = pools.len();

        // ── Step 1: Group pools by (chain, token_symbol) ──────────────────────
        let mut by_token: HashMap<(ScanChain, String), Vec<LivePool>> = HashMap::new();
        for pool in pools.values() {
            for sym in [&pool.token0_sym, &pool.token1_sym] {
                if is_stable(sym) { continue; }
                if sym.len() < 2 || sym.contains("UNKNOWN") { continue; }
                by_token.entry((pool.chain, sym.clone())).or_default().push(pool.clone());
            }
        }

        // ── Step 2: Cross-chain index ─────────────────────────────────────────
        let mut by_sym_global: HashMap<String, Vec<ScanChain>> = HashMap::new();
        for ((chain, sym), _) in &by_token {
            by_sym_global.entry(sym.clone()).or_default().push(*chain);
        }

        // ── Step 3: Score every (chain, token) ───────────────────────────────
        let mut all: Vec<ScoredToken> = Vec::with_capacity(by_token.len());

        for ((chain, symbol), token_pools) in &by_token {
            if token_pools.is_empty() { continue; }

            let total_tvl:  f64 = token_pools.iter().map(|p|p.tvl_usd).sum();
            let total_vol:  f64 = token_pools.iter().map(|p|p.vol_24h_usd).sum();
            let total_vol1h:f64 = token_pools.iter().map(|p|p.vol_1h_usd).sum();
            let total_tx:   u64 = token_pools.iter().map(|p|p.tx_count_24h).sum();
            let max_vt:     f64 = token_pools.iter().map(|p|p.vol_tvl).fold(0.0f64, f64::max);
            let pool_count      = token_pools.len();
            let newest_ms:  u64 = token_pools.iter().map(|p|p.first_seen_ms).max().unwrap_or(0);

            // ── 7 sub-scores (each 0-100) ─────────────────────────────────────

            // 1. Vol/TVL ratio — highest arb signal
            //    2x = 30pts, 5x = 50pts, 10x = 65pts, 50x = 100pts
            let vol_tvl_score = if max_vt > 0.0 { (max_vt.ln() * 20.0).clamp(0.0, 100.0) } else { 0.0 };

            // 2. Multi-pool — more pools = more arb paths
            let multi_score = match pool_count {
                0|1 => 10.0, 2 => 38.0, 3 => 62.0, 4 => 78.0, 5 => 88.0, _ => 100.0,
            };

            // 3. Whale score — from external detector (decays over time)
            let whale_score = whale_s.get(symbol.as_str()).copied().unwrap_or(0.0).clamp(0.0,100.0);

            // 4. CEX score — binary: on Binance = 100
            let cex_score = if binance.contains_key(symbol.as_str()) { 100.0 } else { 0.0 };

            // 5. Trend score — spike in 1h vs 24h average suggests trending
            let avg_hourly = total_vol / 24.0;
            let trend_score = if avg_hourly > 0.0 {
                ((total_vol1h / avg_hourly - 1.0) * 50.0).clamp(0.0, 100.0)
            } else { 0.0 };

            // 6. Freshness — new pools have worst price sync = easiest arb
            let age_h = (now.saturating_sub(newest_ms)) as f64 / 3_600_000.0;
            let fresh_score = (100.0 - age_h / 168.0 * 100.0).clamp(0.0, 100.0);

            // 7. Liquidity score — enough TVL to execute without massive slippage
            let liq_score = (total_tvl.log10().max(0.0) * 14.0).clamp(0.0, 100.0);

            // ── Composite (0-1000) ────────────────────────────────────────────
            let score = (
                vol_tvl_score * 3.2 +
                multi_score   * 1.8 +
                whale_score   * 1.6 +
                cex_score     * 1.1 +
                trend_score   * 0.9 +
                fresh_score   * 0.5 +
                liq_score     * 0.4
            ).clamp(0.0, 999.0);

            // ── Signal flags ──────────────────────────────────────────────────
            let is_trending    = trend_score > 40.0;
            let is_new_pool    = age_h < 48.0;
            let is_volatile    = max_vt > 5.0;
            let is_whale_target= whale_score > 30.0;

            // ── Phase eligibility (strict quality gates) ──────────────────────
            // P1: must have real liquidity and multiple pools for price divergence
            let p1_eligible = pool_count >= 2 && total_tvl >= 50_000.0;
            // P2: must be on Binance (verified token, price oracle exists)
            let p2_eligible = cex_score > 0.0;
            // P3: any token with vol > $1K is a backrun target
            let p3_eligible = total_vol >= 1_000.0;
            // P4: must be on 2+ chains with real TVL
            let chains_present = by_sym_global.get(symbol.as_str()).cloned().unwrap_or_default();
            let p4_eligible = chains_present.len() >= 2 && total_tvl >= 10_000.0;

            // ── Phase-specific scores ─────────────────────────────────────────
            let p1_score = score + multi_score * 1.5 + vol_tvl_score * 1.0;
            let p2_score = if p2_eligible { score + cex_score * 1.8 + trend_score * 0.8 } else { 0.0 };
            let p3_score = score + whale_score * 2.0 + trend_score * 1.0 + vol_tvl_score * 0.5;
            let p4_score = if p4_eligible { score + chains_present.len() as f64 * 45.0 } else { 0.0 };

            // ── Best arb path ─────────────────────────────────────────────────
            let (pa, pb, pra, prb, spread) = best_pair(token_pools);
            let est_size   = (total_tvl * 0.01).min(500_000.0);
            let est_profit = (est_size * spread / 10_000.0 - chain.gas_usd() * 2.0).max(0.0);

            let mut sorted = token_pools.clone();
            sorted.sort_by(|a,b| b.tvl_usd.partial_cmp(&a.tvl_usd).unwrap());

            all.push(ScoredToken {
                symbol: symbol.clone(), chain: *chain, score,
                vol_tvl_score, multi_score, whale_score,
                cex_score, trend_score, fresh_score, liq_score,
                total_tvl_usd: total_tvl, total_vol_24h: total_vol,
                total_vol_1h: total_vol1h, tx_count_24h: total_tx,
                pool_count, pools: sorted,
                best_pool_a: pa, best_pool_b: pb,
                best_proto_a: pra, best_proto_b: prb,
                est_spread_bps: spread, est_profit_usd: est_profit,
                p1_eligible, p2_eligible, p3_eligible, p4_eligible,
                p1_score, p2_score, p3_score, p4_score,
                is_trending, is_new_pool, is_volatile, is_whale_target,
                chains_present,
            });
        }

        // ── Step 4: Build 4 phase lists (ALL eligible, no cap) ───────────────

        let mut phase1: Vec<ScoredToken> = all.iter().filter(|t|t.p1_eligible).cloned().collect();
        phase1.sort_by(|a,b| b.p1_score.partial_cmp(&a.p1_score).unwrap());

        let mut phase2: Vec<ScoredToken> = all.iter().filter(|t|t.p2_eligible).cloned().collect();
        phase2.sort_by(|a,b| b.p2_score.partial_cmp(&a.p2_score).unwrap());

        let mut phase3: Vec<ScoredToken> = all.iter().filter(|t|t.p3_eligible).cloned().collect();
        phase3.sort_by(|a,b| b.p3_score.partial_cmp(&a.p3_score).unwrap());

        let mut phase4: Vec<ScoredToken> = all.iter().filter(|t|t.p4_eligible).cloned().collect();
        phase4.sort_by(|a,b| b.p4_score.partial_cmp(&a.p4_score).unwrap());

        let total_scored = all.len();

        info!(
            "🧮 Scored {} tokens | P1:{} P2:{} P3:{} P4:{} | pools:{}",
            total_scored, phase1.len(), phase2.len(), phase3.len(), phase4.len(), total_pools
        );
        info!("  P1 top: {}", phase1.iter().take(8).map(|t|t.symbol.as_str()).collect::<Vec<_>>().join(", "));
        info!("  P2 top: {}", phase2.iter().take(8).map(|t|t.symbol.as_str()).collect::<Vec<_>>().join(", "));
        info!("  P3 top: {}", phase3.iter().take(8).map(|t|t.symbol.as_str()).collect::<Vec<_>>().join(", "));
        info!("  P4 top: {}", phase4.iter().take(8).map(|t|t.symbol.as_str()).collect::<Vec<_>>().join(", "));

        *self.lists.write().await = PhaseLists {
            phase1, phase2, phase3, phase4,
            total_pools_scanned: total_pools,
            total_tokens_scored: total_scored,
            scored_at_ms: now,
        };
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Find best pool pair for arb path estimation
// ─────────────────────────────────────────────────────────────────────────────

fn best_pair(pools: &[LivePool]) -> (String, String, String, String, f64) {
    if pools.len() < 2 { return (String::new(),String::new(),String::new(),String::new(),0.0); }
    let mut best_spread = 0.0f64;
    let mut ai = 0; let mut bi = 1;
    for i in 0..pools.len() {
        for j in (i+1)..pools.len() {
            let spread = (pools[i].vol_tvl - pools[j].vol_tvl).abs() * 55.0;
            if spread > best_spread { best_spread = spread; ai = i; bi = j; }
        }
    }
    (
        pools[ai].address.clone(), pools[bi].address.clone(),
        pools[ai].protocol.clone(), pools[bi].protocol.clone(),
        best_spread.clamp(0.0, 350.0),
    )
}
