// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {AtomicArb} from "../src/AtomicArb.sol";

contract Deploy is Script {
    function run() public {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        address aavePool;
        address wormholeRelayer;
        uint256 maxDrawdownPerHour = 0.05 ether; // default 5% drawdown

        // Try reading from environment overrides first
        try vm.envAddress("AAVE_POOL") returns (address pool) {
            aavePool = pool;
        } catch {
            aavePool = address(0);
        }

        try vm.envAddress("WORMHOLE_RELAYER") returns (address relayer) {
            wormholeRelayer = relayer;
        } catch {
            wormholeRelayer = address(0);
        }

        try vm.envUint("MAX_DRAWDOWN_PER_HOUR") returns (uint256 maxDraw) {
            maxDrawdownPerHour = maxDraw;
        } catch {}

        // If not specified in environment, auto-detect by chain ID
        if (aavePool == address(0) || wormholeRelayer == address(0)) {
            uint256 chainId = block.chainid;
            console2.log("Auto-detecting addresses for Chain ID:", chainId);

            if (chainId == 11155111) {
                // Sepolia Testnet
                if (aavePool == address(0)) aavePool = address(uint160(0x006Ae43d3271ff6888e7Fc43Fd7321a503ff738951));
                if (wormholeRelayer == address(0)) wormholeRelayer = address(uint160(0x007B621fE2A04a3aB783568b919b40F40171AeFcF4));
            } else if (chainId == 8453) {
                // Base Mainnet
                if (aavePool == address(0)) aavePool = address(uint160(0x00A238Dd80C259a72e81d7e4664a9801593F98d1c5));
                if (wormholeRelayer == address(0)) wormholeRelayer = address(uint160(0x00706F82e9bb5b0813501714Ab5974216704980e31));
            } else if (chainId == 42161) {
                // Arbitrum Mainnet
                if (aavePool == address(0)) aavePool = address(uint160(0x00794a61358D6845594F94dc1DB02A252b5b4814aD));
                if (wormholeRelayer == address(0)) wormholeRelayer = address(uint160(0x0027428Dd2D3Abb50BaC90940428B7bB9662758ebA));
            } else if (chainId == 1) {
                // Ethereum Mainnet
                if (aavePool == address(0)) aavePool = address(uint160(0x0087870B27f0bf4296857d44E8a96a1B714f24F5C9));
                if (wormholeRelayer == address(0)) wormholeRelayer = address(uint160(0x0027428Dd2D3Abb50BaC90940428B7bB9662758ebA));
            } else {
                revert("Unsupported Chain ID. Please configure AAVE_POOL and WORMHOLE_RELAYER in env.");
            }
        }

        console2.log("Deploying AtomicArb with:");
        console2.log("  Aave Pool:       ", aavePool);
        console2.log("  Wormhole Relayer:", wormholeRelayer);
        console2.log("  Max Drawdown/Hr: ", maxDrawdownPerHour);

        AtomicArb arbContract = new AtomicArb(aavePool, wormholeRelayer, maxDrawdownPerHour);

        console2.log("AtomicArb deployed at:", address(arbContract));

        vm.stopBroadcast();
    }
}
