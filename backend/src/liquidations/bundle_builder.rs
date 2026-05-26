// ─────────────────────────────────────────────────────────────────────────────
//  liquidations/bundle_builder.rs — Flashbots Bundle Composer
//
//  Assembles and submits Flashbots bundles for:
//    A) Backrun bundles: [victim_tx, backrun_tx]
//    B) Liquidation txs: single tx with flash loan + liquidation call
//
//  Bundles are signed with the Flashbots auth key and submitted to
//  https://relay.flashbots.net (or private builder endpoints).
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::backrun::BackrunOpportunity;
use super::liquidation_monitor::LiquidationOpportunity;

// ─────────────────────────────────────────────────────────────────────────────
//  Flashbots bundle types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FlashbotsBundle {
    pub txs: Vec<String>,     // hex-encoded signed txs
    pub block_number: String, // hex target block
    pub min_timestamp: Option<u64>,
    pub max_timestamp: Option<u64>,
    pub reverting_tx_hashes: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct FlashbotsRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'static str,
    pub params: Vec<serde_json::Value>,
}

#[derive(Deserialize, Debug)]
pub struct FlashbotsResponse {
    pub id: u64,
    pub result: Option<BundleStats>,
    pub error: Option<FlashbotsError>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BundleStats {
    pub bundle_hash: String,
    pub is_simulated: bool,
    pub is_sent_to_miners: bool,
    pub eth_sent_to_coinbase: String,
    pub coinbase_diff: String,
    pub total_gas_used: u64,
}

#[derive(Deserialize, Debug)]
pub struct FlashbotsError {
    pub code: i64,
    pub message: String,
}

// ─────────────────────────────────────────────────────────────────────────────
//  BundleBuilder
// ─────────────────────────────────────────────────────────────────────────────

pub struct BundleBuilder {
    /// Flashbots relay URL
    relay_url: String,
    /// Auth key (separate from execution wallet)
    auth_key: String,
    /// Execution wallet key (signs the actual txs)
    execution_key: String,
    /// AtomicArbV2 contract address
    contract_address: String,
    /// HTTP client
    client: reqwest::Client,
    /// Chain ID (8453 = Base)
    chain_id: u64,

