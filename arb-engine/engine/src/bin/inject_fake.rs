use redis::AsyncCommands;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = redis::Client::open("redis://127.0.0.1:6379")?;
    let mut con = client.get_multiplexed_tokio_connection().await?;
    
    // Create a fake mispriced pool: Token A -> Token B where Token B is super cheap
    // WETH (18) and USDC (6)
    let weth = "0x4200000000000000000000000000000000000006";
    let usdc = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
    
    let fake_pool = json!({
        "id": "fake_arb_pool_1",
        "chain": "Base",
        "dex": "UniswapV2",
        "token_a": { "address": weth, "symbol": "WETH", "decimals": 18 },
        "token_b": { "address": usdc, "symbol": "USDC", "decimals": 6 },
        "pool_type": "ConstantProduct",
        "fee_bps": 30,
        "state": {
            "reserve_a": "1000000000000000000000", // 1000 WETH
            "reserve_b": "500000000", // 500 USDC (Extremely mispriced! 1 WETH = 0.5 USDC)
            "sqrt_price_x96": null,
            "tick": null,
            "liquidity": null,
            "amp_coeff": null
        },
        "last_updated_block": 99999999,
        "last_updated_ts": chrono::Utc::now().timestamp()
    });

    let key = format!("pool:Base:{}:{}:30", weth, usdc);
    let _: () = con.set(&key, fake_pool.to_string()).await?;
    println!("Fake opportunity injected into Redis at key {}!", key);
    Ok(())
}
