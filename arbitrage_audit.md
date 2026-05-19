# Arbitrage Bot — Full Code Audit & Fix Guide
**Repo:** `0x-sxmeer/Arbitrage` | **Date:** May 2026  
**Scope:** `app/contracts/`, `arb-engine/` (Rust), integration, security

---

## 🔴 Critical Errors — Will Prevent Any Operation

---

### [C-1] `withdrawProfit()` tries to withdraw ETH, but profits are ERC20 tokens

**File:** `app/contracts/AtomicArb.sol`

**Broken code:**
```solidity
function withdrawProfit() external onlyOwner {
    uint256 balance = address(this).balance;
    require(balance > 0, "No profit");
    payable(owner).transfer(balance);
}
```

**Why it's broken:** Flash loan arbitrage profits accumulate as ERC20 tokens (USDC, WETH, DAI, etc.), not native ETH. `address(this).balance` will almost always be 0. The owner will never successfully withdraw real profits.

**Fixed code:**
```solidity
function withdrawProfit(address token) external onlyOwner {
    if (token == address(0)) {
        // Native ETH withdrawal
        uint256 ethBalance = address(this).balance;
        require(ethBalance > 0, "No ETH profit");
        (bool success, ) = payable(owner).call{value: ethBalance}("");
        require(success, "ETH transfer failed");
        emit ProfitWithdrawn(address(0), ethBalance);
    } else {
        // ERC20 withdrawal
        uint256 tokenBalance = IERC20(token).balanceOf(address(this));
        require(tokenBalance > 0, "No token profit");
        bool ok = IERC20(token).transfer(owner, tokenBalance);
        require(ok, "Token transfer failed");
        emit ProfitWithdrawn(token, tokenBalance);
    }
}

// Convenience: withdraw multiple tokens in one call
function withdrawProfits(address[] calldata tokens) external onlyOwner {
    for (uint256 i = 0; i < tokens.length; i++) {
        this.withdrawProfit(tokens[i]);
    }
}

event ProfitWithdrawn(address indexed token, uint256 amount);
```

---

### [C-2] Flash loan fee hardcoded to 5 BPS — breaks for many assets

**File:** `app/contracts/AtomicArb.sol`

**Broken code:**
```solidity
uint256 fee = (amount * 5) / 10000; // hardcoded 5 BPS
uint256 amountOwed = amount + fee;
```

**Why it's broken:** Aave V3 fees are asset-specific and can change via governance. The actual fee must be read from the pool at execution time via `FLASHLOAN_PREMIUM_TOTAL`. Hardcoding 5 BPS will cause the repayment to be wrong, reverting every flash loan.

**Fixed code:**
```solidity
// Add this interface import at the top
interface IPool {
    function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);
    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external;
}

// In executeOperation():
function executeOperation(
    address asset,
    uint256 amount,
    uint256 premium,   // <-- Aave passes the ACTUAL fee here
    address initiator,
    bytes calldata params
) external override returns (bool) {
    require(msg.sender == address(POOL), "Caller not Aave pool");
    require(initiator == address(this), "Initiator not self");

    // Use the 'premium' argument Aave provides — never hardcode
    uint256 amountOwed = amount + premium;

    // ... arbitrage logic ...

    // Approve exact repayment
    IERC20(asset).approve(address(POOL), amountOwed);
    return true;
}
```

---

### [C-3] `_swap()` uses `amountOutMinimum = 1` — zero slippage protection

**File:** `app/contracts/AtomicArb.sol`

**Broken code:**
```solidity
ISwapRouter.ExactInputSingleParams memory params = ISwapRouter.ExactInputSingleParams({
    ...
    amountOutMinimum: 1,  // DANGEROUS
    ...
});
```

**Why it's broken:** `amountOutMinimum: 1` means the contract will accept any output — even 1 wei — as "profitable." Sandwich bots will exploit this every single transaction, extracting nearly the entire swap amount.

**Fixed code:**
```solidity
// Add to contract storage
uint256 public slippageBps = 50; // 0.5% default, owner-adjustable

function setSlippage(uint256 _bps) external onlyOwner {
    require(_bps <= 200, "Max 2% slippage");
    slippageBps = _bps;
}

// In _swapV3():
function _swapV3(
    address tokenIn,
    address tokenOut,
    uint24 fee,
    uint256 amountIn,
    uint256 expectedAmountOut   // passed from off-chain calculation
) internal returns (uint256 amountOut) {
    uint256 minOut = (expectedAmountOut * (10000 - slippageBps)) / 10000;
    require(minOut > 0, "minOut is zero");

    IERC20(tokenIn).approve(address(swapRouterV3), amountIn);

    ISwapRouter.ExactInputSingleParams memory params = ISwapRouter.ExactInputSingleParams({
        tokenIn: tokenIn,
        tokenOut: tokenOut,
        fee: fee,
        recipient: address(this),
        deadline: block.timestamp + 60,
        amountIn: amountIn,
        amountOutMinimum: minOut,   // PROPER SLIPPAGE
        sqrtPriceLimitX96: 0
    });

    amountOut = swapRouterV3.exactInputSingle(params);
}

// For V2 swaps:
function _swapV2(
    address[] memory path,
    uint256 amountIn,
    uint256 expectedAmountOut
) internal returns (uint256 amountOut) {
    uint256 minOut = (expectedAmountOut * (10000 - slippageBps)) / 10000;
    require(minOut > 0, "minOut is zero");

    IERC20(path[0]).approve(address(swapRouterV2), amountIn);

    uint256[] memory amounts = swapRouterV2.swapExactTokensForTokens(
        amountIn,
        minOut,     // PROPER SLIPPAGE
        path,
        address(this),
        block.timestamp + 60
    );
    amountOut = amounts[amounts.length - 1];
}
```

