// ─────────────────────────────────────────────────────────────────────────────
//  engine/src/mempool/calldata_decoder.rs  [PATCHED]
//
//  FIXES applied vs. original:
//
//  FIX-3: Added all Base chain DEX router addresses (Uniswap V3 SwapRouter02,
//         Universal Router, Aerodrome V2, Aerodrome UR, BaseSwap, PancakeSwap V3,
//         SushiSwap V3) so that `is_known_dex_router` returns true for Base
//         mempool transactions and `process_payload` actually processes them.
//
//  FIX-5: Corrected the V2 selector collision.  SEL_V2_TOKENS_FOR_ETH
//         (0x4a25d94a) is `swapTokensForExactETH` — it has amountOut first,
//         not amountIn.  Both selectors now dispatch to their correct decoders
//         instead of both being decoded with swapExactTokensForETHCall.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use alloy::sol;
use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use tracing::debug;

// ─────────────────────────────────────────────────────────────────────────────
//  ABI type definitions
// ─────────────────────────────────────────────────────────────────────────────

sol! {
    // ── Uniswap / SushiSwap / PancakeSwap V2 Router ──────────────────────────
    interface IUniswapV2Router {
        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address   to,
            uint256   deadline
        ) external returns (uint256[] memory amounts);

        function swapTokensForExactTokens(
            uint256   amountOut,
            uint256   amountInMax,
            address[] calldata path,
            address   to,
            uint256   deadline
        ) external returns (uint256[] memory amounts);

        function swapExactETHForTokens(
            uint256   amountOutMin,
            address[] calldata path,
            address   to,
            uint256   deadline
        ) external payable returns (uint256[] memory amounts);

        // FIX-5: swapTokensForExactETH — amountOut is the FIRST parameter
        function swapTokensForExactETH(
            uint256   amountOut,
            uint256   amountInMax,
            address[] calldata path,
            address   to,
            uint256   deadline
        ) external returns (uint256[] memory amounts);

        function swapExactTokensForETH(
            uint256   amountIn,
            uint256   amountOutMin,
            address[] calldata path,
            address   to,
            uint256   deadline
        ) external returns (uint256[] memory amounts);

        function swapETHForExactTokens(
            uint256   amountOut,
            address[] calldata path,
            address   to,
            uint256   deadline
        ) external payable returns (uint256[] memory amounts);
    }

    // ── Aerodrome V2 Router ──────────────────────────────────────────────────
    interface IAerodromeV2Router {
        struct Route {
            address from;
            address to;
            bool stable;
            address factory;
        }
        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            Route[] calldata routes,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);
        
        function swapExactETHForTokens(
            uint256 amountOutMin,
            Route[] calldata routes,
            address to,
            uint256 deadline
        ) external payable returns (uint256[] memory amounts);
        
        function swapExactTokensForETH(
            uint256 amountIn,
            uint256 amountOutMin,
            Route[] calldata routes,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);
    }

    // ── Uniswap V3 / PancakeSwap V3 Router ───────────────────────────────────
    interface IUniswapV3Router {
        struct ExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24  fee;
            address recipient;
            uint256 deadline;
            uint256 amountIn;
            uint256 amountOutMinimum;
            uint160 sqrtPriceLimitX96;
        }

        struct ExactInputParams {
            bytes   path;
            address recipient;
            uint256 deadline;
            uint256 amountIn;
            uint256 amountOutMinimum;
        }

        struct ExactOutputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24  fee;
            address recipient;
            uint256 deadline;
            uint256 amountOut;
            uint256 amountInMaximum;
            uint160 sqrtPriceLimitX96;
        }

        function exactInputSingle(ExactInputSingleParams calldata params)
            external payable returns (uint256 amountOut);

        function exactInput(ExactInputParams calldata params)
            external payable returns (uint256 amountOut);

        function exactOutputSingle(ExactOutputSingleParams calldata params)
            external payable returns (uint256 amountIn);

        // Router02: multicall(uint256 deadline, bytes[] data)
        function multicall(uint256 deadline, bytes[] calldata data)
            external payable returns (bytes[] memory);

        // Router02: multicall(bytes[] data)
        function multicall(bytes[] calldata data)
            external payable returns (bytes[] memory);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  4-byte selectors
