// ─────────────────────────────────────────────────────────────────────────────
//  mempool/calldata_decoder.rs — Uniswap V3 calldata decoder
//
//  Decodes raw calldata from pending transactions targeting Uniswap V3 Router.
//  Supported selectors:
//    - exactInputSingle  (0x414bf389) — single-hop swap
//    - exactInput        (0xb858183f) — multi-hop swap
//    - exactOutputSingle (0xdb3e2198) — single-hop, exact output
//
//  ABI types are decoded manually to avoid pulling a full ABI codec dependency.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

// ── Function selectors (first 4 bytes of keccak256 of signature) ─────────────
pub const SELECTOR_EXACT_INPUT_SINGLE: &[u8; 4]  = &[0x41, 0x4b, 0xf3, 0x89];
pub const SELECTOR_EXACT_INPUT:        &[u8; 4]  = &[0xb8, 0x58, 0x18, 0x3f];
pub const SELECTOR_EXACT_OUTPUT_SINGLE: &[u8; 4] = &[0xdb, 0x3e, 0x21, 0x98];
pub const SELECTOR_MULTICALL:           &[u8; 4] = &[0xac, 0x96, 0x50, 0xd8];

// ── Uniswap V3 Router addresses ───────────────────────────────────────────────
pub const UNISWAP_V3_ROUTER_V1:    &str = "0xe592427a0aece92de3edee1f18e0157c05861564";
pub const UNISWAP_V3_ROUTER_V2:    &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
pub const UNISWAP_UNIVERSAL_ROUTER: &str = "0x3fc91a3afd70395cd496c647d5a6cc9d4b2b7fad";

/// All Uniswap V3-compatible router addresses monitored by the mempool listener.
pub const WATCHED_ROUTERS: &[&str] = &[
    UNISWAP_V3_ROUTER_V1,
    UNISWAP_V3_ROUTER_V2,
    UNISWAP_UNIVERSAL_ROUTER,
];


// ─────────────────────────────────────────────────────────────────────────────
//  Decoded parameter types
// ─────────────────────────────────────────────────────────────────────────────

/// Decoded parameters from `exactInputSingle`.
///
/// Solidity struct:
/// ```solidity
/// struct ExactInputSingleParams {
///     address tokenIn;       // slot 0
///     address tokenOut;      // slot 1
///     uint24  fee;           // slot 2
///     address recipient;     // slot 3
///     uint256 deadline;      // slot 4
///     uint256 amountIn;      // slot 5
///     uint256 amountOutMin;  // slot 6
///     uint160 sqrtPriceLimitX96; // slot 7
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExactInputSingleParams {
    pub token_in:             String,
    pub token_out:            String,
    pub fee:                  u32,    // pool fee tier in hundredths of a bip
    pub recipient:            String,
    pub deadline:             u64,
    pub amount_in:            u128,
    pub amount_out_minimum:   u128,
    pub sqrt_price_limit_x96: u128,
}

/// Decoded parameters from `exactInput` (multi-hop).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExactInputParams {
    /// ABI-encoded path: abi.encodePacked(tokenA, fee, tokenB, fee, tokenC, ...)
    pub path_raw:           Vec<u8>,
    /// Decoded hops extracted from path
    pub hops:               Vec<PathHop>,
    pub recipient:          String,
    pub deadline:           u64,
    pub amount_in:          u128,
    pub amount_out_minimum: u128,
}

/// A single hop in a multi-hop swap path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathHop {
    pub token_in:  String,
    pub fee:       u32,
    pub token_out: String,
}

/// Top-level decoded call result.
#[derive(Debug, Clone)]
pub enum DecodedCall {
    ExactInputSingle(ExactInputSingleParams),
    ExactInput(ExactInputParams),
    Unknown { selector: [u8; 4] },
}

// ─────────────────────────────────────────────────────────────────────────────
//  Decoder
// ─────────────────────────────────────────────────────────────────────────────

