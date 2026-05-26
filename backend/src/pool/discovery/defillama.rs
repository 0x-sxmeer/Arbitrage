use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

use crate::arb::router::LiquidityGraph;
use crate::db::postgres::PostgresStore;
use crate::pool::{ChainId, DexProtocol, Pool, PoolState, PoolType, Token};

#[derive(serde::Deserialize, Debug)]
struct DefiLlamaPool {
    pool: String,
    chain: String,
    project: String,
    symbol: String,
    #[serde(rename = "tvlUsd")]
    tvl_usd: Option<f64>,
    #[serde(rename = "volumeUsd1d")]
    volume_usd1d: Option<f64>,
    #[serde(rename = "underlyingTokens")]
    underlying_tokens: Option<Vec<String>>,
}

#[derive(serde::Deserialize, Debug)]
struct DefiLlamaResponse {
    data: Vec<DefiLlamaPool>,
}

pub async fn run_defillama_discovery(
    _graph: Arc<RwLock<LiquidityGraph>>,
    pg: Option<Arc<PostgresStore>>,
) {
    let client = reqwest::Client::new();
    let interval = std::time::Duration::from_secs(6 * 3600); // 6 hours

    loop {
        info!("Running DeFiLlama pool discovery...");
        match fetch_defillama_pools(&client).await {
            Ok(pools) => {
                info!("Discovered {} valid pools from DeFiLlama", pools.len());
                let mut added = 0;
                for p in pools {
                    if let Some(ref _pg_store) = pg {
                        // We could persist it, but for now we'll just log and let the main loop fetch state if we upsert
                        // In a real prod scenario we'd insert to DB, then fetch live state.
                    }
                    // Currently we don't have the live state, we just add it to the DB so it's picked up
                    // on the next restart or we can fetch state directly.
                    // Let's add it to DB if pg is enabled.
                    if let Some(ref pg_store) = pg {
                        let _ = pg_store.upsert_pool(&p).await;
                    }
                    added += 1;
                }
                if added > 0 {
                    info!("Registered {} new pools from DeFiLlama", added);
                }
            }
            Err(e) => {
                error!("DeFiLlama discovery failed: {}", e);
            }
        }

        tokio::time::sleep(interval).await;
    }
}

async fn fetch_defillama_pools(client: &reqwest::Client) -> anyhow::Result<Vec<Pool>> {
    let url = "https://yields.llama.fi/pools";
    let resp = client
        .get(url)
        .send()
        .await?
        .json::<DefiLlamaResponse>()
        .await?;

    let target_tokens = vec![
        "0x4200000000000000000000000000000000000006", // WETH
        "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913", // USDC
        "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf", // cbBTC
        "0x940181a94a35a4569e4529a3cdfb74e38fd98631", // AERO
    ];

    let mut result = Vec::new();
    for p in resp.data {
        if p.chain.to_lowercase() != "base" {
            continue;
        }
        let tvl = p.tvl_usd.unwrap_or(0.0);
        let vol = p.volume_usd1d.unwrap_or(0.0);

        if tvl < 50_000.0 || vol < 100_000.0 {
            continue;
        }

        if let Some(tokens) = p.underlying_tokens {
            if tokens.len() == 2 {
                let t0 = tokens[0].to_lowercase();
                let t1 = tokens[1].to_lowercase();
                if target_tokens.contains(&t0.as_str()) || target_tokens.contains(&t1.as_str()) {
                    let parts: Vec<&str> = p.symbol.split('-').collect();
                    let s0 = parts.get(0).unwrap_or(&"UNK").to_string();
                    let s1 = parts.get(1).unwrap_or(&"UNK").to_string();

                    let dex = match p.project.to_lowercase().as_str() {
                        "aerodrome" => DexProtocol::Aerodrome,
                        "aerodrome-slipstream" => DexProtocol::Aerodrome,
                        "uniswap-v3" => DexProtocol::UniswapV3,
                        _ => DexProtocol::UniswapV3,
                    };
                    let pool_type = match p.project.to_lowercase().as_str() {
                        "aerodrome" => PoolType::ConstantProduct,
                        _ => PoolType::ConcentratedLiquidity,
                    };

                    result.push(Pool {
                        id: p.pool.to_lowercase(),
                        chain: ChainId::Base,
                        dex,
                        token_a: Token {
                            address: t0,
                            symbol: s0,
                            decimals: 18,
                        }, // Approximations for discovery
                        token_b: Token {
                            address: t1,
                            symbol: s1,
                            decimals: 18,
                        },
                        pool_type,
                        state: PoolState::empty(),
                        fee_bps: 30, // generic fallback
                        last_updated_block: 0,
                        last_updated_ts: 0,
                    });
                }
            }
        }
    }
    Ok(result)
}