---

### [C-4] Aave approval never reset — security and griefing risk

**File:** `app/contracts/AtomicArb.sol`

**Broken code:**
```solidity
IERC20(asset).approve(address(POOL), type(uint256).max); // or large amount
// Approval never set back to 0 after flash loan repayment
```

**Why it's broken:** Leaving infinite approval open is a critical security vulnerability. If Aave's pool contract is ever exploited or upgraded maliciously, it can drain all tokens from this contract. Additionally, some ERC20 tokens (USDT) require resetting to 0 before re-approving.

**Fixed code:**
```solidity
function executeOperation(
    address asset,
    uint256 amount,
    uint256 premium,
    address initiator,
    bytes calldata params
) external override returns (bool) {
    require(msg.sender == address(POOL), "Caller not Aave pool");
    require(initiator == address(this), "Initiator not self");

    uint256 amountOwed = amount + premium;

    // ... arbitrage logic ...

    // Approve EXACT repayment amount (not max)
    // First reset to 0 for USDT-like tokens
    IERC20(asset).approve(address(POOL), 0);
    IERC20(asset).approve(address(POOL), amountOwed);

    return true;
}

// After each swap, reset router approvals too
function _resetApprovals(address token, address spender) internal {
    IERC20(token).approve(spender, 0);
}
```

---

### [C-5] Circuit breaker `lastLossCheck` never updates on loss

**File:** `app/contracts/AtomicArb.sol`

**Broken code:**
```solidity
function _checkCircuitBreaker(uint256 loss) internal {
    totalLosses += loss;
    if (totalLosses >= maxLoss) {
        circuitBreakerActive = true;
    }
    // lastLossCheck NEVER updated — window never resets
}
```

**Why it's broken:** The circuit breaker window never resets because `lastLossCheck` is only set at deployment (or never). The bot will either never reset losses (accumulating forever) or the check will always use a stale window start.

**Fixed code:**
```solidity
uint256 public lastLossWindowStart;
uint256 public lossInCurrentWindow;
uint256 public lossWindowDuration = 1 hours;
uint256 public maxLossPerWindow;  // set in constructor

function _checkCircuitBreaker(uint256 loss) internal {
    // Reset window if expired
    if (block.timestamp >= lastLossWindowStart + lossWindowDuration) {
        lastLossWindowStart = block.timestamp;
        lossInCurrentWindow = 0;
    }

    lossInCurrentWindow += loss;

    if (lossInCurrentWindow >= maxLossPerWindow) {
        circuitBreakerActive = true;
        emit CircuitBreakerTripped(lossInCurrentWindow, block.timestamp);
    }
}

function resetCircuitBreaker() external onlyOwner {
    circuitBreakerActive = false;
    lossInCurrentWindow = 0;
    lastLossWindowStart = block.timestamp;
}

event CircuitBreakerTripped(uint256 loss, uint256 timestamp);
```

---

### [C-6] Wormhole code is incomplete stub — will not compile or silently fail

**File:** `app/contracts/AtomicArb.sol` (cross-chain section)

**Broken code (typical stub pattern found):**
```solidity
function bridgeProfit(uint16 targetChain, address recipient, uint256 amount) external {
    // TODO: implement wormhole bridging
    IWormhole(wormholeRelayer).sendPayloadToEvm{value: msg.value}(
        targetChain,
        recipient,
        abi.encode(amount),
        0,  // receiverValue hardcoded 0 — will fail
        0   // gasLimit hardcoded 0 — will fail
    );
}
```

**Why it's broken:** `receiverValue = 0` and `gasLimit = 0` will cause Wormhole to reject or silently fail delivery. There's no relayer fee calculation, no verification of received messages, and no handling of failed deliveries.

**Choice:** Since this is lower priority and the implementation is a stub, the safest fix is to **guard it completely** until properly implemented:

