// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";

interface ISlipstreamRouter8 {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        int24 tickSpacing;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

interface ISlipstreamRouter7 {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        int24 tickSpacing;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}


contract TestSlipstream is Script {
    function run() external {
        address router = 0xBE6D8f0d05cC4be24d5167a3eF062215bE6D18a5;
        
        console2.log("=== Testing 8-word struct (WITH deadline) ===");
        ISlipstreamRouter8.ExactInputSingleParams memory params8 = ISlipstreamRouter8.ExactInputSingleParams({
            tokenIn: address(0x111),
            tokenOut: address(0x222),
            tickSpacing: 100,
            recipient: address(0x123),
            deadline: block.timestamp + 1000,
            amountIn: 1000,
            amountOutMinimum: 0,
            sqrtPriceLimitX96: 0
        });
        
        try ISlipstreamRouter8(router).exactInputSingle(params8) {
            console2.log("Success 8?");
        } catch Error(string memory reason) {
            console2.log("8 Revert reason:", reason);
        } catch (bytes memory data) {
            console2.log("8 Revert bytes:");
            console2.logBytes(data);
        }

        console2.log("=== Testing 7-word struct (NO deadline) ===");
        ISlipstreamRouter7.ExactInputSingleParams memory params7 = ISlipstreamRouter7.ExactInputSingleParams({
            tokenIn: address(0x111),
            tokenOut: address(0x222),
            tickSpacing: 100,
            recipient: address(0x123),
            amountIn: 1000,
            amountOutMinimum: 0,
            sqrtPriceLimitX96: 0
        });
        
        try ISlipstreamRouter7(router).exactInputSingle(params7) {
            console2.log("Success 7?");
        } catch Error(string memory reason) {
            console2.log("7 Revert reason:", reason);
        } catch (bytes memory data) {
            console2.log("7 Revert bytes:");
            console2.logBytes(data);
        }
    }
}
