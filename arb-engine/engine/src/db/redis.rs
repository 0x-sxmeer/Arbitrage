// ─────────────────────────────────────────────────────────────────────────────
//  db/redis.rs — Pool State Cache (Real Implementation)
//
//  Redis is the hot cache for pool state data that needs sub-millisecond reads.
//  Keys:
//    pool:{chain}:{pool_id}        → serialized Pool (JSON), TTL = 2 blocks (~24s)
//    opportunity:{uuid}            → "1" (dedup flag), TTL = 60s
//    gas_price:gwei                → current gas price as string, TTL = 15s
//
//  Uses ConnectionManager for automatic reconnection on transient failures.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use anyhow::{Context, Result};
use redis::{aio::ConnectionManager, AsyncCommands, Client};
use tracing::{debug, info, warn};

use crate::pool::{ChainId, Pool};

// ── TTL constants (seconds) ───────────────────────────────────────────────────
/// Pool state expires after 2 Ethereum blocks (≈ 24 seconds)
const POOL_STATE_TTL_SECS: u64 = 24;
/// Opportunity record TTL — kept long enough for execution monitoring
const OPPORTUNITY_TTL_SECS: u64 = 60;
/// Gas price TTL — 1 Ethereum block
const GAS_PRICE_TTL_SECS: u64 = 15;

// ─────────────────────────────────────────────────────────────────────────────
//  RedisCache
// ─────────────────────────────────────────────────────────────────────────────

pub struct RedisCache {
    conn: ConnectionManager,
}

impl RedisCache {
    /// Connect to Redis and return a cache handle.
    /// Uses ConnectionManager for automatic reconnection on failures.
    pub async fn new(redis_url: &str) -> Result<Self> {
        let client = Client::open(redis_url)
            .with_context(|| format!("Invalid Redis URL: {}", redis_url))?;

        let conn = ConnectionManager::new(client)
            .await
            .with_context(|| format!("Failed to connect to Redis: {}", redis_url))?;

        info!("Redis connected via ConnectionManager (auto-reconnect enabled)");
        Ok(Self { conn })
    }

    /// Alias for `new()` — used by main.rs for clarity.
    pub async fn connect(redis_url: &str) -> Result<Self> {
        Self::new(redis_url).await
    }

    // ── Pool state cache ──────────────────────────────────────────────────────

    /// Cache a pool's full state as JSON with TTL.
    pub async fn set_pool(&self, pool: &Pool) -> Result<()> {
        let key = pool_key(pool.chain, &pool.id);
        let json = serde_json::to_string(pool)
            .context("Failed to serialize pool")?;

        let mut conn = self.conn.clone();
        conn.set_ex::<_, _, ()>(&key, &json, POOL_STATE_TTL_SECS)
            .await
            .with_context(|| format!("Failed to SET pool {}", key))?;

        debug!(key = %key, ttl = POOL_STATE_TTL_SECS, "Pool cached in Redis");
        Ok(())
    }

    /// Retrieve a cached pool by chain and pool ID.
    pub async fn get_pool(&self, chain: ChainId, pool_id: &str) -> Result<Option<Pool>> {
        let key = pool_key(chain, pool_id);
        let mut conn = self.conn.clone();

        let json: Option<String> = conn.get(&key)
            .await
            .with_context(|| format!("Failed to GET pool {}", key))?;

        match json {
            Some(data) => {
                let pool: Pool = serde_json::from_str(&data)
                    .with_context(|| format!("Failed to deserialize pool from key {}", key))?;
                Ok(Some(pool))
            }
            None => Ok(None),
        }
    }

    /// Delete a cached pool (force re-fetch from chain).
    pub async fn invalidate_pool(&self, chain: ChainId, pool_id: &str) -> Result<()> {
        let key = pool_key(chain, pool_id);
        let mut conn = self.conn.clone();
        conn.del::<_, ()>(&key)
            .await
            .with_context(|| format!("Failed to DEL pool {}", key))?;

        debug!(key = %key, "Pool invalidated in Redis");
        Ok(())
    }

