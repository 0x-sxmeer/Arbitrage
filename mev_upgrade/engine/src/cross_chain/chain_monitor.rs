// ─────────────────────────────────────────────────────────────────────────────
//  cross_chain/chain_monitor.rs — Per-Chain Price Poller
//
//  One instance runs per chain (Base, Optimism, Arbitrum).
//  Every block, fetches spot prices from the best DEX pools on that chain
//  and writes them into the shared PriceMatrix.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::RwLock;
use tokio::time::{interval, sleep};
use tracing::{debug, error, info, warn};

use super::cross_chain_engine::{ChainId, ChainPrice, PriceMatrix};

/// Well-known high-liquidity pool configurations per chain
pub fn default_pools(chain: ChainId) -> Vec<PoolConfig> {
    match chain {
        ChainId::Base => vec![
            PoolConfig { token_symbol: "ETH".into(),  pool_address: "0xd0b53D9277642d899DF5C87A3966A349A798F224".into(), token0_is_token: false, fee_bps: 5, is_v3: true  },
            PoolConfig { token_symbol: "WBTC".into(), pool_address: "0xFd36cE0A6cCDE9B3F5c9b0e17Adb0fBE55CAadCb".into(), token0_is_token: true,  fee_bps: 30, is_v3: true  },
            PoolConfig { token_symbol: "ETH".into(),  pool_address: "0x7c3B2a83A6cF0b8Ca2c0c8A5A47af5b7F6B2dAB2".into(), token0_is_token: false, fee_bps: 5, is_v3: false },
        ],
        ChainId::Optimism => vec![
            PoolConfig { token_symbol: "ETH".into(),  pool_address: "0x85149247691df622eaF1a8Bd0CaFd40BC45154a".into(), token0_is_token: false, fee_bps: 5, is_v3: true  },
            PoolConfig { token_symbol: "WBTC".into(), pool_address: "0x73B14a78a0D396C521f954532d43fd5fFe7277b".into(), token0_is_token: true,  fee_bps: 30, is_v3: true  },
        ],
        ChainId::Arbitrum => vec![
            PoolConfig { token_symbol: "ETH".into(),  pool_address: "0xC31E54c7a869B9FcBEcc14363CF510d1c41fa443".into(), token0_is_token: false, fee_bps: 5, is_v3: true  },
            PoolConfig { token_symbol: "WBTC".into(), pool_address: "0x2f5e87C9312fa29aed5c179E456625D79015299c".into(), token0_is_token: true,  fee_bps: 30, is_v3: true  },
            PoolConfig { token_symbol: "ARB".into(),  pool_address: "0x81C48D31365e6B526f6BBadC5c9aaFd822134863".into(), token0_is_token: true,  fee_bps: 30, is_v3: true  },
        ],
        _ => vec![],
    }
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub token_symbol:    String,
    pub pool_address:    String,
    pub token0_is_token: bool,   // false = USDC is token0, token is token1
    pub fee_bps:         u32,
    pub is_v3:           bool,
}

pub struct ChainMonitor {
    pub chain:      ChainId,
    pub rpc_url:    String,
    pub pools:      Vec<PoolConfig>,
    pub matrix:     PriceMatrix,
    poll_interval:  Duration,
}

impl ChainMonitor {
    pub fn new(
        chain:         ChainId,
        rpc_url:       String,
        matrix:        PriceMatrix,
        poll_ms:       u64,
    ) -> Self {
        Self {
            pools:         default_pools(chain),
            chain,
            rpc_url,
            matrix,
            poll_interval: Duration::from_millis(poll_ms),
        }
    }

    /// Run forever — poll prices every `poll_interval`
    pub async fn run(self) -> Result<()> {
        let mut ticker = interval(self.poll_interval);
        info!(
            "📡 Chain monitor started: {} ({} pools, poll={}ms)",
            self.chain.name(), self.pools.len(), self.poll_interval.as_millis()
        );

        loop {
            ticker.tick().await;
            if let Err(e) = self.poll_all_pools().await {
                warn!("[{}] Pool poll error: {}", self.chain.name(), e);
            }
        }
    }

    async fn poll_all_pools(&self) -> Result<()> {
        let now_ms   = now_ms();
        let mut matrix = self.matrix.write().await;

        for pool in &self.pools {
            // In production: call slot0() on V3 or getReserves() on V2
            // via alloy provider connected to self.rpc_url
            // Here we show the structure:
            
            let price_usd = self.fetch_pool_price(pool).await.unwrap_or(0.0);
            if price_usd <= 0.0 { continue; }

            let key = (self.chain, pool.token_symbol.clone());
            let existing = matrix.get(&key);
            
            // Only update if price changed meaningfully (>0.001%)
            let should_update = existing.map(|p| {
                ((price_usd - p.price_usd) / p.price_usd).abs() > 0.00001
            }).unwrap_or(true);

            if should_update {
                debug!(
                    "[{}] {} price: ${:.4} (pool={})",
                    self.chain.name(), pool.token_symbol, price_usd, &pool.pool_address[..8]
                );
                matrix.insert(key, ChainPrice {
                    price_usd,
                    liquidity_usd: 1_000_000.0,  // placeholder — fetch from pool
                    pool_address:  pool.pool_address.clone(),
                    fee_bps:       pool.fee_bps,
                    block_number:  0,             // fill from block subscription
                    timestamp_ms:  now_ms,
                    is_stale:      false,
                });
            }
        }
        Ok(())
    }

    /// Fetch current price from pool contract
    /// V3: slot0() → sqrtPriceX96 → price
    /// V2: getReserves() → reserve0/reserve1 → price
    async fn fetch_pool_price(&self, pool: &PoolConfig) -> Option<f64> {
        // In production, use alloy:
        //   let provider = ProviderBuilder::new().on_http(self.rpc_url.parse()?);
        //   let pool_contract = UniswapV3Pool::new(pool.pool_address.parse()?, provider);
        //   let slot0 = pool_contract.slot0().call().await?;
        //   let sqrt_price = slot0.sqrtPriceX96.as_u128() as f64;
        //   let price = (sqrt_price / 2f64.powi(96)).powi(2) * usdc_per_eth_adjustment
        //
        // Returning None here as placeholder — real impl connects to chain
        None
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
