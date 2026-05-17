// ─────────────────────────────────────────────────────────────────────────────
//  metrics.rs — Engine-wide runtime metrics (lock-free atomics)
//
//  All counters use AtomicU64 for concurrent access without locks.
//  Read via `EngineMetrics::snapshot()` for dashboard logging.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};

/// Lock-free runtime metrics for the arbitrage engine.
pub struct EngineMetrics {
    // ── Mempool ───────────────────────────────────────────────────────────────
    /// Total pending transactions seen from the mempool WebSocket
    pub txs_seen:              AtomicU64,
    /// Transactions that passed the router address filter
    pub txs_filtered:          AtomicU64,
    /// Transactions successfully decoded (calldata parsed)
    pub txs_decoded:           AtomicU64,

    // ── Cache ─────────────────────────────────────────────────────────────────
    /// Redis pool state cache hits
    pub cache_hits:            AtomicU64,
    /// Redis pool state cache misses
    pub cache_misses:          AtomicU64,

    // ── Router ────────────────────────────────────────────────────────────────
    /// Number of Bellman-Ford scans executed
    pub router_scans:          AtomicU64,
    /// Total arbitrage opportunities detected (including non-executable)
    pub opportunities_found:   AtomicU64,
    /// Opportunities that passed NEV threshold (is_executable = true)
    pub opportunities_executable: AtomicU64,
    /// Opportunities persisted to PostgreSQL
    pub opportunities_persisted:  AtomicU64,

    // ── Graph ─────────────────────────────────────────────────────────────────
    /// Current number of pools in the liquidity graph
    pub graph_pools:           AtomicU64,
    /// Current number of tokens (nodes) in the liquidity graph
    pub graph_tokens:          AtomicU64,

    // ── Errors ────────────────────────────────────────────────────────────────
    /// WebSocket reconnection count
    pub ws_reconnections:      AtomicU64,
    /// Redis errors
    pub redis_errors:          AtomicU64,
    /// Postgres errors
    pub pg_errors:             AtomicU64,
    /// Transactions dropped due to channel capacity
    pub txs_dropped:           AtomicU64,

    // ── Live Data for API ─────────────────────────────────────────────────────
    pub recent_mempool_txs:    tokio::sync::RwLock<std::collections::VecDeque<serde_json::Value>>,
}

impl EngineMetrics {
    pub fn new() -> Self {
        Self {
            txs_seen:              AtomicU64::new(0),
            txs_filtered:          AtomicU64::new(0),
            txs_decoded:           AtomicU64::new(0),
            cache_hits:            AtomicU64::new(0),
            cache_misses:          AtomicU64::new(0),
            router_scans:          AtomicU64::new(0),
            opportunities_found:   AtomicU64::new(0),
            opportunities_executable: AtomicU64::new(0),
            opportunities_persisted:  AtomicU64::new(0),
            graph_pools:           AtomicU64::new(0),
            graph_tokens:          AtomicU64::new(0),
            ws_reconnections:      AtomicU64::new(0),
            redis_errors:          AtomicU64::new(0),
            pg_errors:             AtomicU64::new(0),
            txs_dropped:           AtomicU64::new(0),
            recent_mempool_txs:    tokio::sync::RwLock::new(std::collections::VecDeque::new()),
        }
    }

    // ── Increment helpers ─────────────────────────────────────────────────────

    pub fn inc_txs_seen(&self)              { self.txs_seen.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_txs_filtered(&self)          { self.txs_filtered.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_txs_decoded(&self)           { self.txs_decoded.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_cache_hits(&self)            { self.cache_hits.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_cache_misses(&self)          { self.cache_misses.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_router_scans(&self)          { self.router_scans.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_opportunities_found(&self)   { self.opportunities_found.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_opportunities_executable(&self) { self.opportunities_executable.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_opportunities_persisted(&self)  { self.opportunities_persisted.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_ws_reconnections(&self)      { self.ws_reconnections.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_redis_errors(&self)          { self.redis_errors.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_pg_errors(&self)             { self.pg_errors.fetch_add(1, Ordering::Relaxed); }
    pub fn inc_txs_dropped(&self)           { self.txs_dropped.fetch_add(1, Ordering::Relaxed); }

    pub fn set_graph_pools(&self, n: u64)  { self.graph_pools.store(n, Ordering::Relaxed); }
    pub fn set_graph_tokens(&self, n: u64) { self.graph_tokens.store(n, Ordering::Relaxed); }

    /// Create a snapshot for logging.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            txs_seen:              self.txs_seen.load(Ordering::Relaxed),
            txs_filtered:          self.txs_filtered.load(Ordering::Relaxed),
            txs_decoded:           self.txs_decoded.load(Ordering::Relaxed),
            cache_hits:            self.cache_hits.load(Ordering::Relaxed),
            cache_misses:          self.cache_misses.load(Ordering::Relaxed),
            router_scans:          self.router_scans.load(Ordering::Relaxed),
            opportunities_found:   self.opportunities_found.load(Ordering::Relaxed),
            opportunities_executable: self.opportunities_executable.load(Ordering::Relaxed),
            opportunities_persisted:  self.opportunities_persisted.load(Ordering::Relaxed),
            graph_pools:           self.graph_pools.load(Ordering::Relaxed),
            graph_tokens:          self.graph_tokens.load(Ordering::Relaxed),
            ws_reconnections:      self.ws_reconnections.load(Ordering::Relaxed),
            redis_errors:          self.redis_errors.load(Ordering::Relaxed),
            pg_errors:             self.pg_errors.load(Ordering::Relaxed),
            txs_dropped:           self.txs_dropped.load(Ordering::Relaxed),
        }
    }

    /// Log a summary line (called periodically by the listener).
    pub fn log_summary(&self) {
        let s = self.snapshot();
        tracing::info!(
            txs_seen     = s.txs_seen,
            decoded      = s.txs_decoded,
            cache_hit    = s.cache_hits,
            cache_miss   = s.cache_misses,
            scans        = s.router_scans,
            opps_found   = s.opportunities_found,
            opps_exec    = s.opportunities_executable,
            graph_pools  = s.graph_pools,
            graph_tokens = s.graph_tokens,
            ws_reconn    = s.ws_reconnections,
            txs_dropped  = s.txs_dropped,
            "📊 Engine metrics"
        );
    }
}

impl Default for EngineMetrics {
    fn default() -> Self { Self::new() }
}

/// Copyable snapshot of all metrics at a point in time.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct MetricsSnapshot {
    pub txs_seen:              u64,
    pub txs_filtered:          u64,
    pub txs_decoded:           u64,
    pub cache_hits:            u64,
    pub cache_misses:          u64,
    pub router_scans:          u64,
    pub opportunities_found:   u64,
    pub opportunities_executable: u64,
    pub opportunities_persisted:  u64,
    pub graph_pools:           u64,
    pub graph_tokens:          u64,
    pub ws_reconnections:      u64,
    pub redis_errors:          u64,
    pub pg_errors:             u64,
    pub txs_dropped:           u64,
}

impl MetricsSnapshot {
    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 { return 0.0; }
        self.cache_hits as f64 / total as f64 * 100.0
    }
}