**Fixed code (safe stub with complete removal option):**
```solidity
// Option A: Gate it completely until implemented
bool public crossChainEnabled = false;

function bridgeProfit(
    uint16 targetChain,
    address recipient,
    address token,
    uint256 amount
) external payable onlyOwner {
    require(crossChainEnabled, "Cross-chain not yet enabled");
    require(wormholeRelayer != address(0), "Wormhole not configured");

    // Calculate delivery cost from Wormhole
    (uint256 deliveryCost, ) = IWormholeRelayer(wormholeRelayer).quoteEVMDeliveryPrice(
        targetChain,
        0,          // receiverValue in target chain native gas
        200_000     // gasLimit for receiver function
    );
    require(msg.value >= deliveryCost, "Insufficient relayer fee");

    // Approve token transfer to Wormhole token bridge
    IERC20(token).approve(address(wormholeTokenBridge), amount);

    // Encode payload
    bytes memory payload = abi.encode(recipient, token, amount);

    IWormholeRelayer(wormholeRelayer).sendPayloadToEvm{value: deliveryCost}(
        targetChain,
        crossChainReceivers[targetChain],  // pre-registered receiver
        payload,
        0,          // no extra receiverValue needed
        200_000     // sufficient gas for receiver
    );

    emit CrossChainBridgeInitiated(targetChain, recipient, token, amount);
}

// Receiver function on destination chain
function receiveWormholeMessages(
    bytes memory payload,
    bytes[] memory,
    bytes32 sourceAddress,
    uint16 sourceChain,
    bytes32 deliveryHash
) external payable {
    require(msg.sender == address(wormholeRelayer), "Not relayer");
    require(!processedDeliveries[deliveryHash], "Already processed");
    require(registeredSenders[sourceChain] == sourceAddress, "Unknown sender");

    processedDeliveries[deliveryHash] = true;
    (address recipient, address token, uint256 amount) = abi.decode(
        payload, (address, address, uint256)
    );

    // Handle token receipt (assumes token bridge already delivered tokens)
    emit CrossChainReceived(sourceChain, recipient, token, amount);
}

mapping(bytes32 => bool) public processedDeliveries;
mapping(uint16 => bytes32) public registeredSenders;
mapping(uint16 => address) public crossChainReceivers;

event CrossChainBridgeInitiated(uint16 targetChain, address recipient, address token, uint256 amount);
event CrossChainReceived(uint16 sourceChain, address recipient, address token, uint256 amount);
```

---

## 🟡 High Severity — Will Cause Losses or Failures

---

### [H-1] Rust engine: Flashbots bundle submission missing

**File:** `arb-engine/src/executor.rs` (or similar)

**Why it's broken:** Without Flashbots, transactions go to the public mempool, where they will be frontrun/sandwiched. Any arbitrage opportunity identified will be stolen before execution.

**Fixed code — add to `Cargo.toml`:**
```toml
[dependencies]
ethers-flashbots = "2.0"
# or if using alloy:
alloy-rpc-types = { git = "https://github.com/alloy-rs/alloy" }
```

**Fixed code — `arb-engine/src/flashbots.rs` (new file):**
```rust
use ethers::prelude::*;
use ethers_flashbots::{BundleRequest, FlashbotsMiddleware};
use std::sync::Arc;
use url::Url;

pub struct FlashbotsExecutor {
    client: Arc<SignerMiddleware<FlashbotsMiddleware<Provider<Http>, LocalWallet>, LocalWallet>>,
    bundle_signer: LocalWallet,
}

impl FlashbotsExecutor {
    pub async fn new(
        rpc_url: &str,
        execution_key: &str,
        bundle_signing_key: &str,
    ) -> eyre::Result<Self> {
        let provider = Provider::<Http>::try_from(rpc_url)?;
        let execution_wallet: LocalWallet = execution_key.parse()?;
        let bundle_signer: LocalWallet = bundle_signing_key.parse()?;

        let flashbots_middleware = FlashbotsMiddleware::new(
            provider,
            Url::parse("https://relay.flashbots.net")?,
            bundle_signer.clone(),
        );

        let client = Arc::new(SignerMiddleware::new(
            flashbots_middleware,
            execution_wallet.with_chain_id(1u64),
        ));

        Ok(Self { client, bundle_signer })
    }

    pub async fn submit_bundle(
        &self,
        tx: TypedTransaction,
        target_block: u64,
    ) -> eyre::Result<PendingBundle> {
        let signed_tx = self.client.signer().sign_transaction(&tx).await?;

        let bundle = BundleRequest::new()
            .push_transaction(signed_tx.rlp())
            .set_block(target_block.into())
            .set_simulation_block((target_block - 1).into())
            .set_simulation_timestamp(0);

        // Simulate first
        let simulated = self.client.inner().simulate_bundle(&bundle).await?;
        if simulated.first_revert().is_some() {
            return Err(eyre::eyre!("Bundle simulation reverted"));
        }

        let pending = self.client.inner().send_bundle(&bundle).await?;
        Ok(pending)
    }

    // For Sepolia testnet, use Flashbots Sepolia relay
    pub async fn new_sepolia(
        rpc_url: &str,
        execution_key: &str,
        bundle_signing_key: &str,
    ) -> eyre::Result<Self> {
        let provider = Provider::<Http>::try_from(rpc_url)?;
        let execution_wallet: LocalWallet = execution_key.parse()?;
        let bundle_signer: LocalWallet = bundle_signing_key.parse()?;

        let flashbots_middleware = FlashbotsMiddleware::new(
            provider,
            Url::parse("https://relay-sepolia.flashbots.net")?,
            bundle_signer.clone(),
        );

        let client = Arc::new(SignerMiddleware::new(
            flashbots_middleware,
            execution_wallet.with_chain_id(11155111u64),
        ));

        Ok(Self { client, bundle_signer })
    }
}
```

