// ─────────────────────────────────────────────────────────────────────────────
//  chains/solana.rs — Solana SVM Chain Adapter
//
//  Fetches pool state from:
//    - Raydium CPMM / AMM v4 (constant product)
//    - Orca Whirlpool (concentrated liquidity, CLMM)
//
//  Uses solana-client for RPC calls. Helius RPC recommended for low latency.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use anyhow::{bail, Result};
use tracing::{debug, info, warn};
use std::str::FromStr;

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_sdk::pubkey::Pubkey;
use solana_client::rpc_config::RpcAccountInfoConfig;
use solana_account_decoder::UiAccountEncoding;
use futures_util::StreamExt;

use crate::pool::{Pool, PoolState, PoolType, U256};

// ── Well-known Solana program IDs ─────────────────────────────────────────────
pub const RAYDIUM_AMM_V4_PROGRAM:      &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
pub const RAYDIUM_CPMM_PROGRAM:        &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
pub const ORCA_WHIRLPOOL_PROGRAM:      &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

// ── Raydium AMM account layout (partial) ─────────────────────────────────────
const RAYDIUM_PC_AMOUNT_OFFSET:   usize = 258; // u64, quote token reserve
const RAYDIUM_COIN_AMOUNT_OFFSET: usize = 266; // u64, base token reserve

// ── Orca Whirlpool account layout (partial) ──────────────────────────────────
// Offset 65: sqrt_price (u128)
const ORCA_SQRT_PRICE_OFFSET: usize = 65;
// Offset 97: tick_current_index (i32)
const ORCA_TICK_OFFSET: usize = 97;
// Offset 101: liquidity (u128)
const ORCA_LIQUIDITY_OFFSET: usize = 101;

// ─────────────────────────────────────────────────────────────────────────────
//  Solana Adapter
// ─────────────────────────────────────────────────────────────────────────────

pub struct SolanaAdapter {
    rpc_url: String,
    ws_url: String,
    client: RpcClient,
}

impl SolanaAdapter {
    pub fn new(rpc_url: impl Into<String>, ws_url: impl Into<String>) -> Self {
        let rpc_url = rpc_url.into();
        let ws_url = ws_url.into();
        info!(rpc_url = %rpc_url, "Initializing Solana adapter");
        
        let client = RpcClient::new(rpc_url.clone());
        
        Self { rpc_url, ws_url, client }
    }

    /// Fetch pool state from a Solana pool (Raydium or Orca).
    pub async fn fetch_pool_state(&self, pool: &Pool) -> Result<PoolState> {
        debug!(pool_id = %pool.id, "Fetching Solana pool state");

        let pubkey = Pubkey::from_str(&pool.id)?;
        let account = self.client.get_account(&pubkey).await?;
        Self::parse_pool_state_from_data(&pool.pool_type, &account.data)
    }

    /// Parses Raydium or Orca pool state directly from the raw binary account data.
    pub fn parse_pool_state_from_data(pool_type: &PoolType, data: &[u8]) -> Result<PoolState> {
        match pool_type {
            PoolType::ConstantProduct | PoolType::StableSwap => {
                if data.len() < RAYDIUM_COIN_AMOUNT_OFFSET + 8 {
                    bail!("Raydium account data too short");
                }

                let reserve_a = u64::from_le_bytes(data[RAYDIUM_COIN_AMOUNT_OFFSET..RAYDIUM_COIN_AMOUNT_OFFSET+8].try_into()?);
                let reserve_b = u64::from_le_bytes(data[RAYDIUM_PC_AMOUNT_OFFSET..RAYDIUM_PC_AMOUNT_OFFSET+8].try_into()?);

                Ok(PoolState {
                    reserve_a:      U256::from(reserve_a),
                    reserve_b:      U256::from(reserve_b),
                    sqrt_price_x96: None,
                    tick:           None,
                    liquidity:      None,
                    amp_coeff:      None,
                })
            }
            PoolType::ConcentratedLiquidity => {
                if data.len() < ORCA_LIQUIDITY_OFFSET + 16 {
                    bail!("Orca Whirlpool account data too short");
                }

                let sqrt_price = u128::from_le_bytes(data[ORCA_SQRT_PRICE_OFFSET..ORCA_SQRT_PRICE_OFFSET+16].try_into()?);
                let tick = i32::from_le_bytes(data[ORCA_TICK_OFFSET..ORCA_TICK_OFFSET+4].try_into()?);
                let liquidity = u128::from_le_bytes(data[ORCA_LIQUIDITY_OFFSET..ORCA_LIQUIDITY_OFFSET+16].try_into()?);

                // Orca sqrt_price is in Q64.64 format, while Uniswap V3 is Q64.96.
                // We need to shift it left by 32 bits to match Q64.96 for our unified math engine.
                let sqrt_price_x96 = U256::from(sqrt_price) << 32;

                Ok(PoolState {
                    reserve_a:      U256::zero(),
                    reserve_b:      U256::zero(),
                    sqrt_price_x96: Some(sqrt_price_x96),
                    tick:           Some(tick),
                    liquidity:      Some(liquidity),
                    amp_coeff:      None,
                })
            }
        }
    }

