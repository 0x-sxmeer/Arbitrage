// ─────────────────────────────────────────────────────────────────────────────
//  pool/mod.rs — Core pool data structures
//  Handles V2 (constant product), V3 (concentrated liquidity), StableSwap
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

pub mod discovery;
pub mod v2;
pub mod v3;

// ── Primitive U256 using primitive-types crate (alloy-compatible) ─────────────
pub use primitive_types::U256;

// ── Fee normalisation ────────────────────────────────────────────────────────
/// All fees in this engine are stored in UNIFIED BASIS POINTS (bps).
///
/// 1 bps = 0.01%
///
/// Common values:
///   Uniswap V2: 30 bps (0.3%)
///   Uniswap V3 low:   5 bps (0.05%)
///   Uniswap V3 mid:  30 bps (0.3%)
///   Uniswap V3 high: 100 bps (1%)
///
/// Uniswap V3 on-chain uses "fee tiers" in hundredths-of-a-bip (e.g. 3000 = 0.3%).
/// Use `fee_tier_to_bps()` to convert V3 raw fee tiers to our canonical bps.
pub const FEE_DENOMINATOR: u32 = 10_000;

/// Convert a Uniswap V3 raw fee tier (hundredths-of-a-bip) to canonical bps.
///
/// Examples:
///   500   → 5 bps  (0.05%)
///   3000  → 30 bps (0.3%)
///   10000 → 100 bps (1%)
pub fn fee_tier_to_bps(v3_fee_tier: u32) -> u32 {
    v3_fee_tier / 100
}

/// Convert our canonical bps back to Uniswap V3 fee tier.
pub fn bps_to_fee_tier(bps: u32) -> u32 {
    bps * 100
}

/// A single liquidity pool on any supported chain.
/// Unified abstraction across V2, V3, and StableSwap invariants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pool {
    /// Unique pool identifier — contract address on EVM, pubkey on Solana
    pub id: String,
    pub chain: ChainId,
    pub dex: DexProtocol,
    pub token_a: Token,
    pub token_b: Token,
    pub pool_type: PoolType,
    pub state: PoolState,
    /// Fee in canonical basis points (0.3% = 30 bps). Always normalised.
    pub fee_bps: u32,
    pub last_updated_block: u64,
    /// Unix timestamp of last state update (for staleness checks)
    pub last_updated_ts: i64,
}

impl Pool {
    /// Returns true if both tokens in the pool are core assets.
    pub fn is_core_pool(&self) -> bool {
        let a = self.token_a.symbol.to_uppercase();
        let b = self.token_b.symbol.to_uppercase();
        let is_core = |sym: &str| {
            matches!(
                sym,
                "WETH" | "USDC" | "USDT" | "USDBC" | "DAI" | "WBTC" | "ETH" | "BTC"
            )
        };
        is_core(&a) && is_core(&b)
    }

    /// Returns true if the pool state is older than `max_blocks` blocks.
    pub fn is_stale(&self, current_block: u64, max_blocks: u64) -> bool {
        current_block.saturating_sub(self.last_updated_block) > max_blocks
    }

    /// Human-readable price of token_a denominated in token_b.
    /// Returns None if reserves are zero.
    pub fn spot_price_a_in_b(&self) -> Option<f64> {
        match self.pool_type {
            PoolType::ConstantProduct => {
                let ra = self.state.reserve_a.low_u128() as f64;
                let rb = self.state.reserve_b.low_u128() as f64;
                if ra == 0.0 {
                    return None;
                }
                Some(rb / ra)
            }
            PoolType::ConcentratedLiquidity => self.state.sqrt_price_x96.map(|sqp| {
                v3::sqrt_price_x96_to_price(sqp, self.token_a.decimals, self.token_b.decimals)
            }),
            PoolType::StableSwap => {
                // StableSwap prices hover near 1:1 — use reserve ratio as approximation
                let ra = self.state.reserve_a.low_u128() as f64;
                let rb = self.state.reserve_b.low_u128() as f64;
                if ra == 0.0 {
                    return None;
                }
                Some(rb / ra)
            }
        }
    }

    /// Returns true if the pool has meaningful liquidity for trading.
    pub fn has_liquidity(&self) -> bool {
        match self.pool_type {
            PoolType::ConstantProduct | PoolType::StableSwap => {
                !self.state.reserve_a.is_zero() && !self.state.reserve_b.is_zero()
            }
            PoolType::ConcentratedLiquidity => {
                self.state.liquidity.map_or(false, |l| l > 0) && self.state.sqrt_price_x96.is_some()
            }
        }
    }

    /// Summary string for logging.
    pub fn summary(&self) -> String {
        format!(
            "{}:{}/{} ({}:{} fee={}bps)",
            self.chain.name(),
            self.token_a.symbol,
            self.token_b.symbol,
            self.dex.name(),
            match self.pool_type {
                PoolType::ConstantProduct => "V2",
                PoolType::ConcentratedLiquidity => "V3",
                PoolType::StableSwap => "SS",
            },
            self.fee_bps,
        )
    }
}

/// Pool invariant type — determines which math library to use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PoolType {
    /// x·y = k  (Uniswap V2, Sushiswap, PancakeSwap, Raydium standard)
    ConstantProduct,
    /// Tick-based concentrated liquidity (Uniswap V3, Orca Whirlpool)
    ConcentratedLiquidity,
    /// Curve StableSwap invariant (Curve, Osmosis StableSwap, Saddle)
    StableSwap,
}

