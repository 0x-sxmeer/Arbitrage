// executor/mod.rs — Flashbots bundle submission
use alloy::{
    primitives::{Address, Bytes, TxKind, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FlashbotsBundleParams {
    txs: Vec<String>,       // hex-encoded signed transactions
    block_number: String,   // target block number as hex
    min_timestamp: u64,     // minimum timestamp for inclusion
    max_timestamp: u64,     // maximum timestamp for inclusion
    reverting_tx_hashes: Vec<String>, // optional reverting hashes
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FlashbotsResponse {
    bundle_hash: Option<String>,
    error: Option<FlashbotsError>,
}

#[derive(Debug, Clone, Deserialize)]
struct FlashbotsError {
    message: String,
}

pub struct FlashbotsSubmitter {
    client: reqwest::Client,
    relay_url: String,
    signing_key: PrivateKeySigner,
    contract_address: Address,
    chain_id: u64,
}

impl FlashbotsSubmitter {
    pub fn new(
        relay_url: String,
        signing_key_hex: &str,
        contract_address: Address,
        chain_id: u64,
    ) -> Result<Self> {
        let signing_key: PrivateKeySigner = signing_key_hex.parse()
            .context("Invalid flashbots signing key")?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;
        Ok(Self {
            client,
            relay_url,
            signing_key,
            contract_address,
            chain_id,
        })
    }

    /// Sign and submit a bundle to Flashbots relay.
    /// `tx` is the unsigned raw transaction request.
    pub async fn submit_bundle(
        &self,
        tx: TransactionRequest,
        target_block: u64,
    ) -> Result<String> {
        let wallet = alloy::network::EthereumWallet::from(self.signing_key.clone());
        use alloy::network::TransactionBuilder;
        let envelope = tx.build(&wallet).await.context("Failed to sign transaction")?;
        
        use alloy::eips::eip2718::Encodable2718;
        let signed_bytes = envelope.encoded_2718();
        let raw_tx_hex = format!("0x{}", hex::encode(signed_bytes));

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let params = FlashbotsBundleParams {
            txs: vec![raw_tx_hex],
            block_number: format!("0x{:x}", target_block),
            min_timestamp: now,
            max_timestamp: now + 120, // 2 minute window
            reverting_tx_hashes: vec![],
        };

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendBundle",
            "params": [params]
        });

        let body_str = body.to_string();
        let signature_val = self.build_signature(&body).await?;

        let resp = self
            .client
            .post(&self.relay_url)
            .header("Content-Type", "application/json")
            .header("X-Flashbots-Signature", signature_val)
            .body(body_str)
            .send()
            .await
            .context("Failed to send Flashbots bundle")?;

        let fb_resp: FlashbotsResponse = resp
            .json()
            .await
            .context("Failed to parse Flashbots response")?;

        if let Some(err) = fb_resp.error {
            anyhow::bail!("Flashbots error: {}", err.message);
        }

        fb_resp
            .bundle_hash
            .ok_or_else(|| anyhow::anyhow!("No bundle hash returned"))
    }

    /// Submit raw, pre-signed transaction bytes as a bundle to Flashbots relay.
    pub async fn submit_raw_bundle(
        &self,
        bundle_txs: Vec<Vec<u8>>,
        target_block: u64,
    ) -> Result<String> {
        if bundle_txs.is_empty() {
            bail!("Cannot submit empty bundle");
        }

        let hex_txs: Vec<String> = bundle_txs.iter()
            .map(|tx| format!("0x{}", hex::encode(tx)))
            .collect();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let params = FlashbotsBundleParams {
            txs: hex_txs,
            block_number: format!("0x{:x}", target_block),
            min_timestamp: now,
            max_timestamp: now + 120, // 2 minute window
            reverting_tx_hashes: vec![],
        };

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendBundle",
            "params": [params]
        });

        let body_str = body.to_string();
        let signature_val = self.build_signature(&body).await?;

        let resp = self
            .client
            .post(&self.relay_url)
            .header("Content-Type", "application/json")
            .header("X-Flashbots-Signature", signature_val)
            .body(body_str)
            .send()
            .await
            .context("Failed to send Flashbots bundle")?;

        let fb_resp: FlashbotsResponse = resp
            .json()
            .await
            .context("Failed to parse Flashbots response")?;

        if let Some(err) = fb_resp.error {
            anyhow::bail!("Flashbots error: {}", err.message);
        }

        fb_resp
            .bundle_hash
            .ok_or_else(|| anyhow::anyhow!("No bundle hash returned"))
    }

    async fn build_signature(&self, body: &serde_json::Value) -> Result<String> {
        let body_str = body.to_string();
        let body_hash = alloy::primitives::keccak256(body_str.as_bytes());
        use alloy::signers::Signer;
        let signature = self.signing_key.sign_message(body_hash.as_slice()).await?;
        let sig_hex = format!("{}:{}", self.signing_key.address(), hex::encode(signature.as_bytes()));
        Ok(sig_hex)
    }

    /// Query bundle status from Flashbots
    pub async fn get_bundle_status(&self, bundle_hash: &str) -> Result<String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getBundleStats",
            "params": [{"bundleHash": bundle_hash, "blockNumber": "latest"}]
        });

        let resp = self
            .client
            .post(&self.relay_url)
            .json(&body)
            .send()
            .await
            .context("Failed to query bundle status")?;

        Ok(resp.text().await?)
    }
}
