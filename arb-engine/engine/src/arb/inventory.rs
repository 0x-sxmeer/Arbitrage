// arb/inventory.rs
//
// ─── Phase 4: Cross-Chain Bridge Arbitrage & Inventory ──────────────────────────
//
// Manages delta-neutral token inventory balances across EVM, SVM, and IBC chains.
// By maintaining pre-funded wallets on both the source and target chains,
// the engine executes both legs of a cross-chain arb concurrently, eliminating
// bridge finality delay risk (avoiding waiting minutes/days for bridge execution).

use std::collections::HashMap;
use crate::pool::U256;
use serde::{Serialize, Deserialize};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBalance {
    pub chain_id: u32,
    pub token_address: String,
    pub symbol: String,
    pub decimals: u8,
    pub balance: U256,
    pub value_usd: f64,
}

pub struct InventoryManager {
    // Key format: "chain_id:token_address"
    pub balances: HashMap<String, TokenBalance>,
    pub rebalance_threshold_usd: f64,
}

impl InventoryManager {
    pub fn new(rebalance_threshold_usd: f64) -> Self {
        let mut balances = HashMap::new();

        // Let's seed some mock pre-funded operational balances for Phase 4 simulation
        // Ethereum USDC
        balances.insert(
            "1:0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            TokenBalance {
                chain_id: 1,
                token_address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
                symbol: "USDC".to_string(),
                decimals: 6,
                balance: U256::from(10_000_000_000u64), // 10,000 USDC
                value_usd: 10_000.00,
            },
        );

        // Base USDC
        balances.insert(
            "8453:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
            TokenBalance {
                chain_id: 8453,
                token_address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
                symbol: "USDC".to_string(),
                decimals: 6,
                balance: U256::from(12_500_000_000u64), // 12,500 USDC
                value_usd: 12_500.00,
            },
        );

        // Base WETH
        balances.insert(
            "8453:0x4200000000000000000000000000000000000006".to_string(),
            TokenBalance {
                chain_id: 8453,
                token_address: "0x4200000000000000000000000000000000000006".to_string(),
                symbol: "WETH".to_string(),
                decimals: 18,
                balance: U256::from(4_000_000_000_000_000_000u128), // 4.0 WETH
                value_usd: 14_000.00, // WETH = $3,500
            },
        );

        Self {
            balances,
            rebalance_threshold_usd,
        }
    }

    /// Update on-chain balance with real-time balance data from chain listeners
    pub fn update_balance(&mut self, chain_id: u32, token_addr: &str, new_balance: U256, price_usd: f64) {
        let key = format!("{}:{}", chain_id, token_addr);
        if let Some(tb) = self.balances.get_mut(&key) {
            tb.balance = new_balance;
            // Value calculations
            let divisor = 10f64.powi(tb.decimals as i32);
            let bal_f64 = new_balance.low_u128() as f64 / divisor;
            tb.value_usd = bal_f64 * price_usd;
        }
    }

    /// Checks if a rebalancing route is needed to restore delta-neutral balance levels.
    /// E.g., if one chain's USDC balance falls below threshold, we trigger a bridge rebalance.
    pub fn check_rebalance_trigger(&self, chain_id: u32, token_addr: &str) -> Option<(String, f64)> {
        let key = format!("{}:{}", chain_id, token_addr);
        if let Some(tb) = self.balances.get(&key) {
            if tb.value_usd < self.rebalance_threshold_usd {
                let deficit = self.rebalance_threshold_usd * 2.0 - tb.value_usd; // Target a comfortable buffer
                info!(
                    "⚠ Balance alert: Deficit of ${:.2} USDC detected on chain {}. Rebalancing triggered.",
                    deficit, chain_id
                );
                return Some((tb.symbol.clone(), deficit));
            }
        }
        None
    }

    /// Returns the total valuation of the multi-chain pre-funded portfolio
    pub fn get_total_valuation_usd(&self) -> f64 {
        self.balances.values().map(|b| b.value_usd).sum()
    }
}
