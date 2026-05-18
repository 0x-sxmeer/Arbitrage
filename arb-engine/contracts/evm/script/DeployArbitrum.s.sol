// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {AtomicArb} from "../src/AtomicArb.sol";

contract DeployArbitrum is Script {
    function run() public {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        // Aave V3 Pool on Arbitrum Mainnet
        address aavePoolArbitrum = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
        
        // Wormhole Relayer on Arbitrum Mainnet
        address wormholeRelayerArbitrum = 0x27428DD2d3ABB50bAC90940428B7bb9662758ebA;
        
        // 5% max drawdown per hour circuit breaker
        uint256 maxDrawdownPerHour = 0.05 ether; // Adjust depending on quote token (e.g., USDC vs WETH)

        AtomicArb arbContract = new AtomicArb(aavePoolArbitrum, wormholeRelayerArbitrum, maxDrawdownPerHour);

        console2.log("AtomicArb deployed on Arbitrum at:", address(arbContract));
        
        vm.stopBroadcast();
    }
}
