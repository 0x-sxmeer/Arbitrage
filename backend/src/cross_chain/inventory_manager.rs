// engine/src/cross_chain/inventory_manager.rs
//
// Tracks USDC + ETH inventory on each chain.
// Triggers Stargate/LayerZero rebalancing when a chain runs low.
// Never blocks the hot arb path — rebalancing is async.

use std::collections::HashMap;
use tracing::{info, warn};

use super::super::discovery::mega_scanner::ScanChain;

#[derive(Debug, Clone)]
pub struct ChainInventory {
    pub chain:          ScanChain,
    pub usdc_balance:   f64,
    pub eth_balance:    f64,
    pub total_usd:      f64,
    pub last_updated_ms: u64,
}

pub struct InventoryManager {
    pub inventories:    HashMap<ScanChain, ChainInventory>,
    pub min_usdc:       f64,
    pub min_eth_usd:    f64,
}

impl InventoryManager {
    pub fn new(min_usdc: f64, min_eth_usd: f64) -> Self {
        let mut inventories = HashMap::new();
        for chain in ScanChain::all() {
            inventories.insert(*chain, ChainInventory {
                chain: *chain,
                usdc_balance: 0.0,
                eth_balance:  0.0,
                total_usd:    0.0,
                last_updated_ms: 0,
            });
        }
        Self { inventories, min_usdc, min_eth_usd }
    }

    pub fn can_execute(&self, buy_chain: ScanChain, sell_chain: ScanChain, size_usd: f64) -> bool {
        let buy_usdc = self.inventories.get(&buy_chain).map(|i|i.usdc_balance).unwrap_or(0.0);
        let sell_eth = self.inventories.get(&sell_chain).map(|i|i.eth_balance*3000.0).unwrap_or(0.0);
        buy_usdc >= size_usd + self.min_usdc && sell_eth >= size_usd
    }

    pub fn needs_rebalance(&self) -> Vec<(ScanChain, ScanChain, f64)> {
        let mut transfers = Vec::new();
        let chains: Vec<ScanChain> = ScanChain::all().to_vec();
        for &chain in &chains {
            let inv = &self.inventories[&chain];
            if inv.usdc_balance < self.min_usdc {
                let deficit = self.min_usdc - inv.usdc_balance;
                // Find source chain with surplus
                for &src in &chains {
                    if src == chain { continue }
                    let src_usdc = self.inventories[&src].usdc_balance;
                    if src_usdc > self.min_usdc * 1.5 {
                        let surplus = src_usdc - self.min_usdc * 1.2;
                        let amount = deficit.min(surplus);
                        warn!("⚠️ Rebalance: ${:.0} USDC {} → {}", amount, src.llama_id(), chain.llama_id());
                        transfers.push((src, chain, amount));
                        break;
                    }
                }
            }
        }
        transfers
    }

    pub fn update_after_trade(
        &mut self, chain: ScanChain,
        spent_usdc: f64, received_token_usd: f64,
    ) {
        if let Some(inv) = self.inventories.get_mut(&chain) {
            inv.usdc_balance -= spent_usdc;
            inv.eth_balance  += received_token_usd / 3000.0;
            inv.total_usd     = inv.usdc_balance + inv.eth_balance * 3000.0;
        }
    }

    pub fn total_tvl(&self) -> f64 {
        self.inventories.values().map(|i|i.total_usd).sum()
    }
}
