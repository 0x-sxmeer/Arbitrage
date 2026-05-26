// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";

// ─────────────────────────────────────────────────────────────────────────────
//  SetupRouters.s.sol
//
//  Post-deployment setup: whitelist every DEX router that AtomicArbV2 will use.
//  Run ONCE after deploying the contract. Reverts gracefully if already set.
// ─────────────────────────────────────────────────────────────────────────────

enum RouterType { UniV2, UniV3, AeroV2, Slipstream, CurveNG }

interface IAtomicArbV2 {
    function setRouter(address router, bool allowed, RouterType rtype) external;
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
    address constant AERODROME_SLIPSTREAM = 0x254cF9E1E6e233aa1AC962CB9B05b2cfeAaE15b0;

    function run() external {
        uint256 deployerPk    = vm.envUint("PRIVATE_KEY");
        address contractAddr  = vm.envAddress("CONTRACT_ADDRESS");

        IAtomicArbV2 arb = IAtomicArbV2(contractAddr);

        console2.log("=== AtomicArbV2 Router Whitelist Setup ===");
        console2.log("Contract:  ", contractAddr);
        console2.log("Owner:     ", arb.owner());

        vm.startBroadcast(deployerPk);

        _whitelist(arb, UNISWAP_V3_ROUTER, "Uniswap V3 SwapRouter02", RouterType.UniV3);
        _whitelist(arb, AERODROME_V2,      "Aerodrome V2 Router", RouterType.AeroV2);
        _whitelist(arb, AERODROME_SLIPSTREAM, "Aerodrome Slipstream Router", RouterType.Slipstream);

        vm.stopBroadcast();

        // ── Verification ──────────────────────────────────────────────────────
        console2.log("\n=== Post-Setup Verification ===");
        _verify(arb, UNISWAP_V3_ROUTER, "Uniswap V3 SwapRouter02");
        _verify(arb, AERODROME_V2,      "Aerodrome V2 Router");
        _verify(arb, AERODROME_SLIPSTREAM, "Aerodrome Slipstream Router");
        console2.log("\n[OK] All routers whitelisted. AtomicArbV2 is ready for live execution.");
    }

    function _whitelist(IAtomicArbV2 arb, address router, string memory name, RouterType rType) internal {
        if (arb.allowedRouters(router)) {
            console2.log("  [SKIP] Already whitelisted:", name);
        } else {
            arb.setRouter(router, true, rType);
            console2.log("  [SET]  Whitelisted:", name);
        }
    }

    function _verify(IAtomicArbV2 arb, address router, string memory name) internal view {
        bool allowed = arb.allowedRouters(router);
        if (allowed) {
            console2.log("  [OK]  ", name);
        } else {
            console2.log("  [FAIL]", name, "-- NOT whitelisted!");
            revert("Router whitelist verification failed");
        }
    }
}