    /// Get remaining TTL for a cached pool (seconds). Returns -2 if key doesn't exist.
    pub async fn pool_ttl(&self, chain: ChainId, pool_id: &str) -> Result<i64> {
        let key = pool_key(chain, pool_id);
        let mut conn = self.conn.clone();
        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap_or(-2);
        Ok(ttl)
    }

    // ── Gas price cache ───────────────────────────────────────────────────────

    /// Cache the current effective gas price (gwei).
    pub async fn set_gas_price_gwei(&self, gwei: f64) -> Result<()> {
        let mut conn = self.conn.clone();
        conn.set_ex::<_, _, ()>("gas_price:gwei", gwei.to_string(), GAS_PRICE_TTL_SECS)
            .await
            .context("Failed to cache gas price")?;
        Ok(())
    }

    /// Get cached gas price. Returns 20.0 gwei as default if not cached.
    pub async fn get_gas_price_gwei(&self) -> f64 {
        let mut conn = self.conn.clone();
        let result: Option<String> = conn.get("gas_price:gwei").await.unwrap_or(None);
        result
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(20.0)
    }

    // ── Opportunity deduplication ─────────────────────────────────────────────

    /// Mark an opportunity as seen. Returns true if this is the first time
    /// (SETNX semantics — set only if not exists).
    pub async fn mark_opportunity_seen(&self, opportunity_id: &str) -> Result<bool> {
        let key = format!("opportunity:{}", opportunity_id);
        let mut conn = self.conn.clone();

        // SETNX: returns true if the key was set (i.e., first time seen)
        let was_set: bool = conn.set_nx(&key, "1")
            .await
            .with_context(|| format!("Failed to SETNX opportunity {}", key))?;

        if was_set {
            // Set TTL so stale entries expire
            let _: () = conn.expire(&key, OPPORTUNITY_TTL_SECS as i64)
                .await
                .unwrap_or(());
        }

        Ok(was_set)
    }

    // ── Generic key/value operations ──────────────────────────────────────────

    /// Get a raw string value by key.
    pub async fn get_raw(&self, key: &str) -> Result<Option<String>> {
        let mut conn = self.conn.clone();
        let val: Option<String> = conn.get(key)
            .await
            .with_context(|| format!("Failed to GET key {}", key))?;
        Ok(val)
    }

    /// Set a raw string value with TTL.
    pub async fn set_raw(&self, key: &str, value: &str, ttl_secs: usize) -> Result<()> {
        let mut conn = self.conn.clone();
        conn.set_ex::<_, _, ()>(key, value, ttl_secs as u64)
            .await
            .with_context(|| format!("Failed to SET key {}", key))?;
        Ok(())
    }

    /// Health check — PING the Redis server.
    pub async fn ping(&self) -> Result<()> {
        let mut conn = self.conn.clone();
        let pong: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .context("Redis PING failed")?;
        if pong != "PONG" {
            anyhow::bail!("Unexpected PING response: {}", pong);
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Key helpers
// ─────────────────────────────────────────────────────────────────────────────

fn pool_key(chain: ChainId, pool_id: &str) -> String {
    format!("pool:{}:{}", chain.name(), pool_id)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_key_format() {
        let key = pool_key(ChainId::Ethereum, "0xPool123");
        assert_eq!(key, "pool:ethereum:0xPool123");
    }

    #[test]
    fn test_pool_key_solana() {
        let key = pool_key(ChainId::Solana, "8sLbNZoA1cfnvMJLPfp98ZLAnFSYCFApfJKMbiXNLwxj");
        assert_eq!(key, "pool:solana:8sLbNZoA1cfnvMJLPfp98ZLAnFSYCFApfJKMbiXNLwxj");
    }
}