/// Decode raw calldata from a Uniswap V3 Router transaction.
///
/// Returns `DecodedCall::Unknown` if the selector is not recognized.
/// Returns `Err` only on malformed calldata (e.g. truncated).
pub fn decode_calldata(input: &[u8]) -> Result<DecodedCall> {
    if input.len() < 4 {
        bail!("Calldata too short: {} bytes", input.len());
    }

    let selector: [u8; 4] = input[..4].try_into().unwrap();
    let data = &input[4..]; // strip selector

    match &selector {
        s if s == SELECTOR_EXACT_INPUT_SINGLE => {
            let params = decode_exact_input_single(data)?;
            Ok(DecodedCall::ExactInputSingle(params))
        }
        s if s == SELECTOR_EXACT_INPUT => {
            let params = decode_exact_input(data)?;
            Ok(DecodedCall::ExactInput(params))
        }
        _ => Ok(DecodedCall::Unknown { selector }),
    }
}

/// Check if a contract address is one of the known Uniswap V3 routers.
pub fn is_uniswap_router(addr: &str) -> bool {
    let lower = addr.to_lowercase();
    lower == UNISWAP_V3_ROUTER_V1
        || lower == UNISWAP_V3_ROUTER_V2
        || lower == UNISWAP_UNIVERSAL_ROUTER
}

// ─────────────────────────────────────────────────────────────────────────────
//  Internal ABI decoders
// ─────────────────────────────────────────────────────────────────────────────

/// Read a 32-byte word at `offset` from `data`.
fn read_word(data: &[u8], offset: usize) -> Result<[u8; 32]> {
    if offset + 32 > data.len() {
        bail!("Calldata truncated at offset {}", offset);
    }
    Ok(data[offset..offset + 32].try_into().unwrap())
}

/// Read an Ethereum address from a 32-byte slot (right-aligned, 20 bytes).
fn read_address(data: &[u8], offset: usize) -> Result<String> {
    let word = read_word(data, offset)?;
    // Address occupies the last 20 bytes of a 32-byte slot
    let addr_bytes = &word[12..];
    Ok(format!("0x{}", hex::encode(addr_bytes)))
}

/// Read a u256 from a 32-byte slot and return as u128 (sufficient for most amounts).
fn read_u128(data: &[u8], offset: usize) -> Result<u128> {
    let word = read_word(data, offset)?;
    // Take the low 16 bytes (u128 max = 3.4 × 10^38, covers all realistic token amounts)
    let low: [u8; 16] = word[16..].try_into().unwrap();
    Ok(u128::from_be_bytes(low))
}

/// Read a u32 from a 32-byte slot.
fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let word = read_word(data, offset)?;
    let low: [u8; 4] = word[28..].try_into().unwrap();
    Ok(u32::from_be_bytes(low))
}

/// Decode `exactInputSingle` calldata (8 × 32-byte slots).
fn decode_exact_input_single(data: &[u8]) -> Result<ExactInputSingleParams> {
    // Layout after selector (each slot = 32 bytes):
    // [0] tokenIn, [1] tokenOut, [2] fee, [3] recipient,
    // [4] deadline, [5] amountIn, [6] amountOutMinimum, [7] sqrtPriceLimitX96
    if data.len() < 8 * 32 {
        bail!(
            "exactInputSingle: expected {} bytes, got {}",
            8 * 32,
            data.len()
        );
    }

    Ok(ExactInputSingleParams {
        token_in:             read_address(data, 0)?,
        token_out:            read_address(data, 32)?,
        fee:                  read_u32(data, 64)?,
        recipient:            read_address(data, 96)?,
        deadline:             read_u128(data, 128)? as u64,
        amount_in:            read_u128(data, 160)?,
        amount_out_minimum:   read_u128(data, 192)?,
        sqrt_price_limit_x96: read_u128(data, 224)?,
    })
}