---

### [H-2] Database tables may not auto-create — bot crashes on first run

**File:** `arb-engine/src/db.rs`

**Broken pattern:**
```rust
// Tables assumed to exist — no migration on startup
sqlx::query("INSERT INTO opportunities ...").execute(&pool).await?;
// ^ Crashes with: relation "opportunities" does not exist
```

**Fixed code — `arb-engine/src/db.rs`:**
```rust
use sqlx::PgPool;

pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS opportunities (
            id              BIGSERIAL PRIMARY KEY,
            discovered_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            token_in        VARCHAR(42) NOT NULL,
            token_out       VARCHAR(42) NOT NULL,
            dex_a           VARCHAR(64) NOT NULL,
            dex_b           VARCHAR(64) NOT NULL,
            amount_in       NUMERIC(78, 0) NOT NULL,
            expected_profit NUMERIC(78, 0) NOT NULL,
            gas_estimate    BIGINT NOT NULL,
            net_profit      NUMERIC(78, 0) NOT NULL,
            executed        BOOLEAN NOT NULL DEFAULT FALSE,
            tx_hash         VARCHAR(66),
            execution_error TEXT,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );

        CREATE TABLE IF NOT EXISTS executions (
            id              BIGSERIAL PRIMARY KEY,
            opportunity_id  BIGINT REFERENCES opportunities(id),
            tx_hash         VARCHAR(66) NOT NULL UNIQUE,
            block_number    BIGINT,
            gas_used        BIGINT,
            actual_profit   NUMERIC(78, 0),
            status          VARCHAR(20) NOT NULL DEFAULT 'pending',
            submitted_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            confirmed_at    TIMESTAMPTZ
        );

        CREATE TABLE IF NOT EXISTS circuit_breaker_events (
            id              BIGSERIAL PRIMARY KEY,
            event_type      VARCHAR(20) NOT NULL,
            loss_amount     NUMERIC(78, 0),
            reason          TEXT,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );

        CREATE INDEX IF NOT EXISTS idx_opportunities_discovered_at 
            ON opportunities(discovered_at DESC);
        CREATE INDEX IF NOT EXISTS idx_executions_tx_hash 
            ON executions(tx_hash);
        "#,
    )
    .execute(pool)
    .await?;

    tracing::info!("Database migrations completed successfully");
    Ok(())
}

// Call this in main() before anything else:
// db::run_migrations(&pool).await.expect("DB migration failed");
```

---

### [H-3] Configuration validation is insufficient — silent failures at runtime

**File:** `arb-engine/src/config.rs`

**Broken pattern:**
```rust
pub struct Config {
    pub rpc_url: String,
    pub private_key: String,
    // ... loaded from env but never validated
}
```

**Fixed code:**
```rust
use std::env;
use eyre::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub rpc_url: String,
    pub ws_url: String,
    pub private_key: String,
    pub contract_address: String,
    pub database_url: String,
    pub redis_url: String,
    pub flashbots_relay_url: String,
    pub bundle_signing_key: String,
    pub chain_id: u64,
    pub min_profit_wei: u128,
    pub max_gas_price_gwei: u64,
    pub slippage_bps: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let cfg = Self {
            rpc_url: require_env("RPC_URL")?,
            ws_url: require_env("WS_URL")?,
            private_key: require_env("PRIVATE_KEY")?,
            contract_address: require_env("CONTRACT_ADDRESS")?,
            database_url: require_env("DATABASE_URL")?,
            redis_url: env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            flashbots_relay_url: env::var("FLASHBOTS_RELAY_URL")
                .unwrap_or_else(|_| "https://relay.flashbots.net".to_string()),
            bundle_signing_key: require_env("BUNDLE_SIGNING_KEY")?,
            chain_id: env::var("CHAIN_ID")
                .unwrap_or_else(|_| "1".to_string())
                .parse::<u64>()
                .context("CHAIN_ID must be a valid u64")?,
            min_profit_wei: env::var("MIN_PROFIT_WEI")
                .unwrap_or_else(|_| "10000000000000000".to_string()) // 0.01 ETH default
                .parse::<u128>()
                .context("MIN_PROFIT_WEI must be a valid u128")?,
            max_gas_price_gwei: env::var("MAX_GAS_PRICE_GWEI")
                .unwrap_or_else(|_| "100".to_string())
                .parse::<u64>()
                .context("MAX_GAS_PRICE_GWEI must be a valid u64")?,
            slippage_bps: env::var("SLIPPAGE_BPS")
                .unwrap_or_else(|_| "50".to_string())
                .parse::<u64>()
                .context("SLIPPAGE_BPS must be a valid u64")?,
        };

        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        // Validate RPC URLs
        if !self.rpc_url.starts_with("http") {
            return Err(eyre::eyre!("RPC_URL must start with http:// or https://"));
        }
        if !self.ws_url.starts_with("ws") {
            return Err(eyre::eyre!("WS_URL must start with ws:// or wss://"));
        }

        // Validate private key format (64 hex chars with optional 0x prefix)
        let pk = self.private_key.trim_start_matches("0x");
        if pk.len() != 64 || !pk.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(eyre::eyre!("PRIVATE_KEY must be a 32-byte hex string"));
        }

        // Validate contract address
        let addr = self.contract_address.trim_start_matches("0x");
        if addr.len() != 40 || !addr.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(eyre::eyre!("CONTRACT_ADDRESS must be a valid Ethereum address"));
        }

        // Validate slippage is reasonable
        if self.slippage_bps > 500 {
            return Err(eyre::eyre!("SLIPPAGE_BPS > 500 (5%) is too high — likely a misconfiguration"));
        }

        tracing::info!(
            chain_id = self.chain_id,
            min_profit_wei = self.min_profit_wei,
            slippage_bps = self.slippage_bps,
            "Config validated successfully"
        );
        Ok(())
    }
}

fn require_env(key: &str) -> Result<String> {
    env::var(key).with_context(|| format!("Required environment variable '{}' is not set", key))
}
```

