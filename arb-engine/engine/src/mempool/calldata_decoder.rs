use alloy::sol;
use alloy::primitives::{Address, U256, Bytes};
use tracing::warn;

// Use Alloy's sol! macro to generate Rust types for the Uniswap V3 Router methods
sol! {
    /// Interface for Uniswap V3 SwapRouter02
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

        function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
    }
}

pub struct DecodedSwap {
    pub token_in: Address,
    pub token_out: Address,
    pub fee: u32,
    pub amount_in: U256,
}

/// Decodes transaction input data into usable swap parameters
pub fn decode_uniswap_v3_swap(input_data: &[u8]) -> Option<DecodedSwap> {
    if input_data.len() < 4 {
        return None;
    }

    // Attempt to decode the exactInputSingle call
    use alloy::sol_types::SolCall;
    
    if let Ok(call) = IUniswapV3Router::exactInputSingleCall::abi_decode(input_data, true) {
        return Some(DecodedSwap {
            token_in: call.params.tokenIn,
            token_out: call.params.tokenOut,
            fee: call.params.fee.to_string().parse::<u32>().unwrap_or(0),
            amount_in: call.params.amountIn,
        });
    }

    // TODO in Phase 2: Add decoders for exactInput (multihop) and V2 routers
    // warn!("Failed to decode recognized swap format or unsupported swap type.");
    None
}

// ── Uniswap V3 Router addresses ───────────────────────────────────────────────
pub const UNISWAP_V3_ROUTER_V1:    &str = "0xe592427a0aece92de3edee1f18e0157c05861564";
pub const UNISWAP_V3_ROUTER_V2:    &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
pub const UNISWAP_UNIVERSAL_ROUTER: &str = "0x3fc91a3afd70395cd496c647d5a6cc9d4b2b7fad";

/// Check if a contract address is one of the known Uniswap V3 routers.
pub fn is_uniswap_router(addr: &str) -> bool {
    let lower = addr.to_lowercase();
    lower == UNISWAP_V3_ROUTER_V1
        || lower == UNISWAP_V3_ROUTER_V2
        || lower == UNISWAP_UNIVERSAL_ROUTER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_uniswap_router() {
        assert!(is_uniswap_router("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"));
        assert!(is_uniswap_router("0xe592427A0AEce92De3Edee1F18E0157C05861564"));
        assert!(!is_uniswap_router("0x1234567890123456789012345678901234567890"));
    }
}
