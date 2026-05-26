// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test, console2} from "forge-std/Test.sol";
import {AtomicArbV2} from "../src/AtomicArbV2.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

contract AtomicArbTest is Test {
    AtomicArbV2 arb;
    address constant AAVE_POOL_SEPOLIA = 0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951;
    address constant WETH_SEPOLIA      = 0x7b79995e5f793A07Bc00c21412e50Ecae098E7f9;
    address constant BALANCER_VAULT    = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;

    function setUp() public {
        // Fork Sepolia
        vm.createSelectFork(vm.envString("ETH_HTTP_URL"));
        arb = new AtomicArbV2(
            AAVE_POOL_SEPOLIA,
            BALANCER_VAULT,
            0.1 ether
        );
    }

    function test_ownerIsDeployer() public view {
        assertEq(arb.owner(), address(this));
    }

    function test_withdrawProfitRevertsOnZero() public {
        vm.expectRevert("V2: no profit");
        arb.withdrawProfit(WETH_SEPOLIA);
    }

    function test_setSlippageRevertsAbove200() public {
        vm.expectRevert("V2: Max 2% slippage");
        arb.setSlippage(201);
    }

    function test_pauseAndUnpause() public {
        arb.pause();
        assertTrue(arb.paused());
        arb.unpause();
        assertFalse(arb.paused());
    }
}