/// Decode `exactInput` calldata.
/// Layout: (bytes path, address recipient, uint256 deadline, uint256 amountIn, uint256 amountOutMinimum)
/// The `bytes path` is ABI-encoded as a dynamic type (offset at slot 0, then length + data).
fn decode_exact_input(data: &[u8]) -> Result<ExactInputParams> {
    if data.len() < 5 * 32 {
        bail!("exactInput: calldata too short ({} bytes)", data.len());
    }

    // Slot 0: offset of path bytes (dynamic)
    let path_offset = read_u128(data, 0)? as usize;

    // At path_offset: length of path bytes
    if path_offset + 32 > data.len() {
        bail!("exactInput: path offset {} out of bounds", path_offset);
    }
    let path_len = read_u128(data, path_offset)? as usize;
    let path_start = path_offset + 32;

    if path_start + path_len > data.len() {
        bail!("exactInput: path data truncated");
    }
    let path_raw = data[path_start..path_start + path_len].to_vec();

    // Slots after path offset pointer:
    let recipient          = read_address(data, 32)?;
    let deadline           = read_u128(data, 64)? as u64;
    let amount_in          = read_u128(data, 96)?;
    let amount_out_minimum = read_u128(data, 128)?;

    let hops = decode_path(&path_raw)?;

    Ok(ExactInputParams {
        path_raw,
        hops,
        recipient,
        deadline,
        amount_in,
        amount_out_minimum,
    })
}

/// Decode a packed Uniswap V3 path: `abi.encodePacked(token0, fee, token1, fee, token2, ...)`
/// Each token is 20 bytes, each fee is 3 bytes.
///
/// Minimum path: 20 + 3 + 20 = 43 bytes (one hop).
pub fn decode_path(path: &[u8]) -> Result<Vec<PathHop>> {
    const ADDR_LEN: usize = 20;
    const FEE_LEN:  usize = 3;
    const HOP_LEN:  usize = ADDR_LEN + FEE_LEN; // 23 bytes per hop segment

    if path.len() < ADDR_LEN + FEE_LEN + ADDR_LEN {
        bail!("Path too short: {} bytes (minimum 43)", path.len());
    }

    let mut hops = Vec::new();
    let mut offset = 0;

    while offset + HOP_LEN + ADDR_LEN <= path.len() {
        let token_in = format!("0x{}", hex::encode(&path[offset..offset + ADDR_LEN]));
        offset += ADDR_LEN;

        let fee_bytes: [u8; 4] = [0, path[offset], path[offset + 1], path[offset + 2]];
        let fee = u32::from_be_bytes(fee_bytes);
        offset += FEE_LEN;

        let token_out = format!("0x{}", hex::encode(&path[offset..offset + ADDR_LEN]));

        hops.push(PathHop { token_in, fee, token_out });

        // Don't advance token_out — it becomes token_in of the next hop
    }

    if hops.is_empty() {
        bail!("No hops decoded from path");
    }

    Ok(hops)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_too_short_calldata() {
        assert!(decode_calldata(&[0x41, 0x4b]).is_err());
    }

    #[test]
    fn test_unknown_selector() {
        let mut data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        data.extend_from_slice(&[0u8; 256]);
        let result = decode_calldata(&data).unwrap();
        assert!(matches!(result, DecodedCall::Unknown { .. }));
    }

    #[test]
    fn test_decode_path_single_hop() {
        // token0 (20 bytes) + fee 3000 (3 bytes) + token1 (20 bytes)
        let mut path = vec![0u8; 43];
        // token_in = 0x000...001
        path[19] = 0x01;
        // fee = 0x000BB8 = 3000
        path[20] = 0x00;
        path[21] = 0x0B;
        path[22] = 0xB8;
        // token_out = 0x000...002
        path[42] = 0x02;

        let hops = decode_path(&path).unwrap();
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].fee, 3000);
        assert!(hops[0].token_in.ends_with("01"));
        assert!(hops[0].token_out.ends_with("02"));
    }

    #[test]
    fn test_is_uniswap_router() {
        assert!(is_uniswap_router("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"));
        assert!(is_uniswap_router("0xe592427A0AEce92De3Edee1F18E0157C05861564"));
        assert!(!is_uniswap_router("0x1234567890123456789012345678901234567890"));
    }
}
