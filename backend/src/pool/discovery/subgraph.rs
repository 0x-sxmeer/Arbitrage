use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

use crate::arb::router::LiquidityGraph;
use crate::db::postgres::PostgresStore;
use crate::pool::{ChainId, DexProtocol, Pool, PoolState, PoolType, Token};

#[derive(serde::Deserialize, Debug)]
struct SubgraphResponse {
    data: SubgraphData,
}

#[derive(serde::Deserialize, Debug)]
struct SubgraphData {
    pools: Vec<SubgraphPool>,
}

#[derive(serde::Deserialize, Debug)]
struct SubgraphPool {
    id: String,
    token0: SubgraphToken,
    token1: SubgraphToken,
    #[serde(rename = "feeTier")]
    fee_tier: Option<String>,
    #[serde(rename = "volumeUSD")]
    volume_usd: String,
    #[serde(rename = "totalValueLockedUSD")]
    total_value_locked_usd: String,
}

#[derive(serde::Deserialize, Debug)]
struct SubgraphToken {
    id: String,
    symbol: String,
    decimals: String,
}

pub async fn run_subgraph_discovery(
    _graph: Arc<RwLock<LiquidityGraph>>,
    pg: Option<Arc<PostgresStore>>,
) {
    let client = reqwest::Client::new();
    let interval = std::time::Duration::from_secs(30 * 60); // 30 mins

    let endpoints = vec![
        (
            "https://api.studio.thegraph.com/query/48211/uniswap-v3-base/version/latest",
            DexProtocol::UniswapV3,
            PoolType::ConcentratedLiquidity,
        ),
        (
            "https://api.studio.thegraph.com/query/59052/aerodrome-base/version/latest",
            DexProtocol::Aerodrome,
            PoolType::ConcentratedLiquidity,
        ), // Generic aerodrome URL example
    ];

    loop {
        info!("Running The Graph pool discovery...");
        for (url, dex, p_type) in &endpoints {
            match fetch_subgraph_pools(&client, url, dex.clone(), p_type.clone()).await {
                Ok(pools) => {
                    info!(
                        "Discovered {} valid pools from Subgraph ({:?})",
                        pools.len(),
                        dex
                    );
                    let mut added = 0;
                    for p in pools {
                        if let Some(ref pg_store) = pg {
                            let _ = pg_store.upsert_pool(&p).await;
                        }
                        added += 1;
                    }
                    if added > 0 {
                        info!("Registered {} new pools from Subgraph ({:?})", added, dex);
                    }
                }
                Err(e) => {
                    error!("Subgraph discovery failed for {:?}: {}", dex, e);
                }
            }
        }

        tokio::time::sleep(interval).await;
    }
}

async fn fetch_subgraph_pools(
    client: &reqwest::Client,
    url: &str,
    dex: DexProtocol,
    pool_type: PoolType,
) -> anyhow::Result<Vec<Pool>> {
    let query = r#"{
        pools(first: 50, orderBy: volumeUSD, orderDirection: desc, where: { volumeUSD_gt: "10000", totalValueLockedUSD_gt: "10000" }) {
            id
            token0 { id symbol decimals }
            token1 { id symbol decimals }
            feeTier
            volumeUSD
            totalValueLockedUSD
        }
    }"#;

    let body = serde_json::json!({
        "query": query
    });

    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await?
        .json::<SubgraphResponse>()
        .await?;

    let target_tokens = vec![
        "0x4200000000000000000000000000000000000006", // WETH
        "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913", // USDC
        "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf", // cbBTC
        "0x940181a94a35a4569e4529a3cdfb74e38fd98631", // AERO
    ];

    let mut result = Vec::new();
    for p in resp.data.pools {
        let t0 = p.token0.id.to_lowercase();
        let t1 = p.token1.id.to_lowercase();

        if target_tokens.contains(&t0.as_str()) || target_tokens.contains(&t1.as_str()) {
            let fee_bps = if let Some(fee_tier) = p.fee_tier {
                if let Ok(fee) = fee_tier.parse::<u32>() {
                    fee / 100
                } else {
                    30
                }
            } else {
                30
            };

            result.push(Pool {
                id: p.id.to_lowercase(),
                chain: ChainId::Base,
                dex: dex.clone(),
                token_a: Token {
                    address: t0,
                    symbol: p.token0.symbol,
                    decimals: p.token0.decimals.parse().unwrap_or(18),
                },
                token_b: Token {
                    address: t1,
                    symbol: p.token1.symbol,
                    decimals: p.token1.decimals.parse().unwrap_or(18),
                },
                pool_type: pool_type.clone(),
                state: PoolState::empty(),
                fee_bps,
                last_updated_block: 0,
                last_updated_ts: 0,
            });
        }
    }
    Ok(result)
}