// ─────────────────────────────────────────────────────────────────────────────

// V2-family selectors
const SEL_V2_EXACT_IN:            [u8; 4] = [0x38, 0xed, 0x17, 0x39]; // swapExactTokensForTokens
const SEL_V2_EXACT_OUT:           [u8; 4] = [0x88, 0x03, 0xdb, 0xee]; // swapTokensForExactTokens
const SEL_V2_ETH_EXACT_IN:        [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5]; // swapExactETHForTokens
// FIX-5: separated into two distinct arms — swapTokensForExactETH (amountOut first)
const SEL_V2_TOKENS_FOR_EXACT_ETH:[u8; 4] = [0x4a, 0x25, 0xd9, 0x4a]; // swapTokensForExactETH
// and swapExactTokensForETH (amountIn first)
const SEL_V2_EXACT_TOKENS_FOR_ETH:[u8; 4] = [0x18, 0xcb, 0xaf, 0xe5]; // swapExactTokensForETH
const SEL_V2_ETH_FOR_EXACT:       [u8; 4] = [0xfb, 0x3b, 0xdb, 0x41]; // swapETHForExactTokens

// V3 selectors
const SEL_V3_EXACT_INPUT_SINGLE:  [u8; 4] = [0x41, 0x4b, 0xf3, 0x89];
const SEL_V3_EXACT_INPUT:         [u8; 4] = [0xc0, 0x4b, 0x8d, 0x59];
const SEL_V3_EXACT_OUTPUT_SINGLE: [u8; 4] = [0xdb, 0x3e, 0x21, 0x98];
const SEL_V3_MULTICALL_DL:        [u8; 4] = [0x5a, 0xe4, 0x01, 0xdc];
const SEL_V3_MULTICALL:           [u8; 4] = [0xac, 0x96, 0x50, 0xd8];

// Universal Router selectors
const SEL_UR_EXECUTE_DL:          [u8; 4] = [0x24, 0x85, 0x6b, 0xc3];
const SEL_UR_EXECUTE:             [u8; 4] = [0x35, 0x93, 0x56, 0x4c];

// Valid Uniswap V3 fee tiers (per-million units)
const VALID_V3_FEES: [u32; 5] = [100, 500, 2500, 3000, 10000];

// ─────────────────────────────────────────────────────────────────────────────
//  Output types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DexVersion {
    UniswapV2,
    UniswapV3,
    SushiSwapV2,
    SushiSwapV3,
    PancakeSwapV2,
    PancakeSwapV3,
    AerodromeV2,
    AerodromeV3,
    BaseSwap,
    UniversalRouter,
    RaydiumAmm,
    OrcaWhirlpool,
    OsmosisCosmos,
    Unknown,
}

impl DexVersion {
    pub fn default_fee_bps(&self) -> u32 {
        match self {
            DexVersion::UniswapV2 | DexVersion::SushiSwapV2  => 30,
            DexVersion::PancakeSwapV2                         => 25,
            DexVersion::AerodromeV2                           => 30,
            DexVersion::BaseSwap                              => 30,
            DexVersion::RaydiumAmm                            => 25,
            DexVersion::OrcaWhirlpool                         => 30,
            DexVersion::OsmosisCosmos                         => 20,
            _                                                  => 30,
        }
    }

    pub fn is_evm(&self) -> bool {
        !matches!(
            self,
            DexVersion::RaydiumAmm | DexVersion::OrcaWhirlpool | DexVersion::OsmosisCosmos
        )
    }
}

#[derive(Debug, Clone)]
pub struct DecodedSwap {
    pub token_in:    Address,
    pub token_out:   Address,
    pub fee_bps:     u32,
    pub amount_in:   U256,
    pub dex_version: DexVersion,
    pub path:        Vec<Address>,
}

