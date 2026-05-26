// ─────────────────────────────────────────────────────────────────────────────
//  cross_chain/inventory_manager.rs — Multi-Chain Inventory Tracking
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use anyhow::Result;
use tracing::{info, warn};
use super::cross_chain_engine::ChainId;

#[derive(Debug, Clone)]
pub struct ChainInventory {
    pub chain:          ChainId,
    /// Token balances: token_address → balance in raw units
    pub balances:       HashMap<String, u128>,
    /// USD value of each token
    pub values_usd:     HashMap<String, f64>,
    pub total_usd:      f64,
    pub last_updated_ms: u64,
}

pub struct InventoryManager {
    /// Inventory per chain
    inventories:     HashMap<ChainId, ChainInventory>,
    /// Minimum USDC to keep on each chain for execution
    min_usdc_reserve: f64,
    /// Minimum ETH to keep on each chain for gas
    min_eth_reserve:  f64,
}

impl InventoryManager {
    pub fn new(min_usdc_reserve: f64, min_eth_reserve: f64) -> Self {
        let mut inventories = HashMap::new();
        for chain in [ChainId::Base, ChainId::Optimism, ChainId::Arbitrum] {
            inventories.insert(chain, ChainInventory {
                chain,
                balances:        HashMap::new(),
                values_usd:      HashMap::new(),
                total_usd:       0.0,
                last_updated_ms: 0,
            });
        }
        Self { inventories, min_usdc_reserve, min_eth_reserve }
    }

    /// Check if we can execute a cross-chain trade
    pub fn can_execute_trade(
        &self,
        buy_chain:     ChainId,
        sell_chain:    ChainId,
        token_address: &str,
        size_usd:      f64,
    ) -> bool {
        // Need USDC on buy_chain to buy
        let buy_inv = self.inventories.get(&buy_chain).unwrap();
        let usdc_available = buy_inv.values_usd
            .get(buy_chain.usdc_address())
            .copied()
            .unwrap_or(0.0);
        if usdc_available < size_usd + self.min_usdc_reserve { return false; }

        // Need token on sell_chain to sell
        let sell_inv = self.inventories.get(&sell_chain).unwrap();
        let token_available = sell_inv.values_usd.get(token_address).copied().unwrap_or(0.0);
        if token_available < size_usd { return false; }

        true
    }

    /// Update balance after a trade
    pub fn update_after_trade(
        &mut self,
        chain:         ChainId,
        token_in:      &str,
        amount_in_usd: f64,
        token_out:     &str,
        amount_out_usd: f64,
    ) {
        if let Some(inv) = self.inventories.get_mut(&chain) {
            let bal_in = inv.values_usd.entry(token_in.to_string()).or_insert(0.0);
            *bal_in -= amount_in_usd;
            let bal_out = inv.values_usd.entry(token_out.to_string()).or_insert(0.0);
            *bal_out += amount_out_usd;
            inv.total_usd = inv.values_usd.values().sum();
        }
    }

    /// Identify chains needing rebalancing
    pub fn chains_needing_rebalance(&self) -> Vec<(ChainId, ChainId, f64)> {
        let mut transfers = Vec::new();

        for chain in [ChainId::Base, ChainId::Optimism, ChainId::Arbitrum] {
            let inv = &self.inventories[&chain];
            let usdc_bal = inv.values_usd.get(chain.usdc_address()).copied().unwrap_or(0.0);

            if usdc_bal < self.min_usdc_reserve {
                let deficit = self.min_usdc_reserve - usdc_bal;
                // Find chain with excess USDC
                for source in [ChainId::Base, ChainId::Optimism, ChainId::Arbitrum] {
                    if source == chain { continue; }
                    let src_inv = &self.inventories[&source];
                    let src_usdc = src_inv.values_usd.get(source.usdc_address()).copied().unwrap_or(0.0);
                    if src_usdc > self.min_usdc_reserve * 2.0 {
                        let surplus = src_usdc - self.min_usdc_reserve * 1.5;
                        let bridge_amount = deficit.min(surplus);
                        transfers.push((source, chain, bridge_amount));
                        warn!(
                            "⚠️  Rebalance needed: ${:.0} USDC {} → {}",
                            bridge_amount, source.name(), chain.name()
                        );
                        break;
                    }
                }
            }
        }
        transfers
    }

    pub fn total_tvl_usd(&self) -> f64 {
        self.inventories.values().map(|inv| inv.total_usd).sum()
    }
}
