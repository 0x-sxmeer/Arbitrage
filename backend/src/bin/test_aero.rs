use alloy::primitives::{Address, Signed};
use alloy::providers::ProviderBuilder;
use alloy::sol;
use anyhow::Result;
use std::str::FromStr;

sol! {
    #[sol(rpc)]
    interface IAerodromeFactory {
        function getPool(address tokenA, address tokenB, bool stable) external view returns (address pool);
    }

    #[sol(rpc)]
    interface ISlipstreamFactory {
        function getPool(address tokenA, address tokenB, int24 tickSpacing) external view returns (address pool);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let base_rpc_url = "https://base-rpc.publicnode.com";
    let provider = ProviderBuilder::new().on_builtin(base_rpc_url).await?;

    let weth = Address::from_str("0x4200000000000000000000000000000000000006").unwrap();
    let usdc = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();

    let aero_factory_addr =
        Address::from_str("0x420DD381b31aEf6683db6B902084cB0FFECe40Da").unwrap();
    let aero_factory = IAerodromeFactory::new(aero_factory_addr, &provider);

    match aero_factory.getPool(weth, usdc, false).call().await {
        Ok(res) => println!("Aerodrome V2 Volatile WETH/USDC Pool: {:?}", res.pool),
        Err(e) => println!("Failed Aerodrome V2 WETH/USDC: {:?}", e),
    }

    let slipstream_factory_addr =
        Address::from_str("0x5e7bb104d84c7cb9b682aac2f3d509f5f406809a").unwrap();
    let slipstream_factory = ISlipstreamFactory::new(slipstream_factory_addr, &provider);

    let ts = "50".parse::<Signed<24, 1>>().unwrap();
    match slipstream_factory.getPool(weth, usdc, ts).call().await {
        Ok(res) => println!(
            "Aerodrome Slipstream WETH/USDC Pool (ts=50): {:?}",
            res.pool
        ),
        Err(e) => println!("Failed Slipstream WETH/USDC: {:?}", e),
    }

    Ok(())
}