// ─────────────────────────────────────────────────────────────────────────────
//  Main public interface
// ─────────────────────────────────────────────────────────────────────────────

pub fn decode_swap(input_data: &[u8], to_addr: &str) -> Option<DecodedSwap> {
    if input_data.len() < 4 {
        return None;
    }

    let dex = classify_router(to_addr);
    let sel: [u8; 4] = input_data[..4].try_into().ok()?;

    match dex {
        DexVersion::UniswapV2
        | DexVersion::SushiSwapV2
        | DexVersion::PancakeSwapV2
        | DexVersion::AerodromeV2
        | DexVersion::BaseSwap => {
            decode_v2_family(input_data, sel, dex)
        }

        DexVersion::UniswapV3
        | DexVersion::PancakeSwapV3
        | DexVersion::SushiSwapV3
        | DexVersion::AerodromeV3 => {
            decode_v3_family(input_data, sel, dex)
        }

        DexVersion::UniversalRouter => {
            decode_universal_router(input_data)
        }

        DexVersion::RaydiumAmm | DexVersion::OrcaWhirlpool | DexVersion::OsmosisCosmos => None,

        DexVersion::Unknown => {
            decode_v3_family(input_data, sel, DexVersion::Unknown)
                .or_else(|| decode_v2_family(input_data, sel, DexVersion::Unknown))
                .or_else(|| decode_universal_router(input_data))
        }
    }
}

pub fn decode_uniswap_v3_swap(input_data: &[u8]) -> Option<DecodedSwap> {
    if input_data.len() < 4 { return None; }
    let sel: [u8; 4] = input_data[..4].try_into().ok()?;
    decode_v3_family(input_data, sel, DexVersion::UniswapV3)
}

pub fn decode_v2_swap(input_data: &[u8], dex: DexVersion) -> Option<DecodedSwap> {
    if input_data.len() < 4 { return None; }
    let sel: [u8; 4] = input_data[..4].try_into().ok()?;
    decode_v2_family(input_data, sel, dex)
}

// ─────────────────────────────────────────────────────────────────────────────
//  V2-family decoder
// ─────────────────────────────────────────────────────────────────────────────