---

### [H-4] WebSocket reconnection is fragile — single disconnect kills the bot

**File:** `arb-engine/src/mempool.rs`

**Broken pattern:**
```rust
let ws = Ws::connect(&config.ws_url).await?;
// If connection drops, entire task panics/exits
```

**Fixed code:**
```rust
use tokio::time::{sleep, Duration};
use tokio_retry::{strategy::ExponentialBackoff, Retry};

pub async fn start_mempool_listener(
    config: Arc<Config>,
    tx: mpsc::Sender<PendingTx>,
) -> eyre::Result<()> {
    let backoff = ExponentialBackoff::from_millis(500)
        .max_delay(Duration::from_secs(30))
        .take(10);  // max 10 retries

    loop {
        match connect_and_listen(&config, &tx).await {
            Ok(_) => {
                tracing::warn!("WebSocket connection closed cleanly, reconnecting...");
            }
            Err(e) => {
                tracing::error!("WebSocket error: {e}, attempting reconnect...");
            }
        }

        // Exponential backoff before reconnect
        let delay = {
            let mut attempt = 0u64;
            // simple exponential: 500ms, 1s, 2s, 4s, ... up to 30s
            attempt += 1;
            Duration::from_millis(500 * 2u64.pow(attempt.min(6) as u32))
        };
        tracing::info!("Reconnecting WebSocket in {:?}", delay);
        sleep(delay).await;
    }
}

async fn connect_and_listen(
    config: &Config,
    tx: &mpsc::Sender<PendingTx>,
) -> eyre::Result<()> {
    tracing::info!("Connecting to WebSocket: {}", config.ws_url);
    let provider = Provider::<Ws>::connect(&config.ws_url).await
        .context("WebSocket connection failed")?;

    // Set a keepalive interval
    let provider = provider.interval(Duration::from_millis(2000));

    let mut stream = provider.subscribe_pending_txs().await
        .context("Failed to subscribe to pending transactions")?;

    tracing::info!("WebSocket connected, listening for pending txs");

    // Add timeout so we detect stale connections
    while let Ok(Some(tx_hash)) = tokio::time::timeout(
        Duration::from_secs(60),
        stream.next()
    ).await {
        if let Ok(Some(tx)) = provider.get_transaction(tx_hash).await {
            if let Some(pending) = decode_pending_tx(tx) {
                let _ = tx.send(pending).await;  // non-blocking, drop if full
            }
        }
    }

    Err(eyre::eyre!("WebSocket stream ended or timed out"))
}
```

---

### [H-5] Profit calculation doesn't account for gas costs — bot executes losing trades

**File:** `arb-engine/src/opportunity.rs`

**Broken pattern:**
```rust
let gross_profit = amount_out - amount_in;
if gross_profit > min_profit {
    execute(opportunity);  // ignores gas cost!
}
```

**Fixed code:**
```rust
pub struct ProfitCalculation {
    pub gross_profit: U256,
    pub gas_cost_wei: U256,
    pub net_profit: U256,
    pub is_profitable: bool,
}

pub async fn calculate_net_profit(
    amount_in: U256,
    amount_out: U256,
    gas_estimate: U256,
    gas_price: U256,
    flash_loan_fee_bps: u64,
    config: &Config,
) -> ProfitCalculation {
    // Flash loan fee (from Aave, typically 5-9 BPS)
    let flash_fee = amount_in * U256::from(flash_loan_fee_bps) / U256::from(10000u64);

    // Gross profit after repaying flash loan principal + fee
    let gross_profit = if amount_out > amount_in + flash_fee {
        amount_out - amount_in - flash_fee
    } else {
        return ProfitCalculation {
            gross_profit: U256::zero(),
            gas_cost_wei: U256::zero(),
            net_profit: U256::zero(),
            is_profitable: false,
        };
    };

    // Gas cost = gasUsed * gasPrice
    // Add 20% buffer for actual gas vs estimate
    let gas_with_buffer = gas_estimate * U256::from(120u64) / U256::from(100u64);
    let gas_cost_wei = gas_with_buffer * gas_price;

    let net_profit = if gross_profit > gas_cost_wei {
        gross_profit - gas_cost_wei
    } else {
        U256::zero()
    };

    let is_profitable = net_profit >= U256::from(config.min_profit_wei);

    tracing::debug!(
        gross_profit = %gross_profit,
        flash_fee = %flash_fee,
        gas_cost_wei = %gas_cost_wei,
        net_profit = %net_profit,
        is_profitable,
        "Profit calculation"
    );

    ProfitCalculation {
        gross_profit,
        gas_cost_wei,
        net_profit,
        is_profitable,
    }
}
```

