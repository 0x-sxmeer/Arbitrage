// ─────────────────────────────────────────────────────────────────────────────
//  bin/seed_base_pools.rs — Hydrate pool_registry with top Base L2 pools v2.0
//
//  Fetches the top pools on Base from GeckoTerminal API, ranked by
//  24h volume, market cap, and trending metrics. Injects thousands of
//  high-liquidity pools directly into PostgreSQL in under 2 minutes.
//
//  Usage:
//    cargo run --bin seed-base-pools
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::Result;
use std::collections::HashMap;
use tracing::{error, info, warn};

#[derive(Clone, Debug)]
struct PoolSeed {
    address: String,
    chain: String,
    dex: String,
    token_a: (String, String), // (address, symbol)
    token_b: (String, String),
    fee: u32,
    pool_type: String,
}

/// Map GeckoTerminal DEX IDs to our internal engine DEX names.
/// Returns (dex_name, pool_type) or None if the DEX is unsupported.
/// Must match the string mapping in db/postgres.rs → PoolRegistryRow::to_pool()
fn map_dex(dex_id: &str) -> Option<(&'static str, &'static str)> {
    let id = dex_id.to_lowercase();

    // ── Uniswap ──
    if id.contains("uniswap") && (id.contains("v3") || id.contains("v4")) {
        return Some(("Uniswap V3", "ConcentratedLiquidity"));
    }
    if id.contains("uniswap") && id.contains("v2") {
        return Some(("Uniswap V2", "ConstantProduct"));
    }

    // ── PancakeSwap ──
    if id.contains("pancakeswap") {
        // PancakeSwap V3, Infinity CLMM — all concentrated liquidity
        return Some(("PancakeSwap V3", "ConcentratedLiquidity"));
    }

    // ── Aerodrome (must check slipstream BEFORE generic aerodrome) ──
    if id.contains("aerodrome") && id.contains("slipstream") {
        return Some(("Aerodrome", "ConcentratedLiquidity"));
    }
    if id.contains("aerodrome") {
        return Some(("Aerodrome V2", "ConstantProduct"));
    }

    // ── Curve ──
    if id.contains("curve") {
        return Some(("Curve", "StableSwap"));
    }

    // ── SushiSwap ──
    if id.contains("sushiswap") || id.contains("sushi") {
        return Some(("SushiSwap", "ConcentratedLiquidity"));
    }

    // Unsupported DEX — skip
    None
}

/// Parse fee bps from pool name like "PROS / USDC 0.01%"
fn parse_fee_from_name(name: &str) -> u32 {
    let n = name.to_lowercase();
    // Detect Aerodrome V2 Stable pools
    if n.contains("samm") || n.contains("stable") || n.contains("sweth") || n.contains("susdc") {
        return 1;
    }

    // Detect Slipstream tick spacing (e.g., "WETH / USDC 50")
    if let Some(space_idx) = n.rfind(' ') {
        let fee_str = &n[space_idx + 1..];
        if let Ok(ts) = fee_str.parse::<u32>() {
            if ts == 1 || ts == 50 || ts == 100 || ts == 200 || ts == 2000 {
                return ts;
            }
        }
    }

    if let Some(pct_idx) = name.rfind('%') {
        let before_pct = &name[..pct_idx];
        if let Some(space_idx) = before_pct.rfind(' ') {
            let fee_str = &before_pct[space_idx + 1..];
            if let Ok(fee_f) = fee_str.parse::<f64>() {
                return (fee_f * 100.0) as u32; // percentage → basis points
            }
        }
    }
    30 // Default 0.30% = 30 bps
}

/// Parse token symbols from pool name like "PROS / USDC 0.01%"
fn parse_symbols_from_name(name: &str) -> (String, String) {
    let parts: Vec<&str> = name.split(" / ").collect();
    if parts.len() >= 2 {
        let base = parts[0].trim().to_string();
        let quote_raw = parts[1].trim();
        // Remove fee percentage if present (e.g. "USDC 0.01%")
        let quote = quote_raw
            .split_whitespace()
            .next()
            .unwrap_or(quote_raw)
            .to_string();
        (base, quote)
    } else {
        ("UNKNOWN".to_string(), "UNKNOWN".to_string())
    }
}

fn extract_address(token_id: &str) -> String {
    let parts: Vec<&str> = token_id.split('_').collect();
    let raw = if parts.len() > 1 { parts[1] } else { parts[0] };
    get_normalized_address(raw)
}

fn get_normalized_address(raw_address: &str) -> String {
    use alloy::primitives::Address;
    use std::str::FromStr;
    match Address::from_str(raw_address) {
        Ok(addr) => addr.to_string().to_lowercase(),
        Err(_) => raw_address.to_lowercase(),
    }
}

