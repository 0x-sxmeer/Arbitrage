// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {AtomicArbV2} from "../src/AtomicArbV2.sol";

contract Deploy is Script {
    function run() public {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        address aavePool;
        address balancerVault;
        uint256 maxDrawdownPerHour = 0.1 ether; // default 0.1 ETH in gas costs

        // Try reading from environment overrides first
        try vm.envAddress("AAVE_POOL") returns (address pool) {
            aavePool = pool;
        } catch {
            aavePool = address(0);
        }

        try vm.envAddress("BALANCER_VAULT") returns (address vault) {
            balancerVault = vault;
        } catch {
            balancerVault = address(0);
        }

        try vm.envUint("MAX_DRAWDOWN_PER_HOUR") returns (uint256 maxDraw) {
            maxDrawdownPerHour = maxDraw;
        } catch {}

        // If not specified in environment, auto-detect by chain ID
        if (aavePool == address(0) || balancerVault == address(0)) {
            uint256 chainId = block.chainid;
            console2.log("Auto-detecting addresses for Chain ID:", chainId);

            if (chainId == 11155111) {
                // Sepolia Testnet
                if (aavePool == address(0)) aavePool = 0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951;
                if (balancerVault == address(0)) balancerVault = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
            } else if (chainId == 8453) {
                // Base Mainnet
                if (aavePool == address(0)) aavePool = 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5;
                if (balancerVault == address(0)) balancerVault = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
            } else if (chainId == 42161) {
                // Arbitrum Mainnet
                if (aavePool == address(0)) aavePool = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
                if (balancerVault == address(0)) balancerVault = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
            } else if (chainId == 1) {
                // Ethereum Mainnet
                if (aavePool == address(0)) aavePool = 0x87870B27f0bf4296857d44E8a96a1B714F24F5C9;
                if (balancerVault == address(0)) balancerVault = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
            } else {
                revert("Unsupported Chain ID. Please configure AAVE_POOL and BALANCER_VAULT in env.");
            }
        }

        console2.log("Deploying AtomicArbV2 with:");
        console2.log("  Aave Pool:       ", aavePool);
        console2.log("  Balancer Vault:  ", balancerVault);
        console2.log("  Max Drawdown/Hr: ", maxDrawdownPerHour);

        AtomicArbV2 arbContract = new AtomicArbV2(aavePool, balancerVault, maxDrawdownPerHour);

        console2.log("AtomicArbV2 deployed at:", address(arbContract));

        vm.stopBroadcast();
    }
}
