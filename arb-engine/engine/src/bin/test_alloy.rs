use alloy::providers::ProviderBuilder;
use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::rpc::types::TransactionRequest;
use alloy::primitives::{Address, U256};
use std::str::FromStr;
use alloy::eips::eip2718::Encodable2718;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let signer = PrivateKeySigner::random();
    let wallet = EthereumWallet::from(signer.clone());
    let provider = ProviderBuilder::new().wallet(wallet.clone()).on_builtin("https://eth.public-rpc.com").await?;

    let tx_req = TransactionRequest::default()
        .to(Address::from_str("0x0000000000000000000000000000000000000000")?)
        .value(U256::from(100));

    let gas_price = (30.0 * 1e9) as u128;
    let tx_req = tx_req.with_gas_limit(350_000)
        .with_max_fee_per_gas(gas_price)
        .with_max_priority_fee_per_gas(gas_price);

    let tx_req = provider.fill(tx_req).await?;
    let built = tx_req.as_builder().unwrap().clone().build(&wallet).await?;
    let rlp_bytes = built.encoded_2718();
    println!("{:?}", rlp_bytes);
    
    Ok(())
}