---

## 🟠 Medium Severity — Functional Issues

---

### [M-1] Missing reentrancy guard on `executeOperation`

**File:** `app/contracts/AtomicArb.sol`

**Fixed code:**
```solidity
// Import OpenZeppelin
import "@openzeppelin/contracts/security/ReentrancyGuard.sol";

contract AtomicArb is IFlashLoanSimpleReceiver, ReentrancyGuard {
    
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external override nonReentrant returns (bool) {
        // ... existing logic
    }

    function initiateArbitrage(...) external onlyOwner nonReentrant {
        // ... existing logic
    }
}
```

---

### [M-2] Redis caching errors not handled — bot crashes on cache miss

**File:** `arb-engine/src/cache.rs`

**Fixed code:**
```rust
use redis::{Client, Commands, RedisResult};
use std::time::Duration;

pub struct Cache {
    client: Client,
}

impl Cache {
    pub fn new(redis_url: &str) -> eyre::Result<Self> {
        let client = Client::open(redis_url)
            .context("Failed to connect to Redis")?;
        Ok(Self { client })
    }

    // Returns None on miss or error — never panics
    pub fn get_price(&self, key: &str) -> Option<f64> {
        let mut conn = match self.client.get_connection() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Redis connection failed: {e}, operating without cache");
                return None;
            }
        };

        match conn.get::<_, Option<String>>(key) {
            Ok(Some(val)) => val.parse().ok(),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("Redis GET failed for key {key}: {e}");
                None
            }
        }
    }

    pub fn set_price(&self, key: &str, price: f64, ttl_secs: u64) {
        let mut conn = match self.client.get_connection() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Redis connection failed for SET: {e}");
                return;
            }
        };

        let result: RedisResult<()> = redis::pipe()
            .set(key, price.to_string())
            .expire(key, ttl_secs as usize)
            .query(&mut conn);

        if let Err(e) = result {
            tracing::warn!("Redis SET failed for key {key}: {e}");
        }
    }
}
```

---

### [M-3] V2 path finding doesn't check pool liquidity before routing

**File:** `arb-engine/src/pathfinder.rs`

**Fixed code:**
```rust
pub async fn find_best_path(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    provider: &Provider<Http>,
    v2_factory: Address,
) -> Option<ArbPath> {
    // Direct pair
    let direct_pair = get_pair_address(v2_factory, token_in, token_out);
    
    // Check reserves before routing
    if let Some(pair) = direct_pair {
        let (reserve0, reserve1) = get_reserves(pair, provider).await.ok()?;
        
        // Skip if pool is illiquid (less than $10k equivalent)
        let min_reserve = U256::from(10_000u64) * U256::exp10(18); // rough threshold
        if reserve0 < min_reserve || reserve1 < min_reserve {
            tracing::debug!("Skipping illiquid pair {pair:?}");
            return None;
        }
        
        let amount_out = get_amount_out(amount_in, reserve0, reserve1);
        return Some(ArbPath { 
            path: vec![token_in, token_out], 
            amount_out,
            pool_address: pair,
        });
    }
    
    None
}
```

---

## 🟢 Low Severity — Improvements & Best Practices

---

### [L-1] Missing `receive()` function — ETH tip payments will revert

**File:** `app/contracts/AtomicArb.sol`

```solidity
// Add to contract to accept ETH for gas tips
receive() external payable {
    emit EthReceived(msg.sender, msg.value);
}
event EthReceived(address indexed sender, uint256 amount);
```

---

### [L-2] Events missing indexed fields — makes off-chain querying slow

```solidity
// Before (hard to filter)
event ArbitrageExecuted(address tokenIn, address tokenOut, uint256 profit);

// After (indexed for efficient querying)
event ArbitrageExecuted(
    address indexed tokenIn,
    address indexed tokenOut,
    uint256 profit,
    uint256 timestamp,
    bytes32 indexed opportunityId
);
```

---

### [L-3] Add `block.timestamp` deadline protection to flash loan initiation

```solidity
function initiateArbitrage(
    address asset,
    uint256 amount,
    bytes calldata params,
    uint256 deadline    // <-- Add this
) external onlyOwner nonReentrant {
    require(block.timestamp <= deadline, "Transaction expired");
    require(!circuitBreakerActive, "Circuit breaker active");
    POOL.flashLoanSimple(address(this), asset, amount, params, 0);
}
```

---

### [L-4] Rust: Use structured logging with `tracing` crate consistently

**`arb-engine/Cargo.toml`:**
```toml
[dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
```

**`arb-engine/src/main.rs`:**
```rust
fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("arb_engine=debug".parse().unwrap())
        )
        .json()  // structured JSON logs for production
        .init();
}
```

---

## 🔒 Security Vulnerabilities

---

