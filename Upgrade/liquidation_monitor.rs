// ─────────────────────────────────────────────────────────────────────────────
//  liquidations/liquidation_monitor.rs — DeFi Liquidation Monitor
//
//  Monitors Aave V3 and Moonwell on Base for undercollateralized positions.
//  Executes liquidations for the ~5% bonus.
//
//  Health Factor < 1.0 → position eligible for liquidation
//  Liquidation bonus: 5% on Aave, 8% on Moonwell
//  Expected profit: $50-$500 per liquidation depending on position size
//
//  Algorithm:
//    1. Index all borrowers from protocol events (UserBorrowed, etc.)
//    2. Every block, sample the N most at-risk positions (lowest HF)
//    3. For positions with HF < 1.05, fetch exact health factor on-chain
//    4. If HF < 1.0: compute liquidation params and fire tx
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// ─────────────────────────────────────────────────────────────────────────────
//  Protocol interfaces (ABI selectors hardcoded for gas efficiency)
// ─────────────────────────────────────────────────────────────────────────────

/// Aave V3 Pool — function selectors
pub const AAVE_GET_USER_ACCOUNT_DATA: [u8; 4] = [0xbf, 0x92, 0x85, 0x7c]; // getUserAccountData(address)
pub const AAVE_LIQUIDATION_CALL: [u8; 4]      = [0x00, 0xe8, 0xb4, 0xd5]; // liquidationCall(...)

/// Moonwell Comptroller
pub const MOONWELL_ACCOUNT_LIQUIDITY: [u8; 4] = [0x5e, 0xc8, 0x8c, 0x79]; // getAccountLiquidity(address)

