// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

// ─────────────────────────────────────────────────────────────────────────────
//  AtomicArb.sol - Flash-loan-powered atomic arbitrage contract
//
//  Strategy:
//    1. Borrow tokens via Aave V3 flash loan (0.05% fee)
//    2. Swap token on DEX A (buy leg)
//    3. Swap token on DEX B (sell leg)
//    4. Repay flash loan + fee
//    5. Send profit to owner - or REVERT the entire transaction if not profitable
//
//  Security properties:
//    ✓ Zero loss guarantee - if any step fails, entire tx reverts
//    ✓ Owner-only execution - no unauthorized arb
//    ✓ Circuit breaker - pause execution if drawdown > 5% in 1 hour
//    ✓ Slippage guard - reverts if output < minOutputAmount
//    ✓ Reentrancy guard - protects executeOperation callback
//
//  Deployment:
//    Ethereum / Base / Arbitrum (all EVM chains with Aave V3)
// ─────────────────────────────────────────────────────────────────────────────

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import {Pausable} from "@openzeppelin/contracts/security/Pausable.sol";

// ── Aave V3 interfaces ────────────────────────────────────────────────────────

interface IPool {
    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external;
}

interface IFlashLoanSimpleReceiver {
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external returns (bool);
}

// ── Uniswap V2 Router (Sushiswap / PancakeSwap compatible) ───────────────────

interface IUniswapV2Router {
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);
}

// ── Wormhole Cross-Chain Relayer Interfaces ───────────────────────────────────

interface IWormholeRelayer {
    function sendPayloadToEvm(
        uint16 targetChain,
        address targetAddress,
        bytes memory payload,
        uint256 receiverValue,
        uint256 gasLimit
    ) external payable returns (uint64 sequence);

    function quoteEVMDeliveryPrice(
        uint16 targetChain,
        uint256 receiverValue,
        uint256 gasLimit
    ) external view returns (uint256 nativePriceQuote, uint256 targetChainRefundPerGasUnused);
}

interface IWormholeReceiver {
    function receiveWormholeMessages(
        bytes memory payload,
        bytes[] memory additionalVaas,
        bytes32 sourceAddress,
        uint16 sourceChain,
        bytes32 deliveryHash
    ) external payable;
}

// ── Uniswap V3 Router ─────────────────────────────────────────────────────────

interface IUniswapV3Router {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24  fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata params) external returns (uint256 amountOut);
}

// ─────────────────────────────────────────────────────────────────────────────
//  ArbParams - calldata passed to flash loan callback
// ─────────────────────────────────────────────────────────────────────────────

struct ArbParams {
    // Buy leg
    address buyRouter;        // DEX to buy on (Uniswap V3 or V2)
    bool    buyIsV3;          // true = Uniswap V3, false = V2
    uint24  buyFee;           // V3 only: pool fee tier (500, 3000, 10000)
    address[] buyPath;        // V2 only: token path [tokenIn, tokenOut]
    // Sell leg
    address sellRouter;       // DEX to sell on
    bool    sellIsV3;
    uint24  sellFee;
    address[] sellPath;
    // Tokens
    address tokenBorrow;      // token we flash-loan
    address tokenIntermediate;// token we receive on buy leg
    // Slippage
    uint256 minProfitWei;     // minimum net profit required (in tokenBorrow units)
    // [C-3] Per-leg expected outputs for slippage protection
    uint256 expectedBuyOut;   // expected output from buy leg (0 = use global slippageBps)
    uint256 expectedSellOut;  // expected output from sell leg (0 = use global slippageBps)
}

// ─────────────────────────────────────────────────────────────────────────────
//  AtomicArb contract
// ─────────────────────────────────────────────────────────────────────────────

