// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test, console2} from "forge-std/Test.sol";
import {AtomicArb} from "../src/AtomicArb.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

contract AtomicArbTest is Test {
    AtomicArb arb;
    address constant AAVE_POOL_SEPOLIA = 0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951;
    address constant WETH_SEPOLIA      = 0x7b79995e5f793A07Bc00c21412e50Ecae098E7f9;

    function setUp() public {
        // Fork Sepolia
        vm.createSelectFork(vm.envString("ETH_HTTP_URL"));
        arb = new AtomicArb(
            AAVE_POOL_SEPOLIA,
            address(0),     // Wormhole disabled
            0.1 ether
        );
    }

    function test_ownerIsDeployer() public view {
        assertEq(arb.owner(), address(this));
    }

    function test_withdrawProfitRevertsOnZero() public {
        vm.expectRevert("AtomicArb: no profit to withdraw for token");
        arb.withdrawProfit(WETH_SEPOLIA);
    }

    function test_setSlippageRevertsAbove200() public {
        vm.expectRevert("AtomicArb: Max 2% slippage");
        arb.setSlippage(201);
    }

    function test_pauseAndUnpause() public {
        arb.pause();
        assertTrue(arb.paused());
        arb.unpause();
        assertFalse(arb.paused());
    }
}