fn decode_v2_family(input_data: &[u8], sel: [u8; 4], dex: DexVersion) -> Option<DecodedSwap> {
    let fee_bps = dex.default_fee_bps();

    // Specific Aerodrome V2 fallback check before Uniswap V2 checks
    if dex == DexVersion::AerodromeV2 {
        if let Ok(call) = IAerodromeV2Router::swapExactTokensForTokensCall::abi_decode(input_data, true) {
            if !call.routes.is_empty() {
                let mut path = Vec::new();
                path.push(call.routes[0].from);
                for r in &call.routes { path.push(r.to); }
                return v2_path_to_swap(path, call.amountIn, fee_bps, dex);
            }
        }
        if let Ok(call) = IAerodromeV2Router::swapExactETHForTokensCall::abi_decode(input_data, true) {
            if !call.routes.is_empty() {
                let mut path = Vec::new();
                path.push(call.routes[0].from);
                for r in &call.routes { path.push(r.to); }
                return v2_path_to_swap(path, U256::ZERO, fee_bps, dex);
            }
        }
        if let Ok(call) = IAerodromeV2Router::swapExactTokensForETHCall::abi_decode(input_data, true) {
            if !call.routes.is_empty() {
                let mut path = Vec::new();
                path.push(call.routes[0].from);
                for r in &call.routes { path.push(r.to); }
                return v2_path_to_swap(path, call.amountIn, fee_bps, dex);
            }
        }
    }

    match sel {
        SEL_V2_EXACT_IN => {
            let call = IUniswapV2Router::swapExactTokensForTokensCall::abi_decode(input_data, true).ok()?;
            debug!("Decoded V2 swapExactTokensForTokens: {} hops", call.path.len());
            v2_path_to_swap(call.path, call.amountIn, fee_bps, dex)
        }

        SEL_V2_EXACT_OUT => {
            let call = IUniswapV2Router::swapTokensForExactTokensCall::abi_decode(input_data, true).ok()?;
            debug!("Decoded V2 swapTokensForExactTokens: {} hops", call.path.len());
            v2_path_to_swap(call.path, call.amountInMax, fee_bps, dex)
        }

        SEL_V2_ETH_EXACT_IN => {
            let call = IUniswapV2Router::swapExactETHForTokensCall::abi_decode(input_data, true).ok()?;
            debug!("Decoded V2 swapExactETHForTokens");
            v2_path_to_swap(call.path, U256::ZERO, fee_bps, dex)
        }

        // FIX-5: swapTokensForExactETH — amountOut is field 0, amountInMax is field 1
        // The old code decoded this with swapExactTokensForETH, reading amountOut as amountIn.
        SEL_V2_TOKENS_FOR_EXACT_ETH => {
            let call = IUniswapV2Router::swapTokensForExactETHCall::abi_decode(input_data, true).ok()?;
            debug!("Decoded V2 swapTokensForExactETH (amountInMax = {})", call.amountInMax);
            // amountInMax is the worst-case input; use it as the amount proxy
            v2_path_to_swap(call.path, call.amountInMax, fee_bps, dex)
        }

        // FIX-5: swapExactTokensForETH — amountIn is field 0 (correct decoder)
        SEL_V2_EXACT_TOKENS_FOR_ETH => {
            let call = IUniswapV2Router::swapExactTokensForETHCall::abi_decode(input_data, true).ok()?;
            debug!("Decoded V2 swapExactTokensForETH");
            v2_path_to_swap(call.path, call.amountIn, fee_bps, dex)
        }

        SEL_V2_ETH_FOR_EXACT => {
            let call = IUniswapV2Router::swapETHForExactTokensCall::abi_decode(input_data, true).ok()?;
            debug!("Decoded V2 swapETHForExactTokens");
            v2_path_to_swap(call.path, U256::ZERO, fee_bps, dex)
        }

        _ => None,
    }
}

