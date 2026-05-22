// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";

// ─────────────────────────────────────────────────────────────────────────────
//  SetupRouters.s.sol
//
//  Post-deployment setup: whitelist every DEX router that AtomicArb will use.
//  Run ONCE after deploying the contract. Reverts gracefully if already set.
//
//  Usage:
//    forge script script/SetupRouters.s.sol \
//      --rpc-url $BASE_HTTP_URL \
//      --private-key $PRIVATE_KEY \
//      --broadcast
// ─────────────────────────────────────────────────────────────────────────────

interface IAtomicArb {
    function setRouterAllowed(address router, bool allowed, uint8 rType) external;
    function allowedRouters(address) external view returns (bool);
    function owner() external view returns (address);
}

contract SetupRouters is Script {
    // ── Base Mainnet DEX Routers ───────────────────────────────────────────────
    // Uniswap V3 SwapRouter02 on Base
    address constant UNISWAP_V3_ROUTER  = 0x2626664c2603336E57B271c5C0b26F421741e481;
    // Aerodrome V2 Router (Solidly-compatible, handles volatile & stable pools)
    address constant AERODROME_V2       = 0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43;
    // Aerodrome SwapRouter (for Slipstream pools)
    address constant AERODROME_SLIPSTREAM = 0xBE6D8f0d05cC4be24d5167a3eF062215bE6D18a5;
    // Uniswap Universal Router on Base (aggregates V2+V3)
    address constant UNISWAP_UNIV       = 0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD;

    function run() external {
        uint256 deployerPk    = vm.envUint("PRIVATE_KEY");
        address contractAddr  = vm.envAddress("CONTRACT_ADDRESS");

        IAtomicArb arb = IAtomicArb(contractAddr);

        console2.log("=== AtomicArb Router Whitelist Setup ===");
        console2.log("Contract:  ", contractAddr);
        console2.log("Owner:     ", arb.owner());

        vm.startBroadcast(deployerPk);

        _whitelist(arb, UNISWAP_V3_ROUTER, "Uniswap V3 SwapRouter02", 0);
        _whitelist(arb, AERODROME_V2,      "Aerodrome V2 Router", 1); // RouterType.AerodromeV2
        _whitelist(arb, AERODROME_SLIPSTREAM, "Aerodrome Slipstream Router", 0);
        _whitelist(arb, UNISWAP_UNIV,      "Uniswap Universal Router", 0);

        vm.stopBroadcast();

        // ── Verification ──────────────────────────────────────────────────────
        console2.log("\n=== Post-Setup Verification ===");
        _verify(arb, UNISWAP_V3_ROUTER, "Uniswap V3 SwapRouter02");
        _verify(arb, AERODROME_V2,      "Aerodrome V2 Router");
        _verify(arb, AERODROME_SLIPSTREAM, "Aerodrome Slipstream Router");
        _verify(arb, UNISWAP_UNIV,      "Uniswap Universal Router");
        console2.log("\n[OK] All routers whitelisted. AtomicArb is ready for live execution.");
    }

    function _whitelist(IAtomicArb arb, address router, string memory name, uint8 rType) internal {
        if (arb.allowedRouters(router)) {
            console2.log("  [SKIP] Already whitelisted:", name);
        } else {
            arb.setRouterAllowed(router, true, rType);
            console2.log("  [SET]  Whitelisted:", name);
        }
    }

    function _verify(IAtomicArb arb, address router, string memory name) internal view {
        bool allowed = arb.allowedRouters(router);
        if (allowed) {
            console2.log("  [OK]  ", name);
        } else {
            console2.log("  [FAIL]", name, "-- NOT whitelisted!");
            revert("Router whitelist verification failed");
        }
    }
}