    /// Subscribe to Raydium pool account changes via WebSocket.
    pub async fn subscribe_pool_updates(&self, pool_id: &str) -> Result<()> {
        let pool_pubkey = Pubkey::from_str(pool_id)?;
        let ws_url = self.ws_url.clone();
        
        info!(pool_id = %pool_id, "Subscribing to Solana pool updates via WS");
        
        // We spawn this task so it runs in the background.
        // In a complete implementation, this would send updates to the central event bus.
        tokio::spawn(async move {
            match PubsubClient::new(&ws_url).await {
                Ok(client) => {
                    let config = RpcAccountInfoConfig {
                        encoding: Some(UiAccountEncoding::Base64),
                        ..Default::default()
                    };
                    
                    match client.account_subscribe(&pool_pubkey, Some(config)).await {
                        Ok((mut sub, _unsub)) => {
                            info!("Successfully subscribed to pool {}", pool_pubkey);
                            while let Some(response) = sub.next().await {
                                // Here we would decode the new state and push it to the router
                                debug!("Received update for Solana pool {}", pool_pubkey);
                                let _data = response.value.data;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to subscribe to pool {}: {}", pool_pubkey, e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to connect Solana WS: {}", e);
                }
            }
        });

        Ok(())
    }

    /// Get current Solana slot (analogous to block number).
    pub async fn get_slot(&self) -> Result<u64> {
        let slot = self.client.get_slot().await?;
        Ok(slot)
    }

    /// Sends a list of signed, serialized transactions as a bundle to the Jito Block Engine.
    pub async fn send_jito_bundle(&self, transactions: Vec<Vec<u8>>, jito_url: &str) -> Result<String> {
        info!("Submitting bundle of {} transactions to Jito Block Engine...", transactions.len());
        
        let client = reqwest::Client::new();
        
        // Jito expects base58 encoded transactions
        let encoded_txs: Vec<String> = transactions
            .iter()
            .map(|tx| bs58::encode(tx).into_string())
            .collect();

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendBundle",
            "params": [encoded_txs]
        });

        let response = client
            .post(jito_url)
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            bail!("Jito Bundle execution failed: {}", error_text);
        }

        let result: serde_json::Value = response.json().await?;
        debug!("Jito response: {:?}", result);
        
        if let Some(err) = result.get("error") {
            bail!("Jito API Error: {:?}", err);
        }

        let bundle_id = result
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();

        info!("Jito Bundle submitted successfully! Bundle ID: {}", bundle_id);
        Ok(bundle_id)
    }

    /// Helper to construct a Jito tip transfer instruction
    pub fn get_jito_tip_instruction(&self, owner: &Pubkey, tip_lamports: u64) -> Result<solana_sdk::instruction::Instruction> {
        // Cw8CFyM99Hi4jrr45CnbC8jS4s291H388vaNs2JjhzgV (Jito Tip Account)
        let tip_pubkey = Pubkey::from_str("Cw8CFyM99Hi4jrr45CnbC8jS4s291H388vaNs2JjhzgV")?;
        
        let instruction = solana_sdk::system_instruction::transfer(
            owner,
            &tip_pubkey,
            tip_lamports
        );
        
        Ok(instruction)
    }
}

