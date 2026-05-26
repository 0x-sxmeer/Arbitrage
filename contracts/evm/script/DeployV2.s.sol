// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/AtomicArbV2.sol";

// ─────────────────────────────────────────────────────────────────────────────
//  DeployV2.s.sol — Deploy AtomicArbV2 on Base / Optimism / Arbitrum
//
//  Usage:
//    # Base mainnet
//    forge script script/DeployV2.s.sol --rpc-url $BASE_HTTP_URL --broadcast --verify -vvvv
//
//    # Optimism mainnet
//    forge script script/DeployV2.s.sol --rpc-url $OP_HTTP_URL --broadcast --verify -vvvv
//
//    # Arbitrum mainnet
//    forge script script/DeployV2.s.sol --rpc-url $ARB_HTTP_URL --broadcast --verify -vvvv
// ─────────────────────────────────────────────────────────────────────────────

contract DeployV2 is Script {

    // ── Protocol addresses per chain ─────────────────────────────────────────

    // Aave V3 Pool
    address constant AAVE_POOL_BASE      = 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5;
    address constant AAVE_POOL_OPTIMISM  = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
    address constant AAVE_POOL_ARBITRUM  = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;

    // Balancer V2 Vault (same address on all EVM chains!)
    address constant BALANCER_VAULT      = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;

    // Routers on Base
    address constant UNISWAP_V3_ROUTER_BASE      = 0x2626664c2603336E57B271c5C0b26F421741e481;
    address constant AERODROME_ROUTER_BASE        = 0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43;
    address constant AERODROME_SLIPSTREAM_BASE    = 0x254cF9E1E6e233aa1AC962CB9B05b2cfeAaE15b0;

    // Routers on Optimism
    address constant UNISWAP_V3_ROUTER_OPTIMISM   = 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45;
    address constant VELODROME_ROUTER_OPTIMISM    = 0xa062aE8A9c5e11aaA026fc2670B0D65cCc8B2858;

    // Routers on Arbitrum
    address constant UNISWAP_V3_ROUTER_ARBITRUM   = 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45;
    address constant CAMELOT_ROUTER_ARBITRUM      = 0xc873fEcbd354f5A56E00E710B90EF4201db2448d;

    function run() external {
        uint256 deployerKey = vm.envUint("PRIVATE_KEY");
        address deployer    = vm.addr(deployerKey);
        uint256 chainId     = block.chainid;

        console.log("Deploying AtomicArbV2...");
        console.log("  Chain ID:  ", chainId);
        console.log("  Deployer:  ", deployer);

        // Select addresses based on chain
        address aavePool;
        if (chainId == 8453) {
            aavePool = AAVE_POOL_BASE;
            console.log("  Network:    Base Mainnet");
        } else if (chainId == 10) {
            aavePool = AAVE_POOL_OPTIMISM;
            console.log("  Network:    Optimism Mainnet");
        } else if (chainId == 42161) {
            aavePool = AAVE_POOL_ARBITRUM;
            console.log("  Network:    Arbitrum One");
        } else {
            revert("Unsupported chain");
        }

        // Max drawdown: 0.5 ETH per hour (~$1,500 at current prices)
        uint256 maxDrawdown = 0.5 ether;

        vm.startBroadcast(deployerKey);

        AtomicArbV2 arb = new AtomicArbV2(
            aavePool,
            BALANCER_VAULT,
            maxDrawdown
        );

        console.log("  Contract:  ", address(arb));

        // ── Whitelist routers per chain ─────────────────────────────────────
        if (chainId == 8453) {
            // Base routers
            arb.setRouter(UNISWAP_V3_ROUTER_BASE,   true, RouterType.UniV3);
            arb.setRouter(AERODROME_ROUTER_BASE,     true, RouterType.AeroV2);
            arb.setRouter(AERODROME_SLIPSTREAM_BASE, true, RouterType.Slipstream);
            console.log("  Routers:    UniV3, AeroV2, Slipstream whitelisted");
        } else if (chainId == 10) {
            // Optimism routers
            arb.setRouter(UNISWAP_V3_ROUTER_OPTIMISM, true, RouterType.UniV3);
            arb.setRouter(VELODROME_ROUTER_OPTIMISM,   true, RouterType.AeroV2);
            console.log("  Routers:    UniV3, Velodrome whitelisted");
        } else if (chainId == 42161) {
            // Arbitrum routers
            arb.setRouter(UNISWAP_V3_ROUTER_ARBITRUM, true, RouterType.UniV3);
            arb.setRouter(CAMELOT_ROUTER_ARBITRUM,     true, RouterType.UniV2);
            console.log("  Routers:    UniV3, Camelot whitelisted");
        }

        vm.stopBroadcast();

        console.log(unicode"\n✅ Deployment successful!");
        console.log("   Add to .env:");
        if (chainId == 8453) {
            console.log("   CONTRACT_ADDRESS=%s", address(arb));
        } else if (chainId == 10) {
            console.log("   CONTRACT_ADDRESS_OPTIMISM=%s", address(arb));
        } else {
            console.log("   CONTRACT_ADDRESS_ARBITRUM=%s", address(arb));
        }
    }
}
