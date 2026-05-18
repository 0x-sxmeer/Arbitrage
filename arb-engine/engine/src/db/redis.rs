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

use dashmap::DashMap;

pub struct RedisCache {
    conn: Option<ConnectionManager>,
    fallback: DashMap<String, String>,
    fallback_ttl: DashMap<String, std::time::Instant>,
}

impl RedisCache {
    /// Connect to Redis and return a cache handle.
    /// Uses ConnectionManager for automatic reconnection on failures.
    /// If Redis is offline, falls back to high-speed in-memory DashMap.
    pub async fn new(redis_url: &str) -> Result<Self> {
        let client = match Client::open(redis_url) {
            Ok(c) => c,
            Err(e) => {
                warn!("⚠ Invalid Redis URL: {}. Falling back to in-memory store.", e);
                return Ok(Self {
                    conn: None,
                    fallback: DashMap::new(),
                    fallback_ttl: DashMap::new(),
                });
            }
        };

        match ConnectionManager::new(client).await {
            Ok(conn) => {
                info!("✓ Redis connected via ConnectionManager (auto-reconnect enabled)");
                Ok(Self {
                    conn: Some(conn),
                    fallback: DashMap::new(),
                    fallback_ttl: DashMap::new(),
                })
            }
            Err(e) => {
                warn!("⚠ Redis connection failed: {}. Falling back to high-performance in-memory cache.", e);
                Ok(Self {
                    conn: None,
                    fallback: DashMap::new(),
                    fallback_ttl: DashMap::new(),
                })
            }
        }
    }

    /// Alias for `new()` — used by main.rs for clarity.
    pub async fn connect(redis_url: &str) -> Result<Self> {
        Self::new(redis_url).await
    }

    fn check_expired(&self, key: &str) -> bool {
        if let Some(expiry) = self.fallback_ttl.get(key) {
            if std::time::Instant::now() > *expiry {
                self.fallback.remove(key);
                self.fallback_ttl.remove(key);
                return true;
            }
        }
        false
    }

    // ── Pool state cache ──────────────────────────────────────────────────────

    /// Cache a pool's full state as JSON with TTL.
    pub async fn set_pool(&self, pool: &Pool) -> Result<()> {
        let key = pool_key(pool.chain, &pool.id);
        let json = serde_json::to_string(pool)
            .context("Failed to serialize pool")?;

        if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            conn.set_ex::<_, _, ()>(&key, &json, POOL_STATE_TTL_SECS)
                .await
                .with_context(|| format!("Failed to SET pool {}", key))?;
        } else {
            self.fallback.insert(key.clone(), json);
            self.fallback_ttl.insert(key.clone(), std::time::Instant::now() + std::time::Duration::from_secs(POOL_STATE_TTL_SECS));
        }