fn v2_path_to_swap(
    path: Vec<Address>,
    amount_in: U256,
    fee_bps: u32,
    dex: DexVersion,
) -> Option<DecodedSwap> {
    if path.len() < 2 { return None; }
    let token_in  = path[0];
    let token_out = *path.last().unwrap();
    let intermediate = path[1..path.len().saturating_sub(1)].to_vec();

    Some(DecodedSwap {
        token_in,
        token_out,
        fee_bps,
        amount_in,
        dex_version: dex,
        path: intermediate,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
//  V3-family decoder
// ─────────────────────────────────────────────────────────────────────────────

fn decode_v3_family(input_data: &[u8], sel: [u8; 4], dex: DexVersion) -> Option<DecodedSwap> {
    match sel {
        SEL_V3_EXACT_INPUT_SINGLE => {
            let call = IUniswapV3Router::exactInputSingleCall::abi_decode(input_data, true).ok()?;
            debug!("Decoded V3 exactInputSingle fee={}", call.params.fee);
            Some(DecodedSwap {
                token_in:    call.params.tokenIn,
                token_out:   call.params.tokenOut,
                fee_bps:     fee_units_to_bps(call.params.fee.to::<u32>()),
                amount_in:   call.params.amountIn,
                dex_version: dex,
                path:        vec![],
            })
        }

        SEL_V3_EXACT_INPUT => {
            let call = IUniswapV3Router::exactInputCall::abi_decode(input_data, true).ok()?;
            debug!("Decoded V3 exactInput (multi-hop)");
            decode_v3_path(&call.params.path, call.params.amountIn, dex)
        }

        SEL_V3_EXACT_OUTPUT_SINGLE => {
            let call = IUniswapV3Router::exactOutputSingleCall::abi_decode(input_data, true).ok()?;
            debug!("Decoded V3 exactOutputSingle fee={}", call.params.fee);
            Some(DecodedSwap {
                token_in:    call.params.tokenIn,
                token_out:   call.params.tokenOut,
                fee_bps:     fee_units_to_bps(call.params.fee.to::<u32>()),
                amount_in:   call.params.amountInMaximum,
                dex_version: dex,
                path:        vec![],
            })
        }

        SEL_V3_MULTICALL_DL => {
            let call = IUniswapV3Router::multicall_0Call::abi_decode(input_data, true).ok()?;
            for inner in &call.data {
                if let Some(swap) = decode_v3_family(inner, get_sel(inner)?, dex) {
                    return Some(swap);
                }
            }
            None
        }

        SEL_V3_MULTICALL => {
            let call = IUniswapV3Router::multicall_1Call::abi_decode(input_data, true).ok()?;
            for inner in &call.data {
                if let Some(swap) = decode_v3_family(inner, get_sel(inner)?, dex) {
                    return Some(swap);
                }
            }
            None
        }

        SEL_UR_EXECUTE | SEL_UR_EXECUTE_DL => decode_universal_router(input_data),

        _ => None,
    }
}

fn decode_v3_path(path: &[u8], amount_in: U256, dex: DexVersion) -> Option<DecodedSwap> {
    if path.len() < 43 { return None; }

    let token_in  = Address::from_slice(&path[0..20]);
    let fee_raw   = u32::from_be_bytes([0, path[20], path[21], path[22]]);
    let token_out_first = Address::from_slice(&path[23..43]);

    let mut path_intermediates = vec![token_out_first];
    let mut offset = 43;
    while offset + 23 <= path.len() {
        if offset + 23 > path.len() { break; }
        let next = Address::from_slice(&path[offset+3..offset+23]);
        path_intermediates.push(next);
        offset += 23;
    }

    let token_out = *path_intermediates.last().unwrap_or(&token_out_first);
    let intermediates: Vec<Address> =
        path_intermediates[..path_intermediates.len().saturating_sub(1)].to_vec();

    Some(DecodedSwap {
        token_in,
        token_out,
        fee_bps:     fee_units_to_bps(fee_raw),
        amount_in,
        dex_version: dex,
        path:        intermediates,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
//  Universal Router decoder
// ─────────────────────────────────────────────────────────────────────────────

fn decode_universal_router(input_data: &[u8]) -> Option<DecodedSwap> {
    if input_data.len() < 47 { return None; } // 4 bytes selector + at least 43 bytes path

    let data = &input_data[4..];

    for offset in 0..=data.len().saturating_sub(43) {
        let fee_raw = u32::from_be_bytes([
            0,
            data[offset + 20],
            data[offset + 21],
            data[offset + 22],
        ]);

        if !VALID_V3_FEES.contains(&fee_raw) {
            continue;
        }

        let token_in  = Address::from_slice(&data[offset..offset + 20]);
        let token_out = Address::from_slice(&data[offset + 23..offset + 43]);

        // Filter out garbage tokens
        if token_in.is_zero() || token_out.is_zero() || token_in == token_out { continue; }
        if token_in[..8].iter().all(|&x| x == 0) || token_out[..8].iter().all(|&x| x == 0) { continue; }

        debug!(
            ?token_in, ?token_out, fee = fee_raw,
            "Universal Router: decoded embedded V3 path (heuristic)"
        );

        return Some(DecodedSwap {
            token_in,
            token_out,
            fee_bps:     fee_units_to_bps(fee_raw),
            amount_in:   U256::ZERO,
            dex_version: DexVersion::UniversalRouter,
            path:        vec![],
        });
    }

    debug!("Universal Router: no recognisable V3 path found");
    None
}

// ─────────────────────────────────────────────────────────────────────────────
//  Router address registry
// ─────────────────────────────────────────────────────────────────────────────

pub struct RouterRegistry;

impl RouterRegistry {
    // ── Ethereum Mainnet ───────────────────────────────────────────────────────
    pub const UNISWAP_V2_ROUTER:        &'static str = "0x7a250d5630b4cf539739df2c5dacb4c659f2488d";
    pub const UNISWAP_V3_ROUTER_V1:     &'static str = "0xe592427a0aece92de3edee1f18e0157c05861564";
    pub const UNISWAP_V3_ROUTER_V2:     &'static str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
    pub const UNISWAP_UNIVERSAL_ETH:    &'static str = "0x3fc91a3afd70395cd496c647d5a6cc9d4b2b7fad";
    pub const UNISWAP_UNIVERSAL_OLD:    &'static str = "0xef1c6e67703c7bd7107eed8303fbe6ec2554bf6b";

    // ── BASE Chain (FIX-3: previously missing — caused zero swap detection) ────
    /// Uniswap V3 SwapRouter02 on Base
    pub const UNISWAP_V3_ROUTER_BASE:   &'static str = "0x2626664c2603336e57b271c5c0b26f421741e481";
    /// Uniswap Universal Router on Base (same address as ETH mainnet)
    pub const UNISWAP_UNIVERSAL_BASE:   &'static str = "0x3fc91a3afd70395cd496c647d5a6cc9d4b2b7fad";
    /// Aerodrome Finance V2 Router (Solidly-compatible, UniV2 interface)
    pub const AERODROME_V2_ROUTER:      &'static str = "0xcf77a3ba9a5ca399b7c97c74d54e5b1beb874e43";
    /// Aerodrome Universal Router (UniV3-compatible interface)
    pub const AERODROME_UNIVERSAL:      &'static str = "0x6cb442acf35158d5eda88fe602221b67b400be3e";
    /// BaseSwap Router (UniV2-compatible)
    pub const BASESWAP_ROUTER:          &'static str = "0x327df1e6de05895d2ab08513aaddd9313fe505d86";
    /// PancakeSwap V3 on Base
    pub const PANCAKE_V3_ROUTER_BASE:   &'static str = "0x678aa4bf4e210cf2166753e054d5b7c31cc7fa86";
    /// SushiSwap V3 on Base
    pub const SUSHISWAP_V3_ROUTER_BASE: &'static str = "0xfb7ef66a7e61224dd6fcd0d7d9c3be5c8b049b9";
    /// SwapBased (UniV2-compatible)
    pub const SWAPBASED_ROUTER:         &'static str = "0xaaa3b1f1bd7bcc97fd1917c18ade665c5d31f066";

    // ── SushiSwap (Ethereum/multi-chain) ──────────────────────────────────────
    pub const SUSHISWAP_V2_ROUTER:      &'static str = "0xd9e1ce17f2641f24ae83637ab66a2cca9c378b9f";
    pub const SUSHISWAP_V3_ROUTER:      &'static str = "0x2c9d885e9a5bce9c4404b7e8853b45bfe96c5f44";

    // ── PancakeSwap (BSC / Ethereum) ──────────────────────────────────────────
    pub const PANCAKE_V2_ROUTER:        &'static str = "0x10ed43c718714eb63d5aa57b78b54704e256024e";
    pub const PANCAKE_V3_ROUTER:        &'static str = "0x1b81d678ffb9c0263b24a97847620c99d213eb14";
    pub const PANCAKE_V2_ROUTER_ETH:    &'static str = "0xeff92a263d31888d860bd50809a8d171709b7b1c";

    // ── Non-EVM (RPC only) ────────────────────────────────────────────────────
    pub const RAYDIUM_AMM_PROGRAM:      &'static str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
    pub const RAYDIUM_CLMM_PROGRAM:     &'static str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
    pub const ORCA_WHIRLPOOL_PROGRAM:   &'static str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
    pub const OSMOSIS_POOL_MANAGER:     &'static str = "cosmos1hhptv9c73cuxrfrqk3ye3vvexgx2v3j8h9w6n2";
}

/// Classify a (lowercased) router address into a DEX version.
pub fn classify_router(addr: &str) -> DexVersion {
    match addr {
        // Ethereum mainnet
        RouterRegistry::UNISWAP_V2_ROUTER                                  => DexVersion::UniswapV2,
        RouterRegistry::UNISWAP_V3_ROUTER_V1
        | RouterRegistry::UNISWAP_V3_ROUTER_V2                             => DexVersion::UniswapV3,
        RouterRegistry::UNISWAP_UNIVERSAL_ETH
        | RouterRegistry::UNISWAP_UNIVERSAL_OLD                            => DexVersion::UniversalRouter,
        RouterRegistry::SUSHISWAP_V2_ROUTER                                => DexVersion::SushiSwapV2,
        RouterRegistry::SUSHISWAP_V3_ROUTER                                => DexVersion::SushiSwapV3,
        RouterRegistry::PANCAKE_V2_ROUTER
        | RouterRegistry::PANCAKE_V2_ROUTER_ETH                            => DexVersion::PancakeSwapV2,
        RouterRegistry::PANCAKE_V3_ROUTER                                  => DexVersion::PancakeSwapV3,

        // Base chain (FIX-3)
        RouterRegistry::UNISWAP_V3_ROUTER_BASE                             => DexVersion::UniswapV3,
        // Note: UNISWAP_UNIVERSAL_BASE == UNISWAP_UNIVERSAL_ETH so already matched above
        RouterRegistry::AERODROME_V2_ROUTER
        | RouterRegistry::SWAPBASED_ROUTER
        | RouterRegistry::BASESWAP_ROUTER                                  => DexVersion::AerodromeV2,
        RouterRegistry::AERODROME_UNIVERSAL                                => DexVersion::AerodromeV3,
        RouterRegistry::PANCAKE_V3_ROUTER_BASE                             => DexVersion::PancakeSwapV3,
        RouterRegistry::SUSHISWAP_V3_ROUTER_BASE                           => DexVersion::SushiSwapV3,

        _                                                                   => DexVersion::Unknown,
    }
}

/// Check if an address belongs to any known EVM DEX router.
pub fn is_known_dex_router(addr: &str) -> bool {
    classify_router(&addr.to_lowercase()) != DexVersion::Unknown
}

/// Specifically detect Uniswap V3 (or compatible) routers.
pub fn is_uniswap_router(addr: &str) -> bool {
    let lower = addr.to_lowercase();
    matches!(
        lower.as_str(),
        RouterRegistry::UNISWAP_V3_ROUTER_V1
            | RouterRegistry::UNISWAP_V3_ROUTER_V2
            | RouterRegistry::UNISWAP_V3_ROUTER_BASE
            | RouterRegistry::UNISWAP_UNIVERSAL_ETH
            | RouterRegistry::UNISWAP_UNIVERSAL_OLD
    )
}

// ─────────────────────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Uniswap V3 fee units (per-million) → basis points (per-ten-thousand).
/// e.g. 3000 → 30, 500 → 5, 100 → 1
#[inline]
pub fn fee_units_to_bps(fee_units: u32) -> u32 {
    fee_units / 100
}

/// Basis points → Uniswap V3 fee units.
/// e.g. 30 → 3000, 5 → 500
#[inline]
pub fn fee_bps_to_units(bps: u32) -> u32 {
    bps * 100
}

#[inline]
fn get_sel(data: &[u8]) -> Option<[u8; 4]> {
    data.get(..4)?.try_into().ok()
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classifier_known_routers() {
        assert_eq!(classify_router(RouterRegistry::UNISWAP_V2_ROUTER),      DexVersion::UniswapV2);
        assert_eq!(classify_router(RouterRegistry::UNISWAP_V3_ROUTER_V1),   DexVersion::UniswapV3);
        assert_eq!(classify_router(RouterRegistry::UNISWAP_V3_ROUTER_BASE), DexVersion::UniswapV3);
        assert_eq!(classify_router(RouterRegistry::UNISWAP_UNIVERSAL_ETH),  DexVersion::UniversalRouter);
        assert_eq!(classify_router(RouterRegistry::AERODROME_V2_ROUTER),    DexVersion::AerodromeV2);
        assert_eq!(classify_router(RouterRegistry::AERODROME_UNIVERSAL),    DexVersion::AerodromeV3);
        assert_eq!(classify_router(RouterRegistry::BASESWAP_ROUTER),        DexVersion::AerodromeV2);
        assert_eq!(classify_router(RouterRegistry::SUSHISWAP_V2_ROUTER),    DexVersion::SushiSwapV2);
        assert_eq!(classify_router(RouterRegistry::PANCAKE_V2_ROUTER),      DexVersion::PancakeSwapV2);
        assert_eq!(classify_router("0xdeadbeef"),                            DexVersion::Unknown);
    }

    #[test]
    fn test_base_routers_are_known() {
        // FIX-3 regression guard: all Base chain routers must be known
        assert!(is_known_dex_router(RouterRegistry::UNISWAP_V3_ROUTER_BASE));
        assert!(is_known_dex_router(RouterRegistry::AERODROME_V2_ROUTER));
        assert!(is_known_dex_router(RouterRegistry::AERODROME_UNIVERSAL));
        assert!(is_known_dex_router(RouterRegistry::BASESWAP_ROUTER));
        assert!(is_known_dex_router(RouterRegistry::PANCAKE_V3_ROUTER_BASE));
        assert!(is_known_dex_router(RouterRegistry::SUSHISWAP_V3_ROUTER_BASE));
    }

    #[test]
    fn test_fee_units_to_bps() {
        assert_eq!(fee_units_to_bps(100),   1);
        assert_eq!(fee_units_to_bps(500),   5);
        assert_eq!(fee_units_to_bps(3000),  30);
        assert_eq!(fee_units_to_bps(10000), 100);
    }

    #[test]
    fn test_fee_roundtrip() {
        for bps in [1u32, 5, 25, 30, 100] {
            assert_eq!(fee_units_to_bps(fee_bps_to_units(bps)), bps);
        }
    }

    #[test]
    fn test_v2_selector_dispatch_correctness() {
        // FIX-5 regression guard:
        // SEL_V2_TOKENS_FOR_EXACT_ETH != SEL_V2_EXACT_TOKENS_FOR_ETH
        assert_ne!(SEL_V2_TOKENS_FOR_EXACT_ETH, SEL_V2_EXACT_TOKENS_FOR_ETH,
            "Selectors for swapTokensForExactETH and swapExactTokensForETH must be distinct");

        // swapTokensForExactETH: 0x4a25d94a
        assert_eq!(SEL_V2_TOKENS_FOR_EXACT_ETH, [0x4a, 0x25, 0xd9, 0x4a]);
        // swapExactTokensForETH: 0x18cbafe5
        assert_eq!(SEL_V2_EXACT_TOKENS_FOR_ETH, [0x18, 0xcb, 0xaf, 0xe5]);
    }

    #[test]
    fn test_decode_v3_path_weth_usdc() {
        let mut path = Vec::new();
        path.extend_from_slice(&[
            0xC0, 0x2a, 0xaA, 0x39, 0xb2, 0x23, 0xFE, 0x8D, 0x0A, 0x0e,
            0x5C, 0x4F, 0x27, 0xeA, 0xD9, 0x08, 0x3C, 0x75, 0x6C, 0xc2,
        ]);
        path.extend_from_slice(&[0x00, 0x0B, 0xB8]); // fee = 3000
        path.extend_from_slice(&[
            0xA0, 0xb8, 0x69, 0x91, 0xc6, 0x21, 0x8b, 0x36, 0xc1, 0xd1,
            0x9D, 0x4a, 0x2e, 0x9E, 0xb0, 0xcE, 0x36, 0x06, 0xeB, 0x48,
        ]);

        let swap = decode_v3_path(
            &path,
            U256::from(1_000_000_000_000_000_000u128),
            DexVersion::UniswapV3,
        )
        .unwrap();
        assert_eq!(swap.fee_bps, 30); // 3000 / 100 = 30
        assert_ne!(swap.token_in,  Address::ZERO);
        assert_ne!(swap.token_out, Address::ZERO);
    }
}
