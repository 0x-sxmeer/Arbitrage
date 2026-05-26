// engine/src/discovery/factory_watcher.rs
//
// Subscribes to PoolCreated events on ALL DEX factory contracts.
// When a new pool appears on-chain, it's injected into the scanner
// immediately with max freshness score — new pools = easiest arb.
//
// WHY NEW POOLS ARE GOLD:
//   - Price not yet synchronized across other DEXs
//   - Bots haven't found it yet
//   - First 2-48 hours = highest arb opportunity window
//   - vol/TVL ratio often 20-50x on launch day

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{info, debug, warn};

use super::mega_scanner::{LivePool, PoolRegistry, ScanChain, now_ms};

// ─── Factory contract addresses ────────────────────────────────────────────────

pub struct FactoryConfig {
    pub address:  &'static str,
    pub chain:    ScanChain,
    pub protocol: &'static str,
    pub event_sig: &'static str,  // keccak256 of event signature
}

pub const ALL_FACTORIES: &[FactoryConfig] = &[
    // ── Base ───────────────────────────────────────────────────────────────────
    FactoryConfig {
        address:   "0x420DD381b31aEf6683db6B902084cB0FFECe40Da",
        chain:     ScanChain::Base,
        protocol:  "aerodrome-v2",
        event_sig: "0x2128d88d14c80cb081c7252b69e803c6407cee6a069aedfaa8c23751e941c84",
    },
    FactoryConfig {
        address:   "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A",
        chain:     ScanChain::Base,
        protocol:  "aerodrome-slipstream",
        event_sig: "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
    },
    FactoryConfig {
        address:   "0x33128a8fC17869897dcE68Ed026d694621f6FDfD",
        chain:     ScanChain::Base,
        protocol:  "uniswap-v3",
        event_sig: "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
    },
    // ── Arbitrum ───────────────────────────────────────────────────────────────
    FactoryConfig {
        address:   "0x1F98431c8aD98523631AE4a59f267346ea31F984",
        chain:     ScanChain::Arbitrum,
        protocol:  "uniswap-v3",
        event_sig: "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
    },
    FactoryConfig {
        address:   "0x6EcCab422D763aC031210895C81787E87B43A652",
        chain:     ScanChain::Arbitrum,
        protocol:  "camelot-v3",
        event_sig: "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
    },
    // ── Optimism ───────────────────────────────────────────────────────────────
    FactoryConfig {
        address:   "0xF1046053aa5682b4F9a81b5481394DA16BE5FF5a",
        chain:     ScanChain::Optimism,
        protocol:  "velodrome-v2",
        event_sig: "0x2128d88d14c80cb081c7252b69e803c6407cee6a069aedfaa8c23751e941c84",
    },
    FactoryConfig {
        address:   "0x1F98431c8aD98523631AE4a59f267346ea31F984",
        chain:     ScanChain::Optimism,
        protocol:  "uniswap-v3",
        event_sig: "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
    },
    // ── BNB Chain ──────────────────────────────────────────────────────────────
    FactoryConfig {
        address:   "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
        chain:     ScanChain::BnbChain,
        protocol:  "pancakeswap-v3",
        event_sig: "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
    },
    // ── Polygon ────────────────────────────────────────────────────────────────
    FactoryConfig {
        address:   "0x1F98431c8aD98523631AE4a59f267346ea31F984",
        chain:     ScanChain::Polygon,
        protocol:  "uniswap-v3",
        event_sig: "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
    },
    // ── Blast ──────────────────────────────────────────────────────────────────
    FactoryConfig {
        address:   "0xb48Db7a30854d74cBf71E52B1bB53EC0e5F65c31",
        chain:     ScanChain::Blast,
        protocol:  "thruster-v3",
        event_sig: "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
    },
];

// ─── Alert struct ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NewPoolAlert {
    pub pool_address: String,
    pub chain:        ScanChain,
    pub protocol:     String,
    pub token0_sym:   String,
    pub token1_sym:   String,
    pub token0_addr:  String,
    pub token1_addr:  String,
    pub fee_bps:      u32,
    pub detected_ms:  u64,
}

// ─── FactoryWatcher ────────────────────────────────────────────────────────────

pub struct FactoryWatcher {
    pool_registry: PoolRegistry,
    alert_log:     Vec<NewPoolAlert>,
}

impl FactoryWatcher {
    pub fn new(pool_registry: PoolRegistry) -> Self {
        Self { pool_registry, alert_log: Vec::new() }
    }

    /// Called by EvmAdapter's log subscription when a PoolCreated event fires.
    /// Wire this into the existing block listener in mempool/listener.rs.
    ///
    /// Example integration in existing listener:
    /// ```rust
    /// let factory_watcher = FactoryWatcher::new(pool_registry.clone());
    /// // Add to the log filter alongside existing filters:
    /// let factory_addrs: Vec<_> = ALL_FACTORIES.iter().map(|f| f.address.parse().unwrap()).collect();
    /// let factory_filter = Filter::new().address(factory_addrs);
    /// // In the log processing loop:
    /// if ALL_FACTORIES.iter().any(|f| f.address.eq_ignore_ascii_case(&log.address)) {
    ///     factory_watcher.on_pool_created_log(&log).await;
    /// }
    /// ```
    pub async fn on_pool_created(
        &mut self,
        pool_address: String,
        chain:        ScanChain,
        protocol:     &str,
        token0_sym:   String,
        token1_sym:   String,
        token0_addr:  String,
        token1_addr:  String,
        fee_bps:      u32,
    ) {
        let now = now_ms();

        info!(
            "🆕 NEW POOL | {}/{} | {} | {} | fee={}bps",
            token0_sym, token1_sym, chain.llama_id(), protocol, fee_bps
        );

        // Inject into pool registry immediately with zero TVL/vol
        // (will be enriched by next DeFiLlama/Gecko scan in 60s)
        let id = format!("{}:{}", chain.llama_id(), pool_address);
        let pool = LivePool {
            id: id.clone(),
            address: pool_address.clone(),
            chain,
            protocol: protocol.to_string(),
            token0_sym: token0_sym.clone(),
            token1_sym: token1_sym.clone(),
            token0_addr: token0_addr.clone(),
            token1_addr: token1_addr.clone(),
            fee_bps,
            tvl_usd:       0.0,   // unknown at creation
            vol_24h_usd:   0.0,
            vol_1h_usd:    0.0,
            tx_count_24h:  0,
            vol_tvl:       0.0,
            first_seen_ms: now,   // freshness = 100 = max score
        };

        self.pool_registry.write().await.insert(id, pool);

        let alert = NewPoolAlert {
            pool_address, chain, protocol: protocol.to_string(),
            token0_sym, token1_sym, token0_addr, token1_addr,
            fee_bps, detected_ms: now,
        };
        self.alert_log.push(alert);

        // Keep last 200 alerts
        if self.alert_log.len() > 200 {
            self.alert_log.remove(0);
        }
    }

    pub fn recent_alerts(&self, last_n: usize) -> &[NewPoolAlert] {
        let start = self.alert_log.len().saturating_sub(last_n);
        &self.alert_log[start..]
    }

    pub fn total_detected(&self) -> usize {
        self.alert_log.len()
    }
}
