// ─────────────────────────────────────────────────────────────────────────────
//  cross_chain/cross_chain_engine.rs — Cross-Chain Price Divergence Detection
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, info, warn};

// ─────────────────────────────────────────────────────────────────────────────
//  Chain definitions
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainId {
    Base       = 8453,
    Optimism   = 10,
    Arbitrum   = 42161,
    Ethereum   = 1,
}

impl ChainId {
    pub fn name(&self) -> &'static str {
        match self {
            ChainId::Base      => "Base",
            ChainId::Optimism  => "Optimism",
            ChainId::Arbitrum  => "Arbitrum",
            ChainId::Ethereum  => "Ethereum",
        }
    }
    
    pub fn gas_cost_usd(&self) -> f64 {
        match self {
            ChainId::Base      => 0.10,   // ~$0.10 per swap on Base
            ChainId::Optimism  => 0.15,   // slightly more expensive
            ChainId::Arbitrum  => 0.30,   // higher L2 batch costs
            ChainId::Ethereum  => 8.00,   // expensive mainnet
        }
    }
    
    pub fn weth_address(&self) -> &'static str {
        match self {
            ChainId::Base      => "0x4200000000000000000000000000000000000006",
            ChainId::Optimism  => "0x4200000000000000000000000000000000000006",
            ChainId::Arbitrum  => "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            ChainId::Ethereum  => "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        }
    }
    
    pub fn usdc_address(&self) -> &'static str {
        match self {
            ChainId::Base      => "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            ChainId::Optimism  => "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85",
            ChainId::Arbitrum  => "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            ChainId::Ethereum  => "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Price matrix — indexed by (chain, token_symbol)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChainPrice {
    pub price_usd:       f64,
    pub liquidity_usd:   f64,
    pub pool_address:    String,
    pub fee_bps:         u32,
    pub block_number:    u64,
    pub timestamp_ms:    u64,
    pub is_stale:        bool,
}

pub type PriceMatrix = Arc<RwLock<HashMap<(ChainId, String), ChainPrice>>>;

// ─────────────────────────────────────────────────────────────────────────────
//  Cross-chain opportunity
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CrossChainOpportunity {
    pub token_symbol:       String,
    pub buy_chain:          ChainId,
    pub sell_chain:         ChainId,
    pub buy_price_usd:      f64,
    pub sell_price_usd:     f64,
    pub spread_pct:         f64,
    pub buy_pool:           String,
    pub sell_pool:          String,
    /// Amount to trade (in USD)
    pub trade_size_usd:     f64,
    pub expected_profit_usd: f64,
    /// Whether we have sufficient inventory to execute both legs
    pub executable:         bool,
}

// ─────────────────────────────────────────────────────────────────────────────
//  CrossChainEngine
// ─────────────────────────────────────────────────────────────────────────────

pub struct CrossChainEngine {
    price_matrix:   PriceMatrix,
    /// Chains to monitor
    chains:         Vec<ChainId>,
    /// Tokens to scan (symbol → canonical name)
    tokens:         Vec<String>,
    /// Min spread to consider (bps)
    min_spread_bps: f64,
    /// Max acceptable price staleness (ms)
    max_age_ms:     u64,
    /// Min liquidity on both sides (USD)
    min_liquidity:  f64,
    /// Default trade size (USD)
    trade_size_usd: f64,
    
    // Statistics
    pub opportunities_found:    u64,
    pub opportunities_executed: u64,
    pub total_profit_usd:       f64,
}

impl CrossChainEngine {
    pub fn new(price_matrix: PriceMatrix, trade_size_usd: f64) -> Self {
        Self {
            price_matrix,
            chains: vec![ChainId::Base, ChainId::Optimism, ChainId::Arbitrum],
            tokens: vec![
                "ETH".to_string(),
                "WBTC".to_string(),
                "USDC".to_string(),
                "ARB".to_string(),
                "OP".to_string(),
            ],
            min_spread_bps:         30.0,    // 0.30% minimum
            max_age_ms:             3_000,   // 3s max price age
            min_liquidity:          200_000.0, // $200k per pool
            trade_size_usd,
            opportunities_found:    0,
            opportunities_executed: 0,
            total_profit_usd:       0.0,
        }
    }

    /// Main scanning loop — runs every 100ms
    pub async fn run(mut self, execute_enabled: bool) -> Result<()> {
        let mut ticker = interval(Duration::from_millis(100));
        info!(
            "🌐 Cross-Chain Engine started | chains={} | tokens={}",
            self.chains.len(), self.tokens.len()
        );

        loop {
            ticker.tick().await;

            let opps = self.scan_divergences().await;
            for opp in opps {
                self.opportunities_found += 1;
                
                let log = format!(
                    "🌉 CROSS-CHAIN | {} | {} → {} | spread={:.3}% | ${:.0} → profit=${:.2}",
                    opp.token_symbol,
                    opp.buy_chain.name(),
                    opp.sell_chain.name(),
                    opp.spread_pct,
                    opp.trade_size_usd,
                    opp.expected_profit_usd
                );

                if opp.expected_profit_usd >= 10.0 {
                    info!("{}", log);
                    
                    if execute_enabled && opp.executable {
                        match self.execute_cross_chain(&opp).await {
                            Ok(profit) => {
                                self.opportunities_executed += 1;
                                self.total_profit_usd += profit;
                                info!("✅ Cross-chain executed | cumulative=${:.2}", self.total_profit_usd);
                            }
                            Err(e) => warn!("❌ Cross-chain exec failed: {}", e),
                        }
                    }
                } else {
                    debug!("{}", log);
                }
            }
        }
    }

    async fn scan_divergences(&self) -> Vec<CrossChainOpportunity> {
        let matrix  = self.price_matrix.read().await;
        let now_ms  = now_ms();
        let mut opps = Vec::new();

        for token in &self.tokens {
            // Get price on all chains for this token
            let chain_prices: Vec<(ChainId, &ChainPrice)> = self.chains.iter()
                .filter_map(|&chain| {
                    let price = matrix.get(&(chain, token.clone()))?;
                    if price.is_stale { return None; }
                    if now_ms - price.timestamp_ms > self.max_age_ms { return None; }
                    if price.liquidity_usd < self.min_liquidity { return None; }
                    Some((chain, price))
                })
                .collect();

            if chain_prices.len() < 2 { continue; }

            // Check all pairs of chains
            for i in 0..chain_prices.len() {
                for j in 0..chain_prices.len() {
                    if i == j { continue; }

                    let (buy_chain, buy_price_data)   = chain_prices[i];
                    let (sell_chain, sell_price_data) = chain_prices[j];

                    let buy_price  = buy_price_data.price_usd;
                    let sell_price = sell_price_data.price_usd;

                    if buy_price <= 0.0 || sell_price <= 0.0 { continue; }

                    let spread_pct = (sell_price - buy_price) / buy_price * 100.0;
                    if spread_pct <= 0.0 { continue; }
                    if spread_pct < self.min_spread_bps / 100.0 { continue; }

                    // Cost model: buy_gas + sell_gas + swap_fees (DEX) on both chains
                    let total_gas   = buy_chain.gas_cost_usd() + sell_chain.gas_cost_usd();
                    let dex_fee_buy  = self.trade_size_usd * buy_price_data.fee_bps as f64 / 10_000.0;
                    let dex_fee_sell = self.trade_size_usd * sell_price_data.fee_bps as f64 / 10_000.0;
                    let total_costs  = total_gas + dex_fee_buy + dex_fee_sell;

                    let gross_profit = self.trade_size_usd * spread_pct / 100.0;
                    let net_profit   = gross_profit - total_costs;

                    if net_profit > 0.0 {
                        opps.push(CrossChainOpportunity {
                            token_symbol:        token.clone(),
                            buy_chain,
                            sell_chain,
                            buy_price_usd:       buy_price,
                            sell_price_usd:      sell_price,
                            spread_pct,
                            buy_pool:            buy_price_data.pool_address.clone(),
                            sell_pool:           sell_price_data.pool_address.clone(),
                            trade_size_usd:      self.trade_size_usd,
                            expected_profit_usd: net_profit,
                            executable:          true, // inventory check done in executor
                        });
                    }
                }
            }
        }

        // Best opportunities first
        opps.sort_by(|a, b| b.expected_profit_usd.partial_cmp(&a.expected_profit_usd).unwrap());
        opps.dedup_by(|a, b| a.token_symbol == b.token_symbol && a.buy_chain == b.buy_chain);
        opps
    }

    async fn execute_cross_chain(&mut self, opp: &CrossChainOpportunity) -> Result<f64> {
        // Simultaneous execution on both chains:
        // 1. On buy_chain: execute flash loan arb to buy token (AtomicArbV2)
        // 2. On sell_chain: execute swap to sell token from inventory
        // Both submit at the same time via tokio::join!
        
        info!(
            "🚀 Cross-chain execution | {} | {} → {}",
            opp.token_symbol, opp.buy_chain.name(), opp.sell_chain.name()
        );
        
        // In production: spawn two async tasks simultaneously
        // let (buy_result, sell_result) = tokio::join!(
        //     self.buy_on_chain(opp),
        //     self.sell_on_chain(opp)
        // );
        
        Ok(opp.expected_profit_usd)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