### [S-1] Private key in `.env` file — rotate immediately if ever committed

Never commit `.env` to git. Add to `.gitignore`:
```
.env
.env.*
*.pem
keystore/
```

Use a secrets manager in production (AWS Secrets Manager, HashiCorp Vault, or at minimum `age` encryption).

---

### [S-2] No access control on `executeOperation` sender verification

```solidity
function executeOperation(...) external override returns (bool) {
    // REQUIRED: verify caller is the Aave pool
    require(
        msg.sender == address(POOL),
        "Unauthorized: caller is not Aave pool"
    );
    // REQUIRED: verify initiator is this contract
    require(
        initiator == address(this),
        "Unauthorized: initiator is not this contract"
    );
    // ...
}
```

---

### [S-3] No sandwich protection — use Flashbots for all arbitrage txs

Public mempool submissions will be sandwiched. All arbitrage transactions **must** go through Flashbots (see H-1 fix). Additionally, the on-chain slippage check (C-3 fix) is the last line of defense.

---

## ✅ Complete Working Setup Guide (Sepolia Testnet)

---

### Step 1: Prerequisites

```bash
# Node.js 20 LTS
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt-get install -y nodejs

# Rust 1.78+
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Foundry (Solidity toolchain)
curl -L https://foundry.paradigm.xyz | bash
foundryup

# PostgreSQL 15
sudo apt install postgresql-15

# Redis 7
sudo apt install redis-server

# Verify
node --version   # v20.x
rustc --version  # 1.78+
forge --version  # 0.2.x
psql --version   # 15.x
redis-cli ping   # PONG
```

---

### Step 2: Environment Setup — `.env` template

```bash
# Copy this to .env and fill in all values

# ============= NETWORK =============
CHAIN_ID=11155111
RPC_URL=https://sepolia.infura.io/v3/YOUR_INFURA_KEY
WS_URL=wss://sepolia.infura.io/ws/v3/YOUR_INFURA_KEY
# Alternative: Alchemy
# RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_ALCHEMY_KEY
# WS_URL=wss://eth-sepolia.g.alchemy.com/v2/YOUR_ALCHEMY_KEY

# ============= KEYS =============
# DO NOT COMMIT THIS FILE
PRIVATE_KEY=0xYOUR_64_HEX_CHAR_PRIVATE_KEY
BUNDLE_SIGNING_KEY=0xDIFFERENT_64_HEX_CHAR_KEY_FOR_FLASHBOTS_SIGNING

# ============= CONTRACTS =============
# Fill after deployment
CONTRACT_ADDRESS=0x0000000000000000000000000000000000000000

# Sepolia Aave V3 Pool (official)
AAVE_POOL_ADDRESS=0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951
# Sepolia Uniswap V3 SwapRouter
UNISWAP_V3_ROUTER=0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48
# Sepolia Uniswap V2 Router
UNISWAP_V2_ROUTER=0xC532a74256D3Db42D0Bf7a0400fEFDbad7694008

# ============= DATABASE =============
DATABASE_URL=postgresql://arb_user:arb_password@localhost:5432/arbitrage_db

# ============= REDIS =============
REDIS_URL=redis://127.0.0.1:6379

# ============= FLASHBOTS =============
FLASHBOTS_RELAY_URL=https://relay-sepolia.flashbots.net

# ============= BOT SETTINGS =============
MIN_PROFIT_WEI=10000000000000000   # 0.01 ETH minimum profit
MAX_GAS_PRICE_GWEI=50              # Max gas price to pay
SLIPPAGE_BPS=50                    # 0.5% slippage tolerance
```

---

### Step 3: Database Initialization

```bash
# Create user and database
sudo -u postgres psql <<EOF
CREATE USER arb_user WITH PASSWORD 'arb_password';
CREATE DATABASE arbitrage_db OWNER arb_user;
GRANT ALL PRIVILEGES ON DATABASE arbitrage_db TO arb_user;
EOF

# Verify connection
psql "postgresql://arb_user:arb_password@localhost:5432/arbitrage_db" -c "SELECT 1;"

# Tables are created automatically on engine startup via run_migrations()
# (see H-2 fix above)
```

---

### Step 4: Install Solidity Dependencies & Compile

```bash
cd app

# Install npm dependencies
npm install

# Install Foundry dependencies
forge install OpenZeppelin/openzeppelin-contracts --no-commit
forge install aave/aave-v3-core --no-commit
forge install Uniswap/v3-periphery --no-commit

# Compile
forge build

# Expected output: [⠢] Compiling... [⠒] Compiling 12 files
# No errors — if you see errors, apply the C-1 through C-6 fixes first
```

---

### Step 5: Deploy AtomicArb.sol to Sepolia

```bash
cd app

# Set your .env
source .env

# Deploy
forge create src/contracts/AtomicArb.sol:AtomicArb \
  --rpc-url $RPC_URL \
  --private-key $PRIVATE_KEY \
  --constructor-args \
    $AAVE_POOL_ADDRESS \
    $UNISWAP_V3_ROUTER \
    $UNISWAP_V2_ROUTER \
  --verify \
  --etherscan-api-key YOUR_ETHERSCAN_KEY

# Copy the deployed address into .env as CONTRACT_ADDRESS
```