/// Live state of a liquidity pool — updated every block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolState {
    /// Token A reserves (raw, no decimal scaling)
    pub reserve_a: U256,
    /// Token B reserves (raw, no decimal scaling)
    pub reserve_b: U256,

    // ── V3 / Concentrated Liquidity fields ────────────────────────────────────
    /// Current sqrt price in Q64.96 fixed-point format
    pub sqrt_price_x96: Option<U256>,
    /// Current active tick index
    pub tick: Option<i32>,
    /// Active liquidity in current tick range
    pub liquidity: Option<u128>,

    // ── StableSwap fields ─────────────────────────────────────────────────────
    /// Curve amplification coefficient (A parameter)
    pub amp_coeff: Option<u64>,
}

impl PoolState {
    /// Create an empty / zero state.
    pub fn empty() -> Self {
        Self {
            reserve_a: U256::zero(),
            reserve_b: U256::zero(),
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            amp_coeff: None,
        }
    }
}

/// Helper to resolve canonical token decimals by address (case-insensitive).
pub fn get_token_decimals(address: &str) -> u8 {
    match address.to_lowercase().as_str() {
        "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913" => 6, // USDC on Base
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48" => 6, // USDC on Ethereum
        "0xd9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca" => 6, // USDbC on Base
        "0xfde4c96c8593536e31f229ea8f37b2ada2699bb2" => 6, // USDT on Base
        "0xdac17f958d2ee523a2206206994597c13d831ec7" => 6, // USDT on Ethereum
        "0x0555e30da8f98308edb960aa94c0db47230d2b9c" => 8, // WBTC on Base
        "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599" => 8, // WBTC on Ethereum
        _ => 18,
    }
}

/// ERC-20 / SPL token descriptor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    /// Contract address (EVM hex) or mint pubkey (Solana base58)
    pub address: String,
    pub symbol: String,
    pub decimals: u8,
}

impl Token {
    /// Convert raw U256 amount to human-readable f64.
    pub fn to_decimal(&self, raw: U256) -> f64 {
        raw.low_u128() as f64 / 10f64.powi(self.decimals as i32)
    }
}

/// Supported blockchain networks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChainId {
    Ethereum, // Chain ID 1
    Base,     // Chain ID 8453
    Arbitrum, // Chain ID 42161
    Solana,   // SVM — no EVM chain ID
    Osmosis,  // Cosmos IBC — chain "osmosis-1"
}

impl std::fmt::Display for ChainId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl ChainId {
    pub fn evm_chain_id(&self) -> Option<u64> {
        match self {
            ChainId::Ethereum => Some(1),
            ChainId::Base => Some(8453),
            ChainId::Arbitrum => Some(42161),
            _ => None,
        }
    }

    pub fn is_evm(&self) -> bool {
        self.evm_chain_id().is_some()
    }

    pub fn name(&self) -> &'static str {
        match self {
            ChainId::Ethereum => "ethereum",
            ChainId::Base => "base",
            ChainId::Arbitrum => "arbitrum",
            ChainId::Solana => "solana",
            ChainId::Osmosis => "osmosis",
        }
    }
}

/// DEX protocol identifier
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DexProtocol {
    UniswapV2,
    UniswapV3,
    SushiSwap,
    PancakeSwapV3,
    Raydium,
    OrcaWhirlpool,
    Osmosis,
    Curve,
    AerodromeV2,
    Aerodrome,
}

impl DexProtocol {
    pub fn name(&self) -> &'static str {
        match self {
            DexProtocol::UniswapV2 => "Uniswap V2",
            DexProtocol::UniswapV3 => "Uniswap V3",
            DexProtocol::SushiSwap => "SushiSwap",
            DexProtocol::PancakeSwapV3 => "PancakeSwap V3",
            DexProtocol::Raydium => "Raydium",
            DexProtocol::OrcaWhirlpool => "Orca Whirlpool",
            DexProtocol::Osmosis => "Osmosis",
            DexProtocol::Curve => "Curve",
            DexProtocol::AerodromeV2 => "Aerodrome V2",
            DexProtocol::Aerodrome => "Aerodrome",
        }
    }

    /// Whether this DEX uses V3-style concentrated liquidity.
    pub fn is_clmm(&self) -> bool {
        matches!(
            self,
            DexProtocol::UniswapV3
                | DexProtocol::PancakeSwapV3
                | DexProtocol::OrcaWhirlpool
                | DexProtocol::Aerodrome
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_tier_conversion() {
        assert_eq!(fee_tier_to_bps(500), 5);
        assert_eq!(fee_tier_to_bps(3000), 30);
        assert_eq!(fee_tier_to_bps(10000), 100);
        assert_eq!(bps_to_fee_tier(30), 3000);
    }

    #[test]
    fn test_chain_id_evm() {
        assert!(ChainId::Ethereum.is_evm());
        assert!(ChainId::Base.is_evm());
        assert!(!ChainId::Solana.is_evm());
        assert!(!ChainId::Osmosis.is_evm());
    }

    #[test]
    fn test_pool_state_empty() {
        let s = PoolState::empty();
        assert!(s.reserve_a.is_zero());
        assert!(s.sqrt_price_x96.is_none());
    }
}
