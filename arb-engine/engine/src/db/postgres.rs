// ─────────────────────────────────────────────────────────────────────────────
//  db/postgres.rs — Opportunity Log & Pool Registry
//
//  Tables:
//    opportunities    — every discovered arb opportunity (executable or not)
//    pool_registry    — known pools across all chains
//
//  Used for:
//    - Backtesting: replay historical opportunities against actual outcomes
//    - Analytics: P&L tracking, gas efficiency, strategy performance
//    - Monitoring: alert on sudden drop in opportunity frequency
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use tracing::{debug, info};
use uuid::Uuid;

use crate::arb::opportunity::ArbitrageOpportunity;
use crate::pool::{ChainId, Pool};

// ─────────────────────────────────────────────────────────────────────────────
//  PostgresStore
// ─────────────────────────────────────────────────────────────────────────────

pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    /// Connect to PostgreSQL and return a store handle.
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .with_context(|| format!("Failed to connect to PostgreSQL: {}", database_url))?;

        info!("PostgreSQL connected ({} max connections)", pool.size());
        Ok(Self { pool })
    }

    /// Alias for `new()` — used by main.rs for clarity.
    pub async fn connect(database_url: &str) -> Result<Self> {
        Self::new(database_url).await
    }

    /// Run embedded SQL migrations to create tables if they don't exist.
    /// [H-2] Creates all required tables: opportunities, pool_registry,
    ///        executions, and circuit_breaker_events.
    pub async fn run_migrations(&self) -> Result<()> {
        sqlx::query(CREATE_OPPORTUNITIES_TABLE)
            .execute(&self.pool)
            .await
            .context("Failed to create opportunities table")?;

        sqlx::query(CREATE_POOL_REGISTRY_TABLE)
            .execute(&self.pool)
            .await
            .context("Failed to create pool_registry table")?;

        sqlx::query(CREATE_EXECUTIONS_TABLE)
            .execute(&self.pool)
            .await
            .context("Failed to create executions table")?;

        sqlx::query(CREATE_CIRCUIT_BREAKER_TABLE)
            .execute(&self.pool)
            .await
            .context("Failed to create circuit_breaker_events table")?;

        sqlx::query(CREATE_OPPORTUNITIES_INDEX)
            .execute(&self.pool)
            .await
            .context("Failed to create opportunities index")?;

        sqlx::query(CREATE_EXECUTIONS_INDEX)
            .execute(&self.pool)
            .await
            .context("Failed to create executions index")?;

        info!("✓ Database migrations applied (6 tables/indexes)");
        Ok(())
    }

    // ── Opportunity logging ───────────────────────────────────────────────────

    /// Insert a discovered arbitrage opportunity into the log table.
    pub async fn insert_opportunity(&self, opp: &ArbitrageOpportunity) -> Result<()> {
        let route_json = serde_json::to_value(&opp.route)
            .context("Failed to serialize route")?;

        sqlx::query(INSERT_OPPORTUNITY)
            .bind(opp.id)
            .bind(opp.chain.name())
            .bind(opp.start_token.as_str())
            .bind(opp.input_amount.low_u128() as i64)
            .bind(opp.gross_output.low_u128() as i64)
            .bind(opp.net_expected_value as i64)
            .bind(opp.is_executable)
            .bind(opp.estimated_gas_units as i64)
            .bind(opp.gas_price_gwei)
            .bind(opp.price_impact_bps as i32)
            .bind(opp.discovered_at_block as i64)
            .bind(opp.discovered_at)
            .bind(route_json)
            .execute(&self.pool)
            .await
            .with_context(|| format!("Failed to insert opportunity {}", opp.id))?;

        debug!(id = %opp.id, "Opportunity logged to PostgreSQL");
        Ok(())
    }

    /// Fetch recent opportunities for monitoring dashboards.
    pub async fn get_recent_opportunities(
        &self,
        limit: i64,
        executable_only: bool,
    ) -> Result<Vec<OpportunityRow>> {
        let rows = if executable_only {
            sqlx::query_as::<_, OpportunityRow>(SELECT_OPPORTUNITIES_EXECUTABLE)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
        } else {
            sqlx::query_as::<_, OpportunityRow>(SELECT_OPPORTUNITIES_ALL)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
        };

        rows.context("Failed to fetch opportunities")
    }

    /// Count executable opportunities in the last N minutes (health metric).
    pub async fn count_executable_since(&self, minutes: i64) -> Result<i64> {
        let row = sqlx::query(COUNT_EXECUTABLE_SINCE)
            .bind(minutes)
            .fetch_one(&self.pool)
            .await
            .context("Failed to count opportunities")?;

        Ok(row.try_get::<i64, _>("count").unwrap_or(0))
    }

    // ── Pool registry ─────────────────────────────────────────────────────────

    /// Register a pool in the pool_registry table.
    pub async fn upsert_pool(&self, pool: &Pool) -> Result<()> {
        sqlx::query(UPSERT_POOL)
            .bind(pool.id.as_str())
            .bind(pool.chain.name())
            .bind(pool.dex.name())
            .bind(pool.token_a.address.as_str())
            .bind(pool.token_a.symbol.as_str())
            .bind(pool.token_b.address.as_str())
            .bind(pool.token_b.symbol.as_str())
            .bind(pool.fee_bps as i32)
            .bind(format!("{:?}", pool.pool_type))
            .execute(&self.pool)
            .await
            .with_context(|| format!("Failed to upsert pool {}", pool.id))?;

        debug!(pool_id = %pool.id, "Pool upserted in registry");
        Ok(())
    }

    /// Fetch all pools for a given chain (to warm up the liquidity graph on startup).
    pub async fn get_pools_by_chain(&self, chain: ChainId) -> Result<Vec<PoolRegistryRow>> {
        sqlx::query_as::<_, PoolRegistryRow>(SELECT_POOLS_BY_CHAIN)
            .bind(chain.name())
            .fetch_all(&self.pool)
            .await
            .context("Failed to fetch pool registry")
    }

    /// Fetch ALL pools from the registry and convert to Pool structs.
    /// Used for graph warm-up on startup.
    pub async fn list_pools(&self) -> Result<Vec<crate::pool::Pool>> {
        let rows = sqlx::query_as::<_, PoolRegistryRow>(SELECT_ALL_POOLS)
            .fetch_all(&self.pool)
            .await
            .context("Failed to fetch pool registry")?;

        Ok(rows.into_iter().map(|r| r.to_pool()).collect())
    }

    /// Pool count (for startup logging).
    pub async fn pool_count(&self) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pool_registry")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Row types (returned by queries)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
