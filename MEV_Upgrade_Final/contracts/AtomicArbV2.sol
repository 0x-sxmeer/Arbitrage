// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

// ─────────────────────────────────────────────────────────────────────────────
//  AtomicArbV2.sol — Institutional MEV Contract
//
//  Upgrades over AtomicArb.sol (V1):
//    ✦ Balancer V2 flash loans (0% fee) alongside Aave V3 (0.05%)
//    ✦ Yul-optimized swap dispatcher (~3,000 gas cheaper per swap)
//    ✦ Up to 5-hop multi-leg routing (triangle + quad arbs)
//    ✦ Router whitelist enforced in assembly
//    ✦ Circuit breaker: auto-pause on hourly drawdown > threshold
//    ✦ Inline profit check in Yul (no SafeMath overhead)
//    ✦ Multi-chain: deploy same contract on Base / OP / ARB
// ─────────────────────────────────────────────────────────────────────────────

import {IERC20}          from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20}        from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Ownable}          from "@openzeppelin/contracts/access/Ownable.sol";
import {ReentrancyGuard}  from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {Pausable}         from "@openzeppelin/contracts/utils/Pausable.sol";

// ── Interfaces ────────────────────────────────────────────────────────────────

interface IAavePool {
    function flashLoanSimple(address receiver, address asset, uint256 amount, bytes calldata params, uint16 ref) external;
}

interface IFlashLoanSimpleReceiver {
    function executeOperation(address asset, uint256 amount, uint256 premium, address initiator, bytes calldata params) external returns (bool);
}

interface IBalancerVault {
    function flashLoan(address recipient, address[] memory tokens, uint256[] memory amounts, bytes memory userData) external;
}

interface IFlashLoanRecipient {
    function receiveFlashLoan(address[] memory tokens, uint256[] memory amounts, uint256[] memory feeAmounts, bytes memory userData) external;
}

interface IUniswapV2Router {
    function swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] calldata path, address to, uint256 deadline) external returns (uint256[] memory);
}

interface IUniswapV3Router {
    struct ExactInputSingleParams {
        address tokenIn; address tokenOut; uint24 fee; address recipient;
        uint256 amountIn; uint256 amountOutMinimum; uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata) external payable returns (uint256);
}

struct Route { address from; address to; bool stable; address factory; }
interface IAerodromeRouter {
    function swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, Route[] calldata routes, address to, uint256 deadline) external returns (uint256[] memory);
}

interface IAerodromeSlipstream {
    struct ExactInputSingleParams {
        address tokenIn; address tokenOut; int24 tickSpacing; address recipient;
        uint256 deadline; uint256 amountIn; uint256 amountOutMinimum; uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata) external payable returns (uint256);
}

// ── Structs ───────────────────────────────────────────────────────────────────

enum RouterType { UniV2, UniV3, AeroV2, Slipstream }

struct SwapLeg {
    address router;
    uint8   routerType;
    uint24  fee;
    address tokenIn;
    address tokenOut;
}

struct ArbParamsV2 {
    address    asset;
    uint256    borrowAmount;
    bool       useBalancer;
    SwapLeg[]  legs;
    address[][] paths;
    uint256    minProfitWei;
    uint256    deadline;
}

// ─────────────────────────────────────────────────────────────────────────────
//  AtomicArbV2
// ─────────────────────────────────────────────────────────────────────────────

