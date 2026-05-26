// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/AtomicArbV2.sol";

// ─────────────────────────────────────────────────────────────────────────────
//  DeployV2.s.sol — Deploy AtomicArbV2 on Base / Optimism / Arbitrum
//
//  Usage:
//    # Base mainnet
//    forge script script/DeployV2.s.sol \
//      --rpc-url $BASE_HTTP_URL \
//      --broadcast --verify \
//      --etherscan-api-key $BASESCAN_API_KEY -vvvv
//
//    # Optimism
//    forge script script/DeployV2.s.sol \
//      --rpc-url $OP_HTTP_URL \
//      --broadcast --verify \
//      --etherscan-api-key $OP_ETHERSCAN_KEY -vvvv
//
//    # Arbitrum
//    forge script script/DeployV2.s.sol \
//      --rpc-url $ARB_HTTP_URL \
//      --broadcast --verify \
//      --etherscan-api-key $ARBISCAN_KEY -vvvv
// ─────────────────────────────────────────────────────────────────────────────

contract DeployV2 is Script {

    // Aave V3 Pool (chain-specific)
    address constant AAVE_BASE      = 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5;
    address constant AAVE_OPTIMISM  = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
    address constant AAVE_ARBITRUM  = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;

    // Balancer V2 Vault (SAME on all EVM chains)
    address constant BALANCER       = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;

    // Routers — Base
    address constant UNIV3_BASE         = 0x2626664c2603336E57B271c5C0b26F421741e481;
    address constant AERO_ROUTER_BASE   = 0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43;
    address constant AERO_SLIP_BASE     = 0x254cF9E1E6e233aa1AC962CB9B05b2cfeAaE15b0;

    // Routers — Optimism
    address constant UNIV3_OP           = 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45;
    address constant VELO_ROUTER_OP     = 0xa062aE8A9c5e11aaA026fc2670B0D65cCc8B2858;

    // Routers — Arbitrum
    address constant UNIV3_ARB          = 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45;
    address constant CAMELOT_ARB        = 0xc873fEcbd354f5A56E00E710B90EF4201db2448d;

    function run() external {
        uint256 key     = vm.envUint("PRIVATE_KEY");
        uint256 chainId = block.chainid;
        address deployer= vm.addr(key);

        console.log("Deploying AtomicArbV2 on chain:", chainId);
        console.log("Deployer:", deployer);

        address aavePool;
        if      (chainId == 8453)  aavePool = AAVE_BASE;
        else if (chainId == 10)    aavePool = AAVE_OPTIMISM;
        else if (chainId == 42161) aavePool = AAVE_ARBITRUM;
        else revert("Unsupported chain");

        // Max drawdown: 0.5 ETH per hour (~$1,500 at current prices)
        uint256 maxDrawdown = 0.5 ether;

        vm.startBroadcast(key);

        AtomicArbV2 arb = new AtomicArbV2(aavePool, BALANCER, maxDrawdown);
        console.log("Contract deployed at:", address(arb));

        // Whitelist routers per chain
        if (chainId == 8453) {
            arb.setRouter(UNIV3_BASE,       true, RouterType.UniV3);
            arb.setRouter(AERO_ROUTER_BASE, true, RouterType.AeroV2);
            arb.setRouter(AERO_SLIP_BASE,   true, RouterType.Slipstream);
            console.log("Whitelisted: UniV3, AeroV2, Slipstream (Base)");
        } else if (chainId == 10) {
            arb.setRouter(UNIV3_OP,     true, RouterType.UniV3);
            arb.setRouter(VELO_ROUTER_OP, true, RouterType.AeroV2);
            console.log("Whitelisted: UniV3, Velodrome (Optimism)");
        } else if (chainId == 42161) {
            arb.setRouter(UNIV3_ARB,  true, RouterType.UniV3);
            arb.setRouter(CAMELOT_ARB,true, RouterType.UniV2);
            console.log("Whitelisted: UniV3, Camelot (Arbitrum)");
        }

        vm.stopBroadcast();

        console.log("\n=== Add to .env ===");
        if      (chainId == 8453)  console.log("CONTRACT_ADDRESS=%s", address(arb));
        else if (chainId == 10)    console.log("CONTRACT_ADDRESS_OPTIMISM=%s", address(arb));
        else if (chainId == 42161) console.log("CONTRACT_ADDRESS_ARBITRUM=%s", address(arb));
    }
}