    // Stats
    pub bundles_submitted: u64,
    pub bundles_included: u64,
    pub total_profit_eth: f64,
}

impl BundleBuilder {
    pub fn new(
        relay_url: String,
        auth_key: String,
        execution_key: String,
        contract_address: String,
        chain_id: u64,
    ) -> Self {
        Self {
            relay_url,
            auth_key,
            execution_key,
            contract_address,
            chain_id,
            client: reqwest::Client::new(),
            bundles_submitted: 0,
            bundles_included: 0,
            total_profit_eth: 0.0,
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    //  Backrun bundle: [victim_tx] + [our_backrun_tx]
    // ─────────────────────────────────────────────────────────────────────

    pub async fn submit_backrun_bundle(
        &mut self,
        opp: &BackrunOpportunity,
        target_block: u64,
        gas_price: u128,
    ) -> Result<String> {
        // Build the backrun calldata for AtomicArbV2
        let calldata = self.encode_backrun_calldata(opp, gas_price);

        // Sign our backrun tx
        let signed_our_tx = self
            .sign_tx(
                &self.contract_address.clone(),
                calldata,
                gas_price * (10_000 + opp.gas_premium_bps as u128) / 10_000,
                500_000, // gas limit
            )
            .await?;

        // Bundle: victim tx first, our tx second (strict ordering)
        let bundle = FlashbotsBundle {
            txs: vec![
                format!("0x{}", opp.target_tx_hash), // victim tx (raw signed)
                signed_our_tx,                       // our backrun tx
            ],
            block_number: format!("0x{:x}", target_block),
            min_timestamp: None,
            max_timestamp: Some(chrono::Utc::now().timestamp() as u64 + 30),
            reverting_tx_hashes: vec![], // we don't care if victim reverts
        };

        let bundle_hash = self.submit_bundle(bundle).await?;
        self.bundles_submitted += 1;

        info!(
            "📦 Backrun bundle submitted | hash={} | block={} | profit=${:.2}",
            &bundle_hash[..16],
            target_block,
            opp.expected_profit
        );

        Ok(bundle_hash)
    }

    // ─────────────────────────────────────────────────────────────────────
    //  Liquidation tx (single tx, no bundle needed for ordering)
    // ─────────────────────────────────────────────────────────────────────

    pub async fn submit_liquidation_tx(
        &mut self,
        opp: &LiquidationOpportunity,
        gas_price: u128,
    ) -> Result<String> {
        let calldata = self.encode_liquidation_calldata(opp);

        // Liquidations: submit to private RPC (Flashbots Protect) for MEV protection
        // and to avoid getting front-run by other liquidation bots
        let signed_tx = self
            .sign_tx(
                &opp.protocol_address.clone(),
                calldata,
                gas_price * 120 / 100, // 20% gas premium to land quickly
                800_000,
            )
            .await?;

        let tx_hash = self.send_private_tx(signed_tx).await?;

        info!(
            "💸 Liquidation submitted | borrower={} | net=${:.2} | tx={}",
            &opp.borrower[..8],
            opp.net_profit_usd,
            &tx_hash[..16]
        );

        Ok(tx_hash)
    }

    // ─────────────────────────────────────────────────────────────────────
    //  Internal helpers
    // ─────────────────────────────────────────────────────────────────────

    fn encode_backrun_calldata(&self, opp: &BackrunOpportunity, _gas_price: u128) -> Vec<u8> {
        // Encode call to AtomicArbV2.executeArbitrageV2(ArbParamsV2)
        // Selector: keccak256("executeArbitrageV2((address,uint256,bool,(address,uint8,uint24,address,address)[][],address[][],uint256,uint256))")[:4]
        // This is a simplified encoding — production would use alloy::sol! codegen
        let mut calldata = vec![0x12, 0x34, 0xab, 0xcd]; // placeholder selector
        calldata.extend_from_slice(&opp.optimal_amount.to_be_bytes());
        calldata
    }

    fn encode_liquidation_calldata(&self, opp: &LiquidationOpportunity) -> Vec<u8> {
        // Aave V3 liquidationCall(collateral, debt, user, debtToCover, receiveAToken)
        // selector: 0x00e8b4d5
        let mut calldata = vec![0x00, 0xe8, 0xb4, 0xd5];
        // In production: ABI-encode all params with alloy
        calldata.extend_from_slice(&[0u8; 32]); // collateral asset (padded address)
        calldata.extend_from_slice(&[0u8; 32]); // debt asset
        calldata.extend_from_slice(&[0u8; 32]); // user
        calldata.extend_from_slice(&opp.debt_repay_amount.to_be_bytes()); // amount (u128 → 16 bytes padded to 32)
        calldata.extend_from_slice(&[0u8; 32]); // receiveAToken = false
        calldata
    }

    async fn sign_tx(
        &self,
        _to: &str,
        _calldata: Vec<u8>,
        _gas_price: u128,
        _gas_limit: u64,
    ) -> Result<String> {
        // In production: use alloy::signers::LocalSigner + TransactionRequest
        // Returns hex-encoded signed transaction
        Ok("0x02f8...".to_string()) // placeholder
    }

    async fn submit_bundle(&self, bundle: FlashbotsBundle) -> Result<String> {
        let payload = FlashbotsRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "eth_sendBundle",
            params: vec![serde_json::to_value(&bundle)?],
        };

        // Sign with auth key (EIP-712 for Flashbots)
        let auth_header = self.sign_flashbots_header(&serde_json::to_string(&payload)?);

        let resp = self
            .client
            .post(&self.relay_url)
            .header("X-Flashbots-Signature", auth_header)
            .json(&payload)
            .send()
            .await?;

        let body: FlashbotsResponse = resp.json().await?;

        if let Some(err) = body.error {
            return Err(anyhow::anyhow!(
                "Flashbots error {}: {}",
                err.code,
                err.message
            ));
        }

        Ok(body.result.map(|r| r.bundle_hash).unwrap_or_default())
    }

    async fn send_private_tx(&self, signed_tx: String) -> Result<String> {
        // eth_sendPrivateTransaction via Flashbots Protect
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendPrivateTransaction",
            "params": [{
                "tx": signed_tx,
                "preferences": {
                    "fast": true,
                    "privacy": { "builders": ["default", "flashbots"] }
                }
            }]
        });

        let auth_header = self.sign_flashbots_header(&payload.to_string());

        let resp = self
            .client
            .post(&self.relay_url)
            .header("X-Flashbots-Signature", auth_header)
            .json(&payload)
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;
        Ok(body["result"].as_str().unwrap_or("").to_string())
    }

    fn sign_flashbots_header(&self, _body: &str) -> String {
        // In production: keccak256(body) → sign with auth_key → return "address:signature"
        format!("0x000...:{}", "0x000...") // placeholder
    }
}