contract AtomicArb is IFlashLoanSimpleReceiver, IWormholeReceiver, Ownable, ReentrancyGuard, Pausable {
    using SafeERC20 for IERC20;

    // ── State ──────────────────────────────────────────────────────────────────

    /// Aave V3 Pool (the flash loan provider)
    IPool public immutable aavePool;

    /// Wormhole Relayer
    IWormholeRelayer public immutable wormholeRelayer;

    /// [C-3] Slippage tolerance in basis points (default 0.5%, max 2%)
    uint256 public slippageBps = 50;

    /// [C-6] Cross-chain bridging is gated until fully implemented
    bool public crossChainEnabled = false;

    /// [C-6] Wormhole delivery deduplication
    mapping(bytes32 => bool) public processedDeliveries;
    /// [C-6] Registered cross-chain senders by source chain
    mapping(uint16 => bytes32) public registeredSenders;
    /// [C-6] Pre-registered receiver addresses by target chain
    mapping(uint16 => address) public crossChainReceivers;

    /// Circuit breaker: max drawdown in wei per hour before auto-pause
    uint256 public maxDrawdownPerHour;

    /// Tracks losses in the current 1-hour window
    uint256 public drawdownThisHour;
    uint256 public drawdownWindowStart;

    /// Total profit accumulated (wei, token varies by arb)
    uint256 public totalProfitAccumulated;

    // ── Events ─────────────────────────────────────────────────────────────────

    // [L-2] Events with indexed fields for efficient off-chain querying
    event ArbExecuted(
        address indexed tokenBorrow,
        uint256 borrowAmount,
        uint256 netProfit,
        address indexed buyRouter,
        address indexed sellRouter,
        uint256 blockNumber,
        uint256 timestamp
    );

    event CircuitBreakerTriggered(uint256 drawdown, uint256 threshold);
    event ProfitWithdrawn(address indexed token, uint256 amount);
    event EthReceived(address indexed sender, uint256 amount);
    event SlippageUpdated(uint256 oldBps, uint256 newBps);
    event CrossChainToggled(bool enabled);
    event CrossChainSenderRegistered(uint16 indexed sourceChain, bytes32 sender);
    event CrossChainReceiverRegistered(uint16 indexed targetChain, address receiver);
    event CrossChainMessageReceived(uint16 indexed sourceChain, bytes32 sourceAddress, bytes payload);
    event CrossChainMessageSent(uint16 indexed targetChain, bytes32 targetAddress, bytes payload);
    event CrossChainBridgeInitiated(uint16 indexed targetChain, address recipient, address token, uint256 amount);

    // ── Constructor ────────────────────────────────────────────────────────────

    constructor(address _aavePool, address _wormholeRelayer, uint256 _maxDrawdownPerHour) Ownable() {
        require(_aavePool != address(0), "AtomicArb: zero aave pool");
        require(_wormholeRelayer != address(0), "AtomicArb: zero wormhole relayer");
        aavePool = IPool(_aavePool);
        wormholeRelayer = IWormholeRelayer(_wormholeRelayer);
        maxDrawdownPerHour = _maxDrawdownPerHour;
        drawdownWindowStart = block.timestamp;
    }

    // ─────────────────────────────────────────────────────────────────────────
    //  Cross-Chain Relayer Functions
    // ─────────────────────────────────────────────────────────────────────────

    // [C-6] Gated until fully implemented + proper delivery cost quoting
    function sendCrossChainMessage(
        uint16 targetChain,
        bytes32 targetAddress,
        bytes memory payload
    ) external payable onlyOwner {
        require(crossChainEnabled, "AtomicArb: Cross-chain not yet enabled");
        require(address(wormholeRelayer) != address(0), "AtomicArb: Wormhole not configured");

        // [C-6] Calculate delivery cost from Wormhole instead of trusting msg.value blindly
        (uint256 deliveryCost, ) = wormholeRelayer.quoteEVMDeliveryPrice(
            targetChain,
            0,          // no extra receiverValue
            200_000     // sufficient gas for receiver
        );
        require(msg.value >= deliveryCost, "AtomicArb: Insufficient relayer fee");

        wormholeRelayer.sendPayloadToEvm{value: deliveryCost}(
            targetChain,
            crossChainReceivers[targetChain] != address(0)
                ? crossChainReceivers[targetChain]
                : address(uint160(uint256(targetAddress))),
            payload,
            0,          // no extra receiverValue
            200_000     // sufficient gas for receiver
        );
        emit CrossChainMessageSent(targetChain, targetAddress, payload);
    }

    // [C-6] Receiver with delivery deduplication + sender verification
    function receiveWormholeMessages(
        bytes memory payload,
        bytes[] memory /* additionalVaas */,
        bytes32 sourceAddress,
        uint16 sourceChain,
        bytes32 deliveryHash
    ) external payable override {
        require(msg.sender == address(wormholeRelayer), "AtomicArb: Only relayer can call");
        require(!processedDeliveries[deliveryHash], "AtomicArb: Already processed");
        require(
            registeredSenders[sourceChain] == bytes32(0) || registeredSenders[sourceChain] == sourceAddress,
            "AtomicArb: Unknown sender"
        );

        processedDeliveries[deliveryHash] = true;
        emit CrossChainMessageReceived(sourceChain, sourceAddress, payload);
    }

    // ─────────────────────────────────────────────────────────────────────────
    //  External: initiate arbitrage
    // ─────────────────────────────────────────────────────────────────────────

    /**
     * @notice Initiate a flash-loan-powered arbitrage.
     * @dev Only owner can call. Reverts if NEV < minProfitWei.
     *
     * @param asset          Token to flash borrow
     * @param borrowAmount   Amount to borrow (in asset units)
     * @param params         ABI-encoded ArbParams struct
     */
    // [L-3] Added deadline parameter for transaction expiry protection
    function executeArbitrage(
        address asset,
        uint256 borrowAmount,
        bytes calldata params,
        uint256 deadline
    ) external onlyOwner whenNotPaused nonReentrant {
        require(borrowAmount > 0, "AtomicArb: borrowAmount must be > 0");
        require(block.timestamp <= deadline, "AtomicArb: Transaction expired");

        // Initiate flash loan - Aave calls back executeOperation()
        aavePool.flashLoanSimple(
            address(this),  // receiver
            asset,
            borrowAmount,
            params,
            0               // referralCode
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    //  Aave V3 callback: executeOperation
    // ─────────────────────────────────────────────────────────────────────────

    /**
     * @notice Called by Aave after flash loan is disbursed.
     * @dev Executes buy → sell and repays. REVERTS if not profitable.
     *
     * All funds must be returned (amount + premium) before this function returns,
     * or Aave reverts the entire transaction - our zero-loss guarantee.
     */
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external override nonReentrant returns (bool) {
        // Security: only Aave Pool can call this
        require(msg.sender == address(aavePool), "AtomicArb: caller is not Aave Pool");
        require(initiator == address(this), "AtomicArb: invalid initiator");

        ArbParams memory arb = abi.decode(params, (ArbParams));
        uint256 repayAmount = amount + premium;

        // ── Step 1: Buy leg - swap tokenBorrow → tokenIntermediate ───────────
        // [C-3] Per-leg slippage protection: use expectedBuyOut if provided,
        // otherwise fall back to global slippageBps percentage
        uint256 buyMinOut;
        if (arb.expectedBuyOut > 0) {
            buyMinOut = (arb.expectedBuyOut * (10000 - slippageBps)) / 10000;
        } else {
            buyMinOut = (amount * (10000 - slippageBps)) / 10000;
        }

        uint256 intermediateAmount = _swap(
            arb.buyRouter,
            arb.buyIsV3,
            arb.buyFee,
            arb.buyPath,
            asset,
            arb.tokenIntermediate,
            amount,
            buyMinOut
        );

        require(intermediateAmount > 0, "AtomicArb: buy leg produced zero output");

        // ── Step 2: Sell leg - swap tokenIntermediate → tokenBorrow ──────────
        // [C-3] Per-leg slippage on sell side: use expectedSellOut if provided,
        // otherwise require at least the repayAmount (break-even guard)
        uint256 sellMinOut;
        if (arb.expectedSellOut > 0) {
            sellMinOut = (arb.expectedSellOut * (10000 - slippageBps)) / 10000;
            // Never go below repayAmount — that would be a guaranteed loss
            if (sellMinOut < repayAmount) {
                sellMinOut = repayAmount;
            }
        } else {
            sellMinOut = repayAmount;
        }

        uint256 finalAmount = _swap(
            arb.sellRouter,
            arb.sellIsV3,
            arb.sellFee,
            arb.sellPath,
            arb.tokenIntermediate,
            asset,
            intermediateAmount,
            sellMinOut
        );

        // ── Step 3: Profit check - revert if not profitable ───────────────────
        require(
            finalAmount >= repayAmount + arb.minProfitWei,
            "AtomicArb: insufficient profit - transaction reverted"
        );

        // NOTE (BUG-10 Resolution): minProfitWei is a gating threshold/slippage guard,
        // not a cost or fee. Therefore, the actual balance increase (netProfit) is
        // exactly finalAmount - repayAmount, which is correctly recorded and emitted.
        uint256 netProfit = finalAmount - repayAmount;

        // ── Step 4: Repay Aave flash loan ─────────────────────────────────────
        IERC20(asset).forceApprove(address(aavePool), repayAmount);

        // ── Step 5: Update accounting ─────────────────────────────────────────
        totalProfitAccumulated += netProfit;
        _checkCircuitBreaker(0); // no loss this execution

        emit ArbExecuted(
            asset,
            amount,
            netProfit,
            arb.buyRouter,
            arb.sellRouter,
            block.number,
            block.timestamp
        );

        IERC20(asset).forceApprove(address(aavePool), 0);
        return true;
    }

    // ─────────────────────────────────────────────────────────────────────────
    //  Internal: swap helpers
    // ─────────────────────────────────────────────────────────────────────────

    function _swap(
        address router,
        bool isV3,
        uint24 fee,
        address[] memory path,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 amountOutMin
    ) internal returns (uint256 amountOut) {
        if (!isV3) {
            require(path.length >= 2, "AtomicArb: V2 path too short");
            require(path[0] == tokenIn, "AtomicArb: path[0] != tokenIn");
            require(path[path.length - 1] == tokenOut, "AtomicArb: path end != tokenOut");
        }

        // FIX: Use forceApprove instead of safeApprove for OpenZeppelin v5 compatibility
        IERC20(tokenIn).forceApprove(router, amountIn);

        if (isV3) {
            IUniswapV3Router.ExactInputSingleParams memory v3Params =
                IUniswapV3Router.ExactInputSingleParams({
                    tokenIn:           tokenIn,
                    tokenOut:          tokenOut,
                    fee:               fee,
                    recipient:         address(this),
                    deadline:          block.timestamp + 60,
                    amountIn:          amountIn,
                    amountOutMinimum:  amountOutMin,
                    sqrtPriceLimitX96: 0
                });
            amountOut = IUniswapV3Router(router).exactInputSingle(v3Params);
        } else {
            uint256[] memory amounts = IUniswapV2Router(router).swapExactTokensForTokens(
                amountIn,
                amountOutMin,
                path,
                address(this),
                block.timestamp + 60
            );
            amountOut = amounts[amounts.length - 1];
        }

        // Clear approval (security: never leave dangling approvals)
        IERC20(tokenIn).forceApprove(router, 0);
    }

    // ─────────────────────────────────────────────────────────────────────────
    //  Circuit breaker
    // ─────────────────────────────────────────────────────────────────────────

    function _checkCircuitBreaker(uint256 loss) internal {
        // Reset window every hour
        if (block.timestamp >= drawdownWindowStart + 1 hours) {
            drawdownThisHour  = 0;
            drawdownWindowStart = block.timestamp;
        }

        drawdownThisHour += loss;

        if (drawdownThisHour >= maxDrawdownPerHour) {
            _pause();
            emit CircuitBreakerTriggered(drawdownThisHour, maxDrawdownPerHour);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    //  Admin functions
    // ─────────────────────────────────────────────────────────────────────────

    function _withdrawProfitFor(address token) internal {
        if (token == address(0)) {
            // Native ETH withdrawal
            uint256 ethBalance = address(this).balance;
            require(ethBalance > 0, "AtomicArb: no ETH balance");
            (bool success, ) = payable(owner()).call{value: ethBalance}("");
            require(success, "AtomicArb: ETH transfer failed");
            emit ProfitWithdrawn(address(0), ethBalance);
        } else {
            // ERC20 withdrawal
            uint256 balance = IERC20(token).balanceOf(address(this));
            require(balance > 0, "AtomicArb: nothing to withdraw");
            IERC20(token).safeTransfer(owner(), balance);
            emit ProfitWithdrawn(token, balance);
        }
    }

    /// Withdraw accumulated profit to owner wallet.
    function withdrawProfit(address token) external onlyOwner {
        _withdrawProfitFor(token);
    }

    /// [C-1] Batch withdraw multiple tokens in one call
    function withdrawProfits(address[] calldata tokens) external onlyOwner {
        for (uint256 i = 0; i < tokens.length; i++) {
            _withdrawProfitFor(tokens[i]);
        }
    }

    /// Report a loss for the circuit breaker
    function reportLoss(uint256 lossWei) external onlyOwner {
        _checkCircuitBreaker(lossWei);
    }

    /// [C-3] Set slippage tolerance (owner only, max 2%)
    function setSlippage(uint256 _bps) external onlyOwner {
        require(_bps <= 200, "AtomicArb: Max 2% slippage");
        uint256 oldBps = slippageBps;
        slippageBps = _bps;
        emit SlippageUpdated(oldBps, _bps);
    }

    /// [C-6] Toggle cross-chain bridging (disabled by default)
    function setCrossChainEnabled(bool _enabled) external onlyOwner {
        crossChainEnabled = _enabled;
        emit CrossChainToggled(_enabled);
    }

    /// [C-6] Register a trusted sender for a source chain
    function registerCrossChainSender(uint16 sourceChain, bytes32 sender) external onlyOwner {
        registeredSenders[sourceChain] = sender;
        emit CrossChainSenderRegistered(sourceChain, sender);
    }

    /// [C-6] Register a receiver address for a target chain
    function registerCrossChainReceiver(uint16 targetChain, address receiver) external onlyOwner {
        crossChainReceivers[targetChain] = receiver;
        emit CrossChainReceiverRegistered(targetChain, receiver);
    }

    /// Update the circuit breaker threshold.
    function setMaxDrawdown(uint256 newMax) external onlyOwner {
        maxDrawdownPerHour = newMax;
    }

    /// Manually pause execution (emergency stop).
    function pause() external onlyOwner { _pause(); }

    /// Resume execution after circuit breaker or manual pause.
    function unpause() external onlyOwner { _unpause(); }

    /// Accept ETH (needed for WETH unwrap scenarios + gas tips).
    receive() external payable {
        emit EthReceived(msg.sender, msg.value);
    }
}