        debug!(key = %key, ttl = POOL_STATE_TTL_SECS, "Pool cached");
        Ok(())
    }

    /// Retrieve a cached pool by chain and pool ID.
    pub async fn get_pool(&self, chain: ChainId, pool_id: &str) -> Result<Option<Pool>> {
        let key = pool_key(chain, pool_id);

        let json = if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            conn.get(&key)
                .await
                .with_context(|| format!("Failed to GET pool {}", key))?
        } else {
            if self.check_expired(&key) {
                None
            } else {
                self.fallback.get(&key).map(|v| v.clone())
            }
        };

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
        if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            conn.del::<_, ()>(&key)
                .await
                .with_context(|| format!("Failed to DEL pool {}", key))?;
        } else {
            self.fallback.remove(&key);
            self.fallback_ttl.remove(&key);
        }

        debug!(key = %key, "Pool invalidated");
        Ok(())
    }

    /// Get remaining TTL for a cached pool (seconds). Returns -2 if key doesn't exist.
    pub async fn pool_ttl(&self, chain: ChainId, pool_id: &str) -> Result<i64> {
        let key = pool_key(chain, pool_id);
        if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            let ttl: i64 = redis::cmd("TTL")
                .arg(&key)
                .query_async(&mut conn)
                .await
                .unwrap_or(-2);
            Ok(ttl)
        } else {
            if let Some(expiry) = self.fallback_ttl.get(&key) {
                let now = std::time::Instant::now();
                if now > *expiry {
                    self.fallback.remove(&key);
                    self.fallback_ttl.remove(&key);
                    Ok(-2)
                } else {
                    Ok((*expiry - now).as_secs() as i64)
                }
            } else {
                Ok(-2)
            }
        }
    }

    // ── Gas price cache ───────────────────────────────────────────────────────

    /// Cache the current effective gas price (gwei).
    pub async fn set_gas_price_gwei(&self, gwei: f64) -> Result<()> {
        if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            conn.set_ex::<_, _, ()>("gas_price:gwei", gwei.to_string(), GAS_PRICE_TTL_SECS)
                .await
                .context("Failed to cache gas price")?;
        } else {
            self.fallback.insert("gas_price:gwei".to_string(), gwei.to_string());
            self.fallback_ttl.insert("gas_price:gwei".to_string(), std::time::Instant::now() + std::time::Duration::from_secs(GAS_PRICE_TTL_SECS));
        }
        Ok(())
    }

    /// Get cached gas price. Returns 20.0 gwei as default if not cached.
    pub async fn get_gas_price_gwei(&self) -> f64 {
        let key = "gas_price:gwei";
        let result = if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            conn.get(key).await.unwrap_or(None)
        } else {
            if self.check_expired(key) {
                None
            } else {
                self.fallback.get(key).map(|v| v.clone())
            }
        };
        result
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(20.0)
    }

    // ── Opportunity deduplication ─────────────────────────────────────────────

    /// Mark an opportunity as seen. Returns true if this is the first time
    /// (SETNX semantics — set only if not exists).
    pub async fn mark_opportunity_seen(&self, opportunity_id: &str) -> Result<bool> {
        let key = format!("opportunity:{}", opportunity_id);
        if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            let was_set: bool = conn.set_nx(&key, "1")
                .await
                .with_context(|| format!("Failed to SETNX opportunity {}", key))?;

            if was_set {
                let _: () = conn.expire(&key, OPPORTUNITY_TTL_SECS as i64)
                    .await
                    .unwrap_or(());
            }
            Ok(was_set)
        } else {
            if self.check_expired(&key) || !self.fallback.contains_key(&key) {
                self.fallback.insert(key.clone(), "1".to_string());
                self.fallback_ttl.insert(key, std::time::Instant::now() + std::time::Duration::from_secs(OPPORTUNITY_TTL_SECS));
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    // ── Generic key/value operations ──────────────────────────────────────────

    /// Get a raw string value by key.
    pub async fn get_raw(&self, key: &str) -> Result<Option<String>> {
        if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            let val: Option<String> = conn.get(key)
                .await
                .with_context(|| format!("Failed to GET key {}", key))?;
            Ok(val)
        } else {
            if self.check_expired(key) {
                Ok(None)
            } else {
                Ok(self.fallback.get(key).map(|v| v.clone()))
            }
        }
    }

    /// Set a raw string value with TTL.
    pub async fn set_raw(&self, key: &str, value: &str, ttl_secs: usize) -> Result<()> {
        if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            conn.set_ex::<_, _, ()>(key, value, ttl_secs as u64)
                .await
                .with_context(|| format!("Failed to SET key {}", key))?;
        } else {
            self.fallback.insert(key.to_string(), value.to_string());
            self.fallback_ttl.insert(key.to_string(), std::time::Instant::now() + std::time::Duration::from_secs(ttl_secs as u64));
        }
        Ok(())
    }

    /// Health check — PING the Redis server.
    pub async fn ping(&self) -> Result<()> {
        if let Some(ref conn) = self.conn {
            let mut conn = conn.clone();
            let pong: String = redis::cmd("PING")
                .query_async(&mut conn)
                .await
                .context("Redis PING failed")?;
            if pong != "PONG" {
                anyhow::bail!("Unexpected PING response: {}", pong);
            }
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
