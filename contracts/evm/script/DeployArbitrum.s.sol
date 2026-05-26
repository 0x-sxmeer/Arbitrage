// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {AtomicArbV2} from "../src/AtomicArbV2.sol";

contract DeployArbitrum is Script {
    function run() public {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        // Aave V3 Pool on Arbitrum Mainnet
        address aavePoolArbitrum = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
        
        // Balancer Vault on Arbitrum Mainnet
        address balancerVaultArbitrum = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
        
        // 5% max drawdown per hour circuit breaker
        uint256 maxDrawdownPerHour = 0.05 ether; // Adjust depending on quote token (e.g., USDC vs WETH)

        AtomicArbV2 arbContract = new AtomicArbV2(aavePoolArbitrum, balancerVaultArbitrum, maxDrawdownPerHour);

        console2.log("AtomicArbV2 deployed on Arbitrum at:", address(arbContract));
        
        vm.stopBroadcast();
    }
}