contract AtomicArbV2 is IFlashLoanSimpleReceiver, IFlashLoanRecipient, Ownable, ReentrancyGuard, Pausable {
    using SafeERC20 for IERC20;

    // Immutables
    IAavePool      public immutable aavePool;
    IBalancerVault public immutable balancerVault;

    // State
    mapping(address => bool)       public allowedRouters;
    mapping(address => RouterType) public routerTypes;
    mapping(address => uint256)    public profitByToken;

    uint256 public maxDrawdownPerHour;
    int256  public netPnlWindow;
    uint256 public pnlWindowStart;
    uint8   private _lock;

    // Events
    event ArbExecuted(address indexed asset, uint256 borrowAmount, uint256 netProfit, uint8 flashSource, uint256 hopCount);
    event CircuitBreaker(uint256 drawdown, uint256 threshold);
    event RouterSet(address indexed router, bool allowed);

    constructor(address _aavePool, address _balancerVault, uint256 _maxDrawdown) Ownable(msg.sender) {
        require(_aavePool != address(0) && _balancerVault != address(0));
        aavePool        = IAavePool(_aavePool);
        balancerVault   = IBalancerVault(_balancerVault);
        maxDrawdownPerHour = _maxDrawdown;
        pnlWindowStart  = block.timestamp;
    }

    // ── Entry point ───────────────────────────────────────────────────────────

    function executeArbitrageV2(ArbParamsV2 calldata arb)
        external onlyOwner whenNotPaused
    {
        require(arb.borrowAmount > 0 && block.timestamp <= arb.deadline);
        require(arb.legs.length >= 2 && arb.legs.length <= 5);

        bytes memory encoded = abi.encode(arb);

        if (arb.useBalancer) {
            address[] memory tokens  = new address[](1);
            uint256[] memory amounts = new uint256[](1);
            tokens[0]  = arb.asset;
            amounts[0] = arb.borrowAmount;
            balancerVault.flashLoan(address(this), tokens, amounts, encoded);
        } else {
            aavePool.flashLoanSimple(address(this), arb.asset, arb.borrowAmount, encoded, 0);
        }
    }

    // ── Balancer callback ─────────────────────────────────────────────────────

    function receiveFlashLoan(address[] memory tokens, uint256[] memory amounts, uint256[] memory feeAmounts, bytes memory userData)
        external override nonReentrant
    {
        require(msg.sender == address(balancerVault) && _lock == 0, "V2: auth");
        _lock = 1;
        ArbParamsV2 memory arb = abi.decode(userData, (ArbParamsV2));
        _runArb(arb, tokens[0], amounts[0], amounts[0] + feeAmounts[0], 1);
        IERC20(tokens[0]).forceApprove(address(balancerVault), amounts[0] + feeAmounts[0]);
        _lock = 0;
    }

    // ── Aave callback ─────────────────────────────────────────────────────────

    function executeOperation(address asset, uint256 amount, uint256 premium, address initiator, bytes calldata params)
        external override nonReentrant returns (bool)
    {
        require(msg.sender == address(aavePool) && initiator == address(this) && _lock == 0);
        _lock = 1;
        ArbParamsV2 memory arb = abi.decode(params, (ArbParamsV2));
        _runArb(arb, asset, amount, amount + premium, 0);
        IERC20(asset).forceApprove(address(aavePool), amount + premium);
        _lock = 0;
        return true;
    }

    // ── Core arb execution with Yul profit gate ───────────────────────────────

    function _runArb(ArbParamsV2 memory arb, address asset, uint256 amount, uint256 repay, uint8 flashSrc) internal {
        uint256 running = amount;

        for (uint256 i; i < arb.legs.length; ++i) {
            running = _dispatchSwap(arb.legs[i], i < arb.paths.length ? arb.paths[i] : new address[](0), running);
            require(running > 0, "V2: zero out");
        }

        // Inline Yul profit check — cheaper than Solidity comparison
        uint256 netProfit;
        assembly {
            if lt(running, repay) {
                let ptr := mload(0x40)
                mstore(ptr, 0x08c379a000000000000000000000000000000000000000000000000000000000)
                mstore(add(ptr, 4), 32)
                mstore(add(ptr, 36), 18)
                mstore(add(ptr, 68), 0x56323a206e6f742070726f66697461626c6500000000000000000000000000)
                revert(ptr, 100)
            }
            netProfit := sub(running, repay)
        }

        require(netProfit >= arb.minProfitWei, "V2: below min");
        profitByToken[asset] += netProfit;

        int256 realProfit = int256(netProfit) - int256(tx.gasprice * gasleft());
        _circuitBreaker(realProfit);

        emit ArbExecuted(asset, amount, netProfit, flashSrc, arb.legs.length);
    }

    // ── Yul-optimized swap dispatcher ─────────────────────────────────────────

    function _dispatchSwap(SwapLeg memory leg, address[] memory path, uint256 amountIn) internal returns (uint256) {
        address router = leg.router;

        // Whitelist check in assembly
        assembly {
            mstore(0x00, router)
            mstore(0x20, 0x00)
            if iszero(sload(keccak256(0x00, 0x40))) {
                revert(0, 0)
            }
        }

        _yulApprove(leg.tokenIn, router, amountIn);
        uint256 deadline = block.timestamp + 30;
        uint256 out;

        if (leg.routerType == uint8(RouterType.UniV2)) {
            uint256[] memory amounts = IUniswapV2Router(router)
                .swapExactTokensForTokens(amountIn, 0, path, address(this), deadline);
            out = amounts[amounts.length - 1];
        } else if (leg.routerType == uint8(RouterType.UniV3)) {
            out = IUniswapV3Router(router).exactInputSingle(
                IUniswapV3Router.ExactInputSingleParams({
                    tokenIn: leg.tokenIn, tokenOut: leg.tokenOut, fee: leg.fee,
                    recipient: address(this), amountIn: amountIn,
                    amountOutMinimum: 0, sqrtPriceLimitX96: 0
                })
            );
        } else if (leg.routerType == uint8(RouterType.AeroV2)) {
            Route[] memory routes = new Route[](1);
            routes[0] = Route({ from: leg.tokenIn, to: leg.tokenOut, stable: false, factory: address(0) });
            uint256[] memory amounts = IAerodromeRouter(router)
                .swapExactTokensForTokens(amountIn, 0, routes, address(this), deadline);
            out = amounts[amounts.length - 1];
        } else if (leg.routerType == uint8(RouterType.Slipstream)) {
            out = IAerodromeSlipstream(router).exactInputSingle(
                IAerodromeSlipstream.ExactInputSingleParams({
                    tokenIn: leg.tokenIn, tokenOut: leg.tokenOut, tickSpacing: int24(uint24(leg.fee)),
                    recipient: address(this), deadline: deadline, amountIn: amountIn,
                    amountOutMinimum: 0, sqrtPriceLimitX96: 0
                })
            );
        } else {
            revert("V2: unknown router");
        }

        _yulApprove(leg.tokenIn, router, 0); // clear approval
        return out;
    }

    // ── Yul approve (~2k gas cheaper than SafeERC20) ──────────────────────────

    function _yulApprove(address token, address spender, uint256 amount) internal {
        assembly {
            let ptr := mload(0x40)
            mstore(ptr, 0x095ea7b300000000000000000000000000000000000000000000000000000000)
            mstore(add(ptr, 4), spender)
            mstore(add(ptr, 36), amount)
            if iszero(call(gas(), token, 0, ptr, 68, 0, 32)) { revert(0, 0) }
        }
    }

    // ── Circuit breaker ───────────────────────────────────────────────────────

    function _circuitBreaker(int256 net) internal {
        if (block.timestamp >= pnlWindowStart + 1 hours) {
            netPnlWindow = 0;
            pnlWindowStart = block.timestamp;
        }
        netPnlWindow += net;
        if (netPnlWindow < 0 && uint256(-netPnlWindow) >= maxDrawdownPerHour) {
            _pause();
            emit CircuitBreaker(uint256(-netPnlWindow), maxDrawdownPerHour);
        }
    }

    // ── Admin ─────────────────────────────────────────────────────────────────

    function setRouter(address router, bool allowed, RouterType rtype) external onlyOwner {
        allowedRouters[router] = allowed;
        routerTypes[router]    = rtype;
        emit RouterSet(router, allowed);
    }

    function setMaxDrawdown(uint256 d) external onlyOwner { maxDrawdownPerHour = d; }
    function pause()   external onlyOwner { _pause(); }
    function unpause() external onlyOwner { _unpause(); }

    function withdraw(address token) external onlyOwner {
        uint256 bal = IERC20(token).balanceOf(address(this));
        if (bal > 0) IERC20(token).safeTransfer(owner(), bal);
    }

    receive() external payable {}
}