pub struct OpportunityRow {
    pub id:                 Uuid,
    pub chain:              String,
    pub start_token:        String,
    pub input_amount_wei:   i64,
    pub gross_output_wei:   i64,
    pub net_expected_value: i64,
    pub is_executable:      bool,
    pub gas_units:          i64,
    pub gas_price_gwei:     f64,
    pub price_impact_bps:   i32,
    pub block_number:       i64,
    pub discovered_at:      DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct PoolRegistryRow {
    pub pool_id:       String,
    pub chain:         String,
    pub dex:           String,
    pub token_a_addr:  String,
    pub token_a_sym:   String,
    pub token_b_addr:  String,
    pub token_b_sym:   String,
    pub fee_bps:       i32,
    pub pool_type:     String,
}

impl PoolRegistryRow {
    /// Convert a registry row back into a full Pool struct.
    /// State is empty — caller must fetch live state from cache/chain.
    pub fn to_pool(&self) -> crate::pool::Pool {
        use crate::pool::*;

        let chain = match self.chain.as_str() {
            "ethereum" => ChainId::Ethereum,
            "base"     => ChainId::Base,
            "arbitrum" => ChainId::Arbitrum,
            "solana"   => ChainId::Solana,
            "osmosis"  => ChainId::Osmosis,
            _          => ChainId::Ethereum,
        };

        let dex = match self.dex.as_str() {
            "Uniswap V2"     => DexProtocol::UniswapV2,
            "Uniswap V3"     => DexProtocol::UniswapV3,
            "SushiSwap"      => DexProtocol::SushiSwap,
            "PancakeSwap V3" => DexProtocol::PancakeSwapV3,
            "Raydium"        => DexProtocol::Raydium,
            "Orca Whirlpool" => DexProtocol::OrcaWhirlpool,
            "Osmosis"        => DexProtocol::Osmosis,
            "Curve"          => DexProtocol::Curve,
            _                => DexProtocol::UniswapV3,
        };

        let pool_type = match self.pool_type.as_str() {
            "ConstantProduct"      => PoolType::ConstantProduct,
            "ConcentratedLiquidity" => PoolType::ConcentratedLiquidity,
            "StableSwap"           => PoolType::StableSwap,
            _                      => PoolType::ConstantProduct,
        };

        Pool {
            id: self.pool_id.clone(),
            chain,
            dex,
            token_a: Token {
                address:  self.token_a_addr.clone(),
                symbol:   self.token_a_sym.clone(),
                decimals: get_token_decimals(&self.token_a_addr),
            },
            token_b: Token {
                address:  self.token_b_addr.clone(),
                symbol:   self.token_b_sym.clone(),
                decimals: get_token_decimals(&self.token_b_addr),
            },
            pool_type,
            fee_bps: self.fee_bps as u32,
            state: PoolState::empty(),
            last_updated_block: 0,
            last_updated_ts:    0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  SQL statements
// ─────────────────────────────────────────────────────────────────────────────

const CREATE_OPPORTUNITIES_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS opportunities (
    id                  UUID        PRIMARY KEY,
    chain               TEXT        NOT NULL,
    start_token         TEXT        NOT NULL,
    input_amount_wei    BIGINT      NOT NULL,
    gross_output_wei    BIGINT      NOT NULL,
    net_expected_value  BIGINT      NOT NULL,
    is_executable       BOOLEAN     NOT NULL,
    gas_units           BIGINT      NOT NULL,
    gas_price_gwei      DOUBLE PRECISION NOT NULL,
    price_impact_bps    INTEGER     NOT NULL,
    block_number        BIGINT      NOT NULL,
    discovered_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    route               JSONB
)
"#;

const CREATE_POOL_REGISTRY_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS pool_registry (
    pool_id     TEXT    PRIMARY KEY,
    chain       TEXT    NOT NULL,
    dex         TEXT    NOT NULL,
    token_a_addr TEXT   NOT NULL,
    token_a_sym TEXT    NOT NULL,
    token_b_addr TEXT   NOT NULL,
    token_b_sym TEXT    NOT NULL,
    fee_bps     INTEGER NOT NULL,
    pool_type   TEXT    NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
)
"#;

const CREATE_OPPORTUNITIES_INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS idx_opportunities_discovered_at
    ON opportunities (discovered_at DESC);
CREATE INDEX IF NOT EXISTS idx_opportunities_executable
    ON opportunities (is_executable, discovered_at DESC);
"#;

const INSERT_OPPORTUNITY: &str = r#"
INSERT INTO opportunities (
    id, chain, start_token, input_amount_wei, gross_output_wei,
    net_expected_value, is_executable, gas_units, gas_price_gwei,
    price_impact_bps, block_number, discovered_at, route
) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
ON CONFLICT (id) DO NOTHING
"#;

const UPSERT_POOL: &str = r#"
INSERT INTO pool_registry (pool_id, chain, dex, token_a_addr, token_a_sym, token_b_addr, token_b_sym, fee_bps, pool_type)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
ON CONFLICT (pool_id) DO UPDATE
    SET dex = EXCLUDED.dex, fee_bps = EXCLUDED.fee_bps, updated_at = NOW()
"#;

const SELECT_OPPORTUNITIES_ALL: &str = r#"
SELECT id, chain, start_token, input_amount_wei, gross_output_wei,
       net_expected_value, is_executable, gas_units, gas_price_gwei,
       price_impact_bps, block_number, discovered_at
FROM opportunities
ORDER BY discovered_at DESC
LIMIT $1
"#;

const SELECT_OPPORTUNITIES_EXECUTABLE: &str = r#"
SELECT id, chain, start_token, input_amount_wei, gross_output_wei,
       net_expected_value, is_executable, gas_units, gas_price_gwei,
       price_impact_bps, block_number, discovered_at
FROM opportunities
WHERE is_executable = true
ORDER BY discovered_at DESC
LIMIT $1
"#;

const SELECT_POOLS_BY_CHAIN: &str = r#"
SELECT pool_id, chain, dex, token_a_addr, token_a_sym, token_b_addr, token_b_sym, fee_bps, pool_type
FROM pool_registry
WHERE chain = $1
ORDER BY pool_id
"#;

const SELECT_ALL_POOLS: &str = r#"
SELECT pool_id, chain, dex, token_a_addr, token_a_sym, token_b_addr, token_b_sym, fee_bps, pool_type
FROM pool_registry
ORDER BY chain, pool_id
"#;

const COUNT_EXECUTABLE_SINCE: &str = r#"
SELECT COUNT(*) as count
FROM opportunities
WHERE is_executable = true
  AND discovered_at > NOW() - ($1 * INTERVAL '1 minute')
"#;

// ── [H-2] Execution tracking table ───────────────────────────────────────────
const CREATE_EXECUTIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS executions (
    id              BIGSERIAL PRIMARY KEY,
    opportunity_id  UUID REFERENCES opportunities(id),
    tx_hash         VARCHAR(66) NOT NULL UNIQUE,
    block_number    BIGINT,
    gas_used        BIGINT,
    actual_profit   NUMERIC(78, 0),
    status          VARCHAR(20) NOT NULL DEFAULT 'pending',
    submitted_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    confirmed_at    TIMESTAMPTZ
)
"#;

const CREATE_EXECUTIONS_INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS idx_executions_tx_hash
    ON executions(tx_hash);
CREATE INDEX IF NOT EXISTS idx_executions_status
    ON executions(status, submitted_at DESC);
"#;

// ── [H-2] Circuit breaker event log ──────────────────────────────────────────
const CREATE_CIRCUIT_BREAKER_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS circuit_breaker_events (
    id              BIGSERIAL PRIMARY KEY,
    event_type      VARCHAR(20) NOT NULL,
    loss_amount     NUMERIC(78, 0),
    reason          TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
)
"#;
