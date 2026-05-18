// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {AtomicArb} from "../src/AtomicArb.sol";

contract DeployBase is Script {
    function run() public {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        // Aave V3 Pool on Base Mainnet
        address aavePoolBase = 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5;
        
        // Wormhole Relayer on Base Mainnet
        address wormholeRelayerBase = 0x706F82e9bb5b0813501714Ab5974216704980e31;
        
        // 5% max drawdown per hour circuit breaker
        uint256 maxDrawdownPerHour = 0.05 ether;

        AtomicArb arbContract = new AtomicArb(aavePoolBase, wormholeRelayerBase, maxDrawdownPerHour);

        console2.log("AtomicArb deployed on Base at:", address(arbContract));
        
        vm.stopBroadcast();
    }
}
