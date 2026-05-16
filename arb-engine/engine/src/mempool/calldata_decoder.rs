use alloy::sol;
use alloy::primitives::{Address, U256};
use tracing::debug;

// ── Alloy-generated ABI types for Uniswap V3 Router methods ─────────────────
sol! {
    /// Interface for Uniswap V3 SwapRouter / SwapRouter02
    interface IUniswapV3Router {
        struct ExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 deadline;
            uint256 amountIn;
            uint256 amountOutMinimum;
            uint160 sqrtPriceLimitX96;
        }

        struct ExactInputParams {
            bytes path;
            address recipient;
            uint256 deadline;
            uint256 amountIn;
            uint256 amountOutMinimum;
        }

        function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
        function exactInput(ExactInputParams calldata params) external payable returns (uint256 amountOut);
        function multicall(uint256 deadline, bytes[] calldata data) external payable returns (bytes[] memory);
        function multicall(bytes[] calldata data) external payable returns (bytes[] memory);
    }
}

// ── Decoded swap result ──────────────────────────────────────────────────────

pub struct DecodedSwap {
    pub token_in: Address,
    pub token_out: Address,
    pub fee: u32,
    pub amount_in: U256,
}

// ── Decoder ──────────────────────────────────────────────────────────────────

/// Decodes Uniswap V3 Router calldata into swap parameters.
///
/// Handles:
///   - `exactInputSingle(...)` — single-hop swaps
///   - `exactInput(...)` — multi-hop swaps (extracts first hop)
///   - `multicall(...)` — unwraps and recursively decodes inner calls
pub fn decode_uniswap_v3_swap(input_data: &[u8]) -> Option<DecodedSwap> {
    if input_data.len() < 4 {
        return None;
    }

    use alloy::sol_types::SolCall;

    // ── 1. Try exactInputSingle (most common single-hop) ─────────────────
    if let Ok(call) = IUniswapV3Router::exactInputSingleCall::abi_decode(input_data, true) {
        debug!("Decoded exactInputSingle");
        return Some(DecodedSwap {
            token_in: call.params.tokenIn,
            token_out: call.params.tokenOut,
            fee: call.params.fee.to_string().parse::<u32>().unwrap_or(0),
            amount_in: call.params.amountIn,
        });
    }

    // ── 2. Try exactInput (multi-hop) ────────────────────────────────────
    if let Ok(call) = IUniswapV3Router::exactInputCall::abi_decode(input_data, true) {
        if let Some(swap) = decode_v3_path(&call.params.path, call.params.amountIn) {
            debug!("Decoded exactInput (multihop)");
            return Some(swap);
        }
    }

    // ── 3. Try multicall (Router02 wraps swaps inside multicall) ─────────
    if let Ok(call) = IUniswapV3Router::multicall_0Call::abi_decode(input_data, true) {
        for inner in &call.data {
            if let Some(swap) = decode_uniswap_v3_swap(inner) {
                debug!("Decoded swap inside multicall(deadline, data[])");
                return Some(swap);
            }
        }
    }

    if let Ok(call) = IUniswapV3Router::multicall_1Call::abi_decode(input_data, true) {
        for inner in &call.data {
            if let Some(swap) = decode_uniswap_v3_swap(inner) {
                debug!("Decoded swap inside multicall(data[])");
                return Some(swap);
            }
        }
    }

    None
}

/// Decode a Uniswap V3 encoded path: `[addr(20)][fee(3)][addr(20)][fee(3)]...`
///
/// Returns the first hop (token_in, token_out, fee) with the given amount_in.
fn decode_v3_path(path: &[u8], amount_in: U256) -> Option<DecodedSwap> {
    // Minimum path: 20 (addr) + 3 (fee) + 20 (addr) = 43 bytes
    if path.len() < 43 {
        return None;
    }

    let token_in = Address::from_slice(&path[0..20]);
    let fee = u32::from_be_bytes([0, path[20], path[21], path[22]]);
    let token_out = Address::from_slice(&path[23..43]);

    Some(DecodedSwap {
        token_in,
        token_out,
        fee,
        amount_in,
    })
}

// ── Router address constants ─────────────────────────────────────────────────

/// Uniswap V3 SwapRouter (original)
pub const UNISWAP_V3_ROUTER_V1:     &str = "0xe592427a0aece92de3edee1f18e0157c05861564";
/// Uniswap V3 SwapRouter02 (current default — wraps calls in multicall)
pub const UNISWAP_V3_ROUTER_V2:     &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
/// Uniswap Universal Router (command-based, Phase 2)
pub const UNISWAP_UNIVERSAL_ROUTER: &str = "0x3fc91a3afd70395cd496c647d5a6cc9d4b2b7fad";

/// Check if a contract address is one of the watched Uniswap routers.
pub fn is_uniswap_router(addr: &str) -> bool {
    let lower = addr.to_lowercase();
    lower == UNISWAP_V3_ROUTER_V1
        || lower == UNISWAP_V3_ROUTER_V2
        || lower == UNISWAP_UNIVERSAL_ROUTER
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_uniswap_router() {
        assert!(is_uniswap_router("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"));
        assert!(is_uniswap_router("0xe592427A0AEce92De3Edee1F18E0157C05861564"));
        assert!(is_uniswap_router("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD"));
        assert!(!is_uniswap_router("0x1234567890123456789012345678901234567890"));
    }

    #[test]
    fn test_decode_v3_path() {
        // Build a minimal path: WETH → (fee 3000) → USDC
        let mut path = Vec::new();
        path.extend_from_slice(&[0xC0, 0x2a, 0xaA, 0x39, 0xb2, 0x23, 0xFE, 0x8D, 0x0A, 0x0e,
                                  0x5C, 0x4F, 0x27, 0xeA, 0xD9, 0x08, 0x3C, 0x75, 0x6C, 0xc2]); // WETH
        path.extend_from_slice(&[0x00, 0x0B, 0xB8]); // fee = 3000
        path.extend_from_slice(&[0xA0, 0xb8, 0x69, 0x91, 0xc6, 0x21, 0x8b, 0x36, 0xc1, 0xd1,
                                  0x9D, 0x4a, 0x2e, 0x9E, 0xb0, 0xcE, 0x36, 0x06, 0xeB, 0x48]); // USDC

        let swap = decode_v3_path(&path, U256::from(1_000_000_000_000_000_000u128)).unwrap();
        assert_eq!(swap.fee, 3000);
    }
}
