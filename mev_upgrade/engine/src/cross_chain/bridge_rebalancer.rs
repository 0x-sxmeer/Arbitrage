// ─────────────────────────────────────────────────────────────────────────────
//  cross_chain/bridge_rebalancer.rs — Async Inventory Rebalancing
//
//  Moves USDC/ETH between chains when inventory is depleted.
//  Uses native L2 bridges (free, but slow ~7 days for ETH mainnet)
//  or LayerZero (fast, small fee) for USDC transfers.
//
//  Called ASYNCHRONOUSLY — never blocks the hot arb execution path.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use anyhow::Result;
use tracing::{info, warn};
use super::cross_chain_engine::ChainId;

#[derive(Debug, Clone)]
pub enum BridgeProvider {
    /// Native L2 bridge — free but slow (minutes to hours)
    Native,
    /// LayerZero — fast (~30s), small fee (~$0.50)
    LayerZero,
    /// Stargate (LayerZero-based USDC bridge) — fast, minimal slippage
    Stargate,
}

impl BridgeProvider {
    pub fn typical_time_secs(&self) -> u64 {
        match self {
            BridgeProvider::Native    => 1800,   // 30 min average
            BridgeProvider::LayerZero => 45,
            BridgeProvider::Stargate  => 60,
        }
    }
    pub fn fee_bps(&self, amount_usd: f64) -> f64 {
        match self {
            BridgeProvider::Native    => 0.0,
            BridgeProvider::LayerZero => 0.50 / amount_usd * 10_000.0,  // flat $0.50
            BridgeProvider::Stargate  => 6.0,   // 6bps
        }
    }
}

#[derive(Debug, Clone)]
pub struct BridgeTransfer {
    pub id:            String,
    pub from_chain:    ChainId,
    pub to_chain:      ChainId,
    pub token:         String,
    pub amount_usd:    f64,
    pub provider:      BridgeProvider,
    pub submitted_at:  u64,
    pub expected_at:   u64,
    pub status:        BridgeStatus,
}

#[derive(Debug, Clone)]
pub enum BridgeStatus { Pending, InFlight, Completed, Failed }

pub struct BridgeRebalancer {
    in_flight: Vec<BridgeTransfer>,
    /// Known Stargate pool addresses per chain
    stargate_pools: HashMap<ChainId, String>,
    /// LayerZero endpoint addresses per chain  
    lz_endpoints:   HashMap<ChainId, String>,
}

impl BridgeRebalancer {
    pub fn new() -> Self {
        let mut stargate_pools = HashMap::new();
        stargate_pools.insert(ChainId::Base,     "0x27a16dc786820B16E5c9028b75B99F6f604b5d26".to_string());
        stargate_pools.insert(ChainId::Optimism, "0xDecC0c09c3B5f6e92EF4184125D5648a66E35298".to_string());
        stargate_pools.insert(ChainId::Arbitrum, "0x892785f33CdeE22A30AEF750F285E18c18040c3e".to_string());

        let mut lz_endpoints = HashMap::new();
        lz_endpoints.insert(ChainId::Base,     "0xb6319cC6c8c27A8F5dAF0dD3DF91EA35C4720dd7".to_string());
        lz_endpoints.insert(ChainId::Optimism, "0x3c2269811836af69497E5F486A85D7316753cf62".to_string());
        lz_endpoints.insert(ChainId::Arbitrum, "0x3c2269811836af69497E5F486A85D7316753cf62".to_string());

        Self { in_flight: Vec::new(), stargate_pools, lz_endpoints }
    }

    /// Initiate a rebalancing bridge transfer
    pub async fn bridge(
        &mut self,
        from_chain:  ChainId,
        to_chain:    ChainId,
        token_addr:  &str,
        amount_usd:  f64,
    ) -> Result<String> {
        // Choose best provider based on amount and chains
        let provider = self.select_provider(from_chain, to_chain, amount_usd);
        let fee       = provider.fee_bps(amount_usd) / 10_000.0 * amount_usd;
        let now_ms    = now_ms();

        info!(
            "🌉 Bridge: ${:.0} {} → {} via {:?} (fee=${:.2}, eta={}s)",
            amount_usd, from_chain.name(), to_chain.name(),
            provider, fee, provider.typical_time_secs()
        );

        let transfer_id = format!("bridge_{}", now_ms);
        
        self.in_flight.push(BridgeTransfer {
            id:           transfer_id.clone(),
            from_chain,
            to_chain,
            token:        token_addr.to_string(),
            amount_usd,
            expected_at:  now_ms + provider.typical_time_secs() * 1000,
            provider,
            submitted_at: now_ms,
            status:       BridgeStatus::InFlight,
        });

        // In production: call the actual bridge contract here
        Ok(transfer_id)
    }

    fn select_provider(&self, from: ChainId, to: ChainId, amount_usd: f64) -> BridgeProvider {
        // For small amounts (<$1000), native is fine (slower but free)
        // For large amounts, use Stargate (cheap fixed fee, fast)
        if amount_usd < 1_000.0 {
            BridgeProvider::Native
        } else if amount_usd < 50_000.0 {
            BridgeProvider::Stargate
        } else {
            BridgeProvider::Stargate  // LayerZero has size limits
        }
    }

    pub fn pending_count(&self) -> usize {
        self.in_flight.iter().filter(|t| matches!(t.status, BridgeStatus::InFlight)).count()
    }

    pub fn pending_volume_usd(&self) -> f64 {
        self.in_flight.iter()
            .filter(|t| matches!(t.status, BridgeStatus::InFlight))
            .map(|t| t.amount_usd)
            .sum()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