// ─────────────────────────────────────────────────────────────────────────────
//  Borrower position tracking
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BorrowerPosition {
    pub address:              String,
    pub protocol:             Protocol,
    /// Collateral assets and amounts (token_address → amount in USD)
    pub collateral_usd:       f64,
    /// Borrow amount (in USD)
    pub debt_usd:             f64,
    /// Health factor (1e18 precision from Aave, or raw from Moonwell)
    pub health_factor:        f64,
    /// Largest collateral asset — liquidation target
    pub collateral_token:     String,
    /// Debt asset to repay
    pub debt_token:           String,
    /// Max repayable amount (50% of debt for Aave)
    pub max_liquidatable_usd: f64,
    /// Last updated block
    pub last_block:           u64,
    /// Estimated liquidation bonus in USD
    pub bonus_usd:            f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Protocol { AaveV3, Moonwell }

impl Protocol {
    pub fn liquidation_bonus(&self) -> f64 {
        match self {
            Protocol::AaveV3   => 0.05,  // 5% bonus
            Protocol::Moonwell => 0.08,  // 8% bonus
        }
    }
    pub fn max_close_factor(&self) -> f64 {
        match self {
            Protocol::AaveV3   => 0.50,  // liquidate up to 50% of debt
            Protocol::Moonwell => 0.50,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  LiquidationOpportunity — passed to executor
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LiquidationOpportunity {
    pub borrower:          String,
    pub protocol:          Protocol,
    pub protocol_address:  String,
    /// Token we repay (we must have or flash-borrow this)
    pub debt_token:        String,
    pub debt_repay_amount: u128,  // raw token units
    /// Token we receive as collateral
    pub collateral_token:  String,
    /// Expected received collateral amount
    pub collateral_amount: u128,
    /// Liquidation bonus in USD
    pub bonus_usd:         f64,
    /// Gas cost estimate in USD
    pub gas_cost_usd:      f64,
    /// Net profit = bonus - gas - flash_loan_fee
    pub net_profit_usd:    f64,
    pub health_factor:     f64,
}

// ─────────────────────────────────────────────────────────────────────────────
//  LiquidationMonitor
// ─────────────────────────────────────────────────────────────────────────────

pub struct LiquidationMonitor {
    /// All known borrowers, keyed by address
    pub borrowers: Arc<RwLock<HashMap<String, BorrowerPosition>>>,
    /// Minimum net profit to execute (USD)
    pub min_profit_usd: f64,
    /// Aave V3 Pool address on Base
    pub aave_pool_address: String,
    /// Moonwell Comptroller address on Base
    pub moonwell_comptroller: String,
    /// Token prices (USD)
    pub token_prices: Arc<RwLock<HashMap<String, f64>>>,
    /// Statistics
    pub liquidations_executed: u64,
    pub total_bonus_usd:       f64,
}

impl LiquidationMonitor {
    /// Known Base mainnet addresses
    pub const AAVE_POOL_BASE:           &'static str = "0xA238Dd80C259a72e81d7e4664a9801593F98d1c5";
    pub const MOONWELL_COMPTROLLER_BASE: &'static str = "0xfBb21d0380beE3312B33c4353c8936a0F13EF26C";
    pub const USDC_BASE:                 &'static str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
    pub const WETH_BASE:                 &'static str = "0x4200000000000000000000000000000000000006";
    pub const WBTC_BASE:                 &'static str = "0x0555E30da8f98308EdB960aa94C0Db47230d2B9c";

    pub fn new(
        min_profit_usd: f64,
        token_prices: Arc<RwLock<HashMap<String, f64>>>,
    ) -> Self {
        Self {
            borrowers:            Arc::new(RwLock::new(HashMap::new())),
            min_profit_usd,
            aave_pool_address:    Self::AAVE_POOL_BASE.to_string(),
            moonwell_comptroller: Self::MOONWELL_COMPTROLLER_BASE.to_string(),
            token_prices,
            liquidations_executed: 0,
            total_bonus_usd:       0.0,
        }
    }

    /// Called every new block — scan at-risk positions
    pub async fn scan_block(&self, block_number: u64) -> Vec<LiquidationOpportunity> {
        let borrowers = self.borrowers.read().await;
        let mut opportunities = Vec::new();

        // Filter to at-risk positions (HF < 1.05 worth a fresh on-chain check)
        let at_risk: Vec<_> = borrowers.values()
            .filter(|b| b.health_factor < 1.05)
            .collect();

        if at_risk.is_empty() { return opportunities; }

        debug!("🔍 Checking {} at-risk positions at block {}", at_risk.len(), block_number);

        for borrower in at_risk {
            if let Some(opp) = self.evaluate_liquidation(borrower).await {
                if opp.net_profit_usd >= self.min_profit_usd {
                    info!(
                        "💸 LIQUIDATION OPPORTUNITY | {} | hf={:.4} | bonus=${:.2} | net=${:.2}",
                        opp.borrower, opp.health_factor, opp.bonus_usd, opp.net_profit_usd
                    );
                    opportunities.push(opp);
                }
            }
        }

        // Sort by net profit descending
        opportunities.sort_by(|a, b| b.net_profit_usd.partial_cmp(&a.net_profit_usd).unwrap());
        opportunities
    }

    async fn evaluate_liquidation(&self, pos: &BorrowerPosition) -> Option<LiquidationOpportunity> {
        let token_prices = self.token_prices.read().await;

        // Only liquidate if HF < 1.0
        if pos.health_factor >= 1.0 { return None; }

        let debt_token_price = token_prices.get(&pos.debt_token).copied().unwrap_or(0.0);
        if debt_token_price <= 0.0 { return None; }

        // Max repayable: 50% of debt for Aave
        let max_repay_usd      = pos.debt_usd * pos.protocol.max_close_factor();
        let bonus_rate         = pos.protocol.liquidation_bonus();
        let collateral_received_usd = max_repay_usd * (1.0 + bonus_rate);
        let bonus_usd          = max_repay_usd * bonus_rate;

        // Costs: gas ~$3 + flash loan cost (0% Balancer)
        let gas_cost_usd       = 5.0;
        let flash_fee          = max_repay_usd * 0.0;  // Balancer = 0%
        let net_profit_usd     = bonus_usd - gas_cost_usd - flash_fee;

        let debt_decimals      = 18u8; // assume 18 for now
        let repay_amount       = (max_repay_usd / debt_token_price * 10f64.powi(debt_decimals as i32)) as u128;

        Some(LiquidationOpportunity {
            borrower:          pos.address.clone(),
            protocol:          pos.protocol.clone(),
            protocol_address:  self.aave_pool_address.clone(),
            debt_token:        pos.debt_token.clone(),
            debt_repay_amount: repay_amount,
            collateral_token:  pos.collateral_token.clone(),
            collateral_amount: (collateral_received_usd / debt_token_price * 1e18) as u128,
            bonus_usd,
            gas_cost_usd,
            net_profit_usd,
            health_factor:     pos.health_factor,
        })
    }

    /// Index a new borrower from protocol events
    pub async fn index_borrower(&self, address: String, protocol: Protocol) {
        let mut borrowers = self.borrowers.write().await;
        borrowers.entry(address.clone()).or_insert_with(|| BorrowerPosition {
            address:              address,
            protocol,
            collateral_usd:       0.0,
            debt_usd:             0.0,
            health_factor:        f64::MAX,
            collateral_token:     String::new(),
            debt_token:           String::new(),
            max_liquidatable_usd: 0.0,
            last_block:           0,
            bonus_usd:            0.0,
        });
    }

    /// Update health factor for a borrower (called after on-chain fetch)
    pub async fn update_health_factor(&self, address: &str, hf: f64, debt_usd: f64, collateral_usd: f64) {
        let mut borrowers = self.borrowers.write().await;
        if let Some(pos) = borrowers.get_mut(address) {
            pos.health_factor   = hf;
            pos.debt_usd        = debt_usd;
            pos.collateral_usd  = collateral_usd;
            pos.max_liquidatable_usd = debt_usd * pos.protocol.max_close_factor();
            pos.bonus_usd       = pos.max_liquidatable_usd * pos.protocol.liquidation_bonus();
        }
    }
}