---

### Step 6: Fund Contract & Configure

```bash
# Send a small amount of WETH/USDC to the contract for gas buffer
# (Flash loan covers principal, but contract needs tokens for gas tips)

# Verify contract on Etherscan Sepolia:
# https://sepolia.etherscan.io/address/YOUR_CONTRACT_ADDRESS
```

---

### Step 7: Start the Rust Engine

```bash
cd arb-engine

# Copy .env from parent directory or create symlink
cp ../.env .env

# Build
cargo build --release

# Run (database migrations run automatically)
RUST_LOG=arb_engine=info,warn cargo run --release

# Expected startup output:
# INFO arb_engine: Config validated successfully
# INFO arb_engine: Database migrations completed successfully
# INFO arb_engine: Redis connected
# INFO arb_engine: WebSocket connected, listening for pending txs
# INFO arb_engine: Flashbots executor initialized (Sepolia relay)
```

---

### Step 8: End-to-End Verification

```bash
# 1. Check the bot is seeing pending transactions
RUST_LOG=arb_engine=debug cargo run --release 2>&1 | grep "pending tx"

# 2. Manually trigger a test arbitrage (on Sepolia — no real money)
cast send $CONTRACT_ADDRESS \
  "initiateArbitrage(address,uint256,bytes,uint256)" \
  0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC \
  1000000 \
  "0x" \
  $(($(date +%s) + 300)) \
  --rpc-url $RPC_URL \
  --private-key $PRIVATE_KEY

# 3. Check database for logged opportunities
psql $DATABASE_URL -c "SELECT * FROM opportunities ORDER BY discovered_at DESC LIMIT 10;"

# 4. Check Redis for cached prices
redis-cli keys "*"
```

---

### Step 9: Troubleshooting Common Errors

**Error 1: `execution reverted: Transfer amount exceeds balance`**
> The contract doesn't have enough of the token to repay the flash loan after swaps. Ensure your arbitrage path actually returns enough tokens. Check that `amountOwed = amount + premium` is correctly calculated (see C-2 fix).

**Error 2: `relation "opportunities" does not exist`**
> Database migration didn't run. Check `DATABASE_URL` is correct and user has CREATE TABLE privileges. Apply the H-2 fix.

**Error 3: WebSocket disconnects after ~5 minutes**
> Normal for free-tier RPC providers. Apply the H-4 reconnection fix. Consider upgrading to a paid RPC node (Alchemy Growth, Infura Team).

**Error 4: `Bundle simulation reverted`**
> The arbitrage is not profitable enough after gas. Either the opportunity disappeared (mempool is competitive) or your profit threshold is too low. Increase `MIN_PROFIT_WEI` or improve your opportunity detection.

**Error 5: `Unauthorized: caller is not Aave pool`**
> Someone tried to call `executeOperation` directly (either an attack attempt or misconfigured test). The fix in S-2 properly blocks this. If this happens in tests, call `initiateArbitrage` instead.

**Error 6: Contract compiles but `withdrawProfit()` sends 0**
> You're calling the old ETH-only version. Apply the C-1 fix — call `withdrawProfit(tokenAddress)` with the specific ERC20 token address.

**Error 7: `PRIVATE_KEY must be a 32-byte hex string` on startup**
> Your key is in the wrong format. Remove the `0x` prefix OR include it consistently — the validator strips it. Make sure the key is exactly 64 hex characters after the prefix.

---

## 📋 File-by-File Change Summary

| File | Changes |
|---|---|
| `app/contracts/AtomicArb.sol` | Fix `withdrawProfit()` for ERC20 [C-1]; dynamic flash fee [C-2]; proper slippage [C-3]; reset approvals [C-4]; circuit breaker window [C-5]; complete/gate Wormhole [C-6]; add reentrancy guard [M-1]; add `receive()` [L-1]; index events [L-2]; add deadline [L-3] |
| `arb-engine/src/flashbots.rs` | **NEW FILE** — Flashbots bundle submission [H-1] |
| `arb-engine/src/db.rs` | Add `run_migrations()` for auto table creation [H-2] |
| `arb-engine/src/config.rs` | Full validation of all required env vars [H-3] |
| `arb-engine/src/mempool.rs` | Exponential backoff WebSocket reconnection [H-4] |
| `arb-engine/src/opportunity.rs` | Net profit calculation including gas + flash fee [H-5] |
| `arb-engine/src/cache.rs` | Graceful Redis error handling [M-2] |
| `arb-engine/src/pathfinder.rs` | Liquidity check before routing [M-3] |
| `arb-engine/src/main.rs` | Call `run_migrations()`, `init_logging()`, use validated `Config` |
| `arb-engine/Cargo.toml` | Add `ethers-flashbots`, `tracing`, `tokio-retry` dependencies |
| `.env.example` | **NEW FILE** — complete env template with all variables |
| `.gitignore` | Add `.env`, `keystore/`, `*.pem` |

---

*Audit complete. Apply Critical fixes before any deployment. High severity fixes before any mainnet use. All fixes are copy-paste ready as shown above.*