/// Parse a single pool entry from the GeckoTerminal API JSON response.
fn parse_pool_from_api(pool_data: &serde_json::Value) -> Option<PoolSeed> {
    let attrs = pool_data.get("attributes")?;
    let rels = pool_data.get("relationships")?;

    // Pool address
    let address = get_normalized_address(attrs.get("address")?.as_str()?);

    // Pool name (for symbol + fee parsing)
    let name = attrs.get("name")?.as_str()?;

    // Reserve (TVL) — pre-filter garbage at the API level
    let reserve_usd: f64 = attrs
        .get("reserve_in_usd")
        .and_then(|r| r.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    // Skip pools with < $1,000 TVL (engine's $50k filter will do final cut)
    if reserve_usd < 1_000.0 {
        return None;
    }

    // DEX identification and mapping
    let dex_id = rels.get("dex")?.get("data")?.get("id")?.as_str()?;
    let (dex_name, pool_type) = map_dex(dex_id)?;

    // Token addresses from relationship IDs
    let base_token_id = rels.get("base_token")?.get("data")?.get("id")?.as_str()?;
    let quote_token_id = rels.get("quote_token")?.get("data")?.get("id")?.as_str()?;
    let base_addr = extract_address(base_token_id);
    let quote_addr = extract_address(quote_token_id);

    // Token symbols from pool name
    let (base_sym, quote_sym) = parse_symbols_from_name(name);

    // Fee from pool name
    let fee = parse_fee_from_name(name);

    Some(PoolSeed {
        address,
        chain: "base".to_string(),
        dex: dex_name.to_string(),
        token_a: (base_addr, base_sym),
        token_b: (quote_addr, quote_sym),
        fee,
        pool_type: pool_type.to_string(),
    })
}

/// Fetch pools from a paginated GeckoTerminal endpoint.
async fn fetch_pools_paginated(
    client: &reqwest::Client,
    base_url: &str,
    max_pages: u32,
    label: &str,
    all_pools: &mut HashMap<String, PoolSeed>,
) {
    let mut consecutive_rate_limits = 0u32;

    for page in 1..=max_pages {
        let url = format!("{}?page={}", base_url, page);

        // Retry loop for rate limiting
        let mut attempts = 0u32;
        let json_result = loop {
            attempts += 1;
            match client.get(&url).send().await {
                Ok(response) => {
                    if response.status().as_u16() == 429 {
                        consecutive_rate_limits += 1;
                        let backoff = std::cmp::min(3 + consecutive_rate_limits * 2, 15);
                        if attempts <= 3 {
                            warn!(
                                "  ⚠ Rate limited on {} page {} — retry in {}s (attempt {})",
                                label, page, backoff, attempts
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(backoff as u64))
                                .await;
                            continue;
                        } else {
                            warn!(
                                "  ⚠ {} page {} — rate limited after 3 retries, skipping",
                                label, page
                            );
                            break None;
                        }
                    }
                    consecutive_rate_limits = 0;

                    if !response.status().is_success() {
                        warn!("  ⚠ {} page {} — HTTP {}", label, page, response.status());
                        // 401 means we hit the free-tier pagination depth limit (max 10 pages).
                        // Hard break out of the entire `fetch_pools_paginated` loop.
                        return;
                    }

                    match response.json::<serde_json::Value>().await {
                        Ok(json) => break Some(json),
                        Err(_) => break None,
                    }
                }
                Err(e) => {
                    warn!("  ⚠ {} page {} — request failed: {}", label, page, e);
                    break None;
                }
            }
        };

        if let Some(json) = json_result {
            if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
                if data.is_empty() {
                    info!("  📄 {} page {} — no more data, stopping", label, page);
                    break;
                }

                let mut page_new = 0u32;
                for pool_data in data {
                    if let Some(seed) = parse_pool_from_api(pool_data) {
                        if !all_pools.contains_key(&seed.address) {
                            all_pools.insert(seed.address.clone(), seed);
                            page_new += 1;
                        }
                    }
                }

                info!(
                    "  📄 {} page {}/{} — +{} new (total: {})",
                    label,
                    page,
                    max_pages,
                    page_new,
                    all_pools.len()
                );
            } else {
                warn!("  ⚠ {} page {} — invalid JSON structure", label, page);
                break;
            }
        }

        // GeckoTerminal free tier: strict ~5 req/min → 12s between requests
        tokio::time::sleep(std::time::Duration::from_millis(12000)).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter("seed_base_pools=info,warn")
        .compact()
        .init();

    info!("═══════════════════════════════════════════════════════════════");
    info!("  🌱 Base L2 Dynamic Pool Discovery & Seeder v2.0");
    info!("  📡 Powered by GeckoTerminal API Pagination");
    info!("═══════════════════════════════════════════════════════════════");

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        error!("DATABASE_URL not set — cannot seed pools");
        std::process::exit(1);
    });

    // Connect to PostgreSQL
    let pool = sqlx::PgPool::connect(&database_url).await?;
    info!("✓ PostgreSQL connected");

    // Ensure pool_registry table exists
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pool_registry (
            pool_id      TEXT    PRIMARY KEY,
            chain        TEXT    NOT NULL,
            dex          TEXT    NOT NULL,
            token_a_addr TEXT    NOT NULL,
            token_a_sym  TEXT    NOT NULL,
            token_b_addr TEXT    NOT NULL,
            token_b_sym  TEXT    NOT NULL,
            fee_bps      INTEGER NOT NULL,
            pool_type    TEXT    NOT NULL,
            created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
    "#,
    )
    .execute(&pool)
    .await?;
    info!("✓ pool_registry table ready");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let mut all_pools: HashMap<String, PoolSeed> = HashMap::new();

    // ── 1. Top pools by volume (primary source — 10 pages × 20 = 200 pools) ──
    info!("📡 Phase 1: Fetching top Base pools ranked by volume...");
    fetch_pools_paginated(
        &client,
        "https://api.geckoterminal.com/api/v2/networks/base/pools",
        10,
        "TopPools",
        &mut all_pools,
    )
    .await;
    info!("  ✅ Top pools: {} unique pools so far", all_pools.len());

    // ── 2. Trending pools (hot momentum tokens) ──────────────────────────
    info!("📈 Phase 2: Fetching trending pools (high 1h/24h momentum)...");
    fetch_pools_paginated(
        &client,
        "https://api.geckoterminal.com/api/v2/networks/base/trending_pools",
        10,
        "Trending",
        &mut all_pools,
    )
    .await;
    info!("  ✅ After trending: {} unique pools", all_pools.len());

    // ── 3. Newly created high-volume pools ───────────────────────────────
    info!("🆕 Phase 3: Fetching newly created pools...");
    fetch_pools_paginated(
        &client,
        "https://api.geckoterminal.com/api/v2/networks/base/new_pools",
        5,
        "NewPools",
        &mut all_pools,
    )
    .await;
    info!("  ✅ After new pools: {} unique pools", all_pools.len());

    // ── 4. Count unique tokens across all discovered pools ───────────────
    let mut unique_tokens: HashMap<String, String> = HashMap::new();
    for (_, p) in &all_pools {
        unique_tokens
            .entry(p.token_a.0.clone())
            .or_insert(p.token_a.1.clone());
        unique_tokens
            .entry(p.token_b.0.clone())
            .or_insert(p.token_b.1.clone());
    }

    info!("═══════════════════════════════════════════════════════════════");
    info!("  📊 Discovery Summary:");
    info!("     Unique pools:  {}", all_pools.len());
    info!("     Unique tokens: {}", unique_tokens.len());
    info!("═══════════════════════════════════════════════════════════════");

    // ── 5. Seed into PostgreSQL ──────────────────────────────────────────
    info!("📦 Seeding {} pools into pool_registry...", all_pools.len());

    let mut inserted = 0u32;
    let mut skipped = 0u32;

    for (_, p) in &all_pools {
        let result = sqlx::query(r#"
            INSERT INTO pool_registry (pool_id, chain, dex, token_a_addr, token_a_sym, token_b_addr, token_b_sym, fee_bps, pool_type)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (pool_id) DO UPDATE
                SET dex = EXCLUDED.dex, token_a_sym = EXCLUDED.token_a_sym, token_b_sym = EXCLUDED.token_b_sym,
                    fee_bps = EXCLUDED.fee_bps, updated_at = NOW()
        "#)
        .bind(&p.address)
        .bind(&p.chain)
        .bind(&p.dex)
        .bind(&p.token_a.0)
        .bind(&p.token_a.1)
        .bind(&p.token_b.0)
        .bind(&p.token_b.1)
        .bind(p.fee as i32)
        .bind(&p.pool_type)
        .execute(&pool)
        .await;

        match result {
            Ok(_) => inserted += 1,
            Err(e) => {
                skipped += 1;
                warn!("  ⚠ {} — {}", p.address, e);
            }
        }
    }

    info!("═══════════════════════════════════════════════════════════════");
    info!("  ✅ Seeding complete!");
    info!("     Inserted/Updated: {}", inserted);
    info!("     Skipped (errors): {}", skipped);
    info!("     Unique tokens:    {}", unique_tokens.len());
    info!("═══════════════════════════════════════════════════════════════");

    Ok(())
}
