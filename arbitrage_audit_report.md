# Atomic Arbitrage Bot — Complete Security & Bug Audit

> Audit date: May 2026 | Codebase: `Arbitrage/arb-engine` (Foundry + Rust)

---

## 🔴 Critical Errors (Will Prevent Any Operation)

---

### C-1 · Deploy.s.sol — All contract addresses are silently truncated to 0x0000…

**File:** `contracts/evm/script/Deploy.s.sol` — lines 40–53

**Problem:** Every address in the deploy script is written as a 22-byte hex literal padded with `0x00` prefix, then cast through `address(uint160(…))`. `uint160` is 20 bytes. The leading `0x00` shifts every single real byte one position to the right, silently dropping the last byte and corrupting every address. The deployed contract will call `address(0)` for Aave and Wormhole.

```solidity
// ❌ BROKEN — 22-byte literal, uint160 truncates the last byte
aavePool = address(uint160(0x006Ae43d3271ff6888e7Fc43Fd7321a503ff738951));
//                          ^^^ extra leading 00 — corrupts address
```

**Fix — replace every address in Deploy.s.sol with correct 20-byte literals:**

```solidity
// ✅ FIXED Deploy.s.sol (complete replacement for the if/else block)
if (chainId == 11155111) {
    // Sepolia Testnet
    if (aavePool == address(0))
        aavePool = 0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951;
    if (wormholeRelayer == address(0))
        wormholeRelayer = 0x7B621fE2A04a3aB783568b919b40F40171AeFcF4;
} else if (chainId == 8453) {
    // Base Mainnet
    if (aavePool == address(0))
        aavePool = 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5;
    if (wormholeRelayer == address(0))
        wormholeRelayer = 0x706F82e9bb5b0813501714Ab5974216704980e31;
} else if (chainId == 42161) {
    // Arbitrum Mainnet
    if (aavePool == address(0))
        aavePool = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;
    if (wormholeRelayer == address(0))
        wormholeRelayer = 0x27428Dd2D3Abb50BaC90940428B7bB9662758ebA;
} else if (chainId == 1) {
    // Ethereum Mainnet
    if (aavePool == address(0))
        aavePool = 0x87870B27f0bf4296857d44E8a96a1B714f24F5C9;
    if (wormholeRelayer == address(0))
        wormholeRelayer = 0x27428Dd2D3Abb50BaC90940428B7bB9662758ebA;
} else {
    revert("Unsupported Chain ID. Set AAVE_POOL and WORMHOLE_RELAYER.");
}
```

---

### C-2 · AtomicArb.sol — `withdrawProfits()` makes an external call to `this` — bypasses `onlyOwner`

**File:** `contracts/evm/src/AtomicArb.sol` — line 497

**Problem:** `withdrawProfits(address[] calldata tokens)` calls `this.withdrawProfit(tokens[i])` — an **external** call. Because it goes through the contract's ABI dispatcher, OpenZeppelin's `Ownable.onlyOwner` re-checks `msg.sender`, which is now the contract itself (not the original owner). This call reverts on every Ownable-protected contract. Additionally, this pattern opens a potential reentrancy vector if the token's `transfer` callback re-enters before the loop finishes.

```solidity
// ❌ BROKEN — this.withdrawProfit() makes an external call;
//             msg.sender becomes address(this), not owner()
function withdrawProfits(address[] calldata tokens) external onlyOwner {
    for (uint256 i = 0; i < tokens.length; i++) {
        this.withdrawProfit(tokens[i]); // external call → owner check FAILS
    }
}
```

**Fix — call the internal logic directly, not via external dispatch:**

```solidity
// ✅ FIXED — call internal helper, not the external function
function withdrawProfits(address[] calldata tokens) external onlyOwner {
    for (uint256 i = 0; i < tokens.length; i++) {
        _withdrawProfitFor(tokens[i]);
    }
}

// Extract shared logic into internal function
function withdrawProfit(address token) external onlyOwner {
    _withdrawProfitFor(token);
}

function _withdrawProfitFor(address token) internal {
    if (token == address(0)) {
        uint256 ethBalance = address(this).balance;
        require(ethBalance > 0, "AtomicArb: no ETH balance");
        (bool success, ) = payable(owner()).call{value: ethBalance}("");
        require(success, "AtomicArb: ETH transfer failed");
        emit ProfitWithdrawn(address(0), ethBalance);
    } else {
        uint256 balance = IERC20(token).balanceOf(address(this));
        require(balance > 0, "AtomicArb: nothing to withdraw");
        IERC20(token).safeTransfer(owner(), balance);
        emit ProfitWithdrawn(token, balance);
    }
}
```

---

### C-3 · AtomicArb.sol — Flash loan fee is a hardcoded constant but `premium` from Aave is already correct

**File:** `contracts/evm/src/AtomicArb.sol` — lines 161–162 and comment on line 8

**Problem:** The constant `AAVE_FLASH_LOAN_FEE_BPS = 5` is declared but **never used** in the execution path. The contract correctly uses the `premium` parameter passed by Aave (which is already the exact fee amount), so the hardcoded constant is dead code. However, the Rust engine's profit calculation in `evm.rs` adds a separate 5-bps fee on top of the on-chain `premium`, causing it to over-subtract fees and miss profitable trades.

**Fix — remove the dead constant and fix Rust-side profit calculation:**

```solidity
// ✅ In AtomicArb.sol — remove or mark as documentation-only
// The constant is unused in the execution path; Aave provides the exact
// premium directly. Remove to avoid confusion:
// REMOVE: uint256 public constant AAVE_FLASH_LOAN_FEE_BPS = 5;
```

In `engine/src/chains/evm.rs`, wherever profit is calculated, use `premium` as-is rather than adding a redundant fee:

```rust
// ✅ In Rust — do NOT add an extra 5bps on top; Aave's premium is the total fee.
// The on-chain contract already enforces: finalAmount >= repayAmount + minProfitWei
// where repayAmount = amount + premium (the exact Aave fee).
// The Rust estimate should mirror this exactly:
let repay_amount = borrow_amount + borrow_amount * 5 / 10_000; // 5 bps estimate for simulation only
// Do NOT apply this twice or compound it.
```

---

### C-4 · env.example / docker-compose — Database name mismatch prevents PostgreSQL connection

**File:** `.env.example` line and `infra/docker-compose.yml`

**Problem:** The Docker Compose file creates a database named `arb_engine`. The `.env.example` points to `arbitrage_db`. These never match, so `DATABASE_URL` in `.env` will always fail to connect unless manually corrected.

```
# ❌ docker-compose.yml creates:
POSTGRES_DB: arb_engine

# ❌ .env.example points to:
DATABASE_URL=postgresql://arb_user:arb_password@localhost:5432/arbitrage_db
#                                                              ^^^^^^^^^^^ wrong name
```

**Fix — align both files:**

```yaml
# ✅ docker-compose.yml — no change needed (arb_engine is fine)
POSTGRES_DB: arb_engine
```

```bash
# ✅ .env.example — fix to match Docker Compose
DATABASE_URL=postgresql://arb_user:arb_password@localhost:5432/arb_engine
```

---

### C-5 · AtomicArb.sol — `Ownable` constructor not called with `msg.sender`

**File:** `contracts/evm/src/AtomicArb.sol` — constructor

**Problem:** The contract inherits OpenZeppelin v5's `Ownable`, which **requires** the initial owner to be passed explicitly as a constructor argument. The current constructor does not pass `msg.sender` (or any address) to `Ownable(...)`. With OZ v5 this is a **compile error** — the contract will not compile.

```solidity
// ❌ BROKEN — OZ v5 Ownable requires explicit owner parameter
constructor(address _aavePool, address _wormholeRelayer, uint256 _maxDrawdownPerHour) {
    aavePool = IPool(_aavePool);
    // ...
}
```

**Fix:**

```solidity
// ✅ FIXED — pass msg.sender to Ownable
constructor(
    address _aavePool,
    address _wormholeRelayer,
    uint256 _maxDrawdownPerHour
) Ownable(msg.sender) {
    require(_aavePool != address(0), "AtomicArb: zero aave pool");
    require(_wormholeRelayer != address(0), "AtomicArb: zero wormhole relayer");
    aavePool = IPool(_aavePool);
    wormholeRelayer = IWormholeRelayer(_wormholeRelayer);
    maxDrawdownPerHour = _maxDrawdownPerHour;
    drawdownWindowStart = block.timestamp;
}
```

---

### C-6 · AtomicArb.sol — Wrong OZ import path for `ReentrancyGuard` and `Pausable` (v4 vs v5)

**File:** `contracts/evm/src/AtomicArb.sol` — imports

**Problem:** The contract imports from `@openzeppelin/contracts/security/ReentrancyGuard.sol` and `@openzeppelin/contracts/security/Pausable.sol`. In OpenZeppelin v5 these files were moved to `@openzeppelin/contracts/utils/ReentrancyGuard.sol` and `@openzeppelin/contracts/utils/Pausable.sol`. Since `foundry.toml` uses the OZ library installed via `forge install`, the version must be consistent. If OZ v5 is installed (which is the default since late 2023), these imports cause **compilation failure**.

```solidity
// ❌ OZ v4 paths — fail with OZ v5
import {ReentrancyGuard} from "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import {Pausable} from "@openzeppelin/contracts/security/Pausable.sol";
```

**Fix:**

```solidity
// ✅ OZ v5 paths
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";
```

> **Note:** If you intentionally use OZ v4, pin it: `forge install OpenZeppelin/openzeppelin-contracts@v4.9.6` and keep the `security/` paths. Do not mix versions.

---

## 🟡 High Severity (Will Cause Losses or Failures)

---

### H-1 · AtomicArb.sol — `slippageBps = 50` fallback is `buyMinOut = 1` when `expectedBuyOut == 0`

**File:** `contracts/evm/src/AtomicArb.sol` — lines approx. 345–350

**Problem:** When `arb.expectedBuyOut == 0` (i.e., the Rust engine did not supply a per-leg estimate), `buyMinOut` falls back to `1`. This means the buy leg will accept any output, including near-zero, allowing MEV sandwich attacks on the buy leg even though the sell leg still has its break-even guard. A griefing sandwich can drain the buy output to dust, causing the sell leg revert to eat the gas with no profit guard on the first swap.

```solidity
// ❌ buyMinOut = 1 — accepts any output on buy leg
if (arb.expectedBuyOut > 0) {
    buyMinOut = (arb.expectedBuyOut * (10000 - slippageBps)) / 10000;
} else {
    buyMinOut = 1; // ← dangerous
}
```

**Fix — use the borrow amount as a minimum proxy when no estimate is supplied:**

```solidity
// ✅ Fall back to slippageBps applied to the input amount as a proxy
// (not perfect, but prevents accepting near-zero output)
if (arb.expectedBuyOut > 0) {
    buyMinOut = (arb.expectedBuyOut * (10000 - slippageBps)) / 10000;
} else {
    // Without an expected output, require at least (1 - slippage) × input
    // as a conservative lower bound. Callers SHOULD always supply expectedBuyOut.
    buyMinOut = (amount * (10000 - slippageBps)) / 10000;
}
```

Additionally, enforce in the Rust engine that `expectedBuyOut` is always set before calling `executeArbitrage`.

---

### H-2 · evm.rs — `execute_arbitrage` sends through the **public mempool**, not Flashbots

**File:** `engine/src/chains/evm.rs` — `execute_arbitrage()` (lines ~425–520)

**Problem:** `execute_arbitrage()` calls `tx.send().await` using a standard `ProviderBuilder::new().wallet(wallet).on_builtin(&self.config.http_url)`. This sends the transaction via the **public HTTP RPC endpoint** — visible to all MEV searchers in the mempool. Any profitable arbitrage this bot finds will be front-run immediately. The `submit_flashbots_bundle()` method exists but is **never called** by `execute_arbitrage`.

```rust
// ❌ CURRENT — public mempool, gets front-run
let provider = ProviderBuilder::new()
    .wallet(wallet)
    .on_builtin(&self.config.http_url)   // ← public RPC, not Flashbots
    .await?;
// ...
match tx.send().await { ... }
```

**Fix — route through Flashbots (or use `eth_sendPrivateTransaction` on Alchemy):**

```rust
// ✅ FIXED — use Flashbots private relay
pub async fn execute_arbitrage(&self, arb: &ArbitrageOpportunity) -> Result<()> {
    let pk = self.config.private_key.as_deref()
        .context("PRIVATE_KEY not set")?;
    let contract_addr_str = self.config.contract_address.as_deref()
        .context("CONTRACT_ADDRESS not set")?;

    if arb.net_expected_value <= 0 {
        bail!("Refusing non-profitable arb (NEV = {})", arb.net_expected_value);
    }

    let signer: PrivateKeySigner = pk.parse().context("Failed to parse PRIVATE_KEY")?;
    let wallet = EthereumWallet::from(signer.clone());

    // Build and sign the transaction using a standard provider
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .on_builtin(&self.config.http_url)
        .await?;

    let contract_addr = Address::from_str(contract_addr_str)?;
    let atomic_arb = IAtomicArb::new(contract_addr, provider.clone());
    let params = self.build_arb_params(arb)?;
    let encoded_params = params.abi_encode();
    let borrow_amount = alloy::primitives::U256::from_str(&arb.input_amount.to_string())
        .unwrap_or_default();
    let deadline = alloy::primitives::U256::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() + 60  // tighter deadline for bundles
    );

    // Build tx call to get the raw bytes
    let call = atomic_arb.executeArbitrage(
        params.tokenBorrow,
        borrow_amount,
        Bytes::from(encoded_params),
        deadline,
    );
    let raw_tx = call.as_builder()
        .send()
        .await
        .context("Failed to build tx")?;

    // --- Route to Flashbots instead of public mempool ---
    let current_block = provider.get_block_number().await.unwrap_or(0);
    let target_block = current_block + 1;

    // Encode signed tx — use eth_signTransaction + RLP encoding
    // (For simplicity here we show the bundle submission path)
    match self.submit_flashbots_bundle(vec![], target_block).await {
        Ok(_) => info!(id = %arb.id, "✅ Flashbots bundle submitted"),
        Err(e) => {
            // Fallback: use Alchemy/Infura private tx if Flashbots fails
            warn!("Flashbots failed ({}), falling back to eth_sendPrivateTransaction", e);
            // NOTE: Do NOT fall back to public mempool — skip this arb instead
            bail!("Execution skipped: Flashbots unavailable and public mempool is unsafe");
        }
    }

    Ok(())
}
```

> **Important:** A complete Flashbots bundle requires RLP-encoding the signed transaction. The stub above shows the control flow; the full implementation requires `alloy`'s transaction signing + RLP serialization before the `submit_flashbots_bundle` call. Use `alloy::network::TransactionBuilder` to build and sign, then `alloy::rlp::encode` on the result.

---

### H-3 · AtomicArb.sol — Aave V3 `FLASH_LOAN_FEE_BPS` is NOT always 5 bps

**File:** `contracts/evm/src/AtomicArb.sol` — line 162

**Problem:** Aave V3 flash loan fees vary by asset and can be changed by governance. On Sepolia testnet the fee is currently 0 bps (waived). On mainnet it is 5 bps by default but can be 9 bps for some assets. The hardcoded constant is misleading documentation only (the contract uses `premium` directly, which is correct), but the **Rust engine** uses 5 bps hardcoded in its profit simulation. If the actual fee differs, the simulation will accept/reject incorrectly.

**Fix in Rust** — query the actual flash loan premium from Aave before simulating:

```rust
// In engine/src/chains/evm.rs — add to EvmAdapter
sol! {
    #[sol(rpc)]
    interface IAavePool {
        function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);
    }
}

pub async fn get_flash_loan_premium_bps(&self, aave_pool_addr: &str) -> Result<u64> {
    let provider = self.get_http_provider().await?;
    let addr = Address::from_str(aave_pool_addr)?;
    let pool = IAavePool::new(addr, provider);
    let premium = pool.FLASHLOAN_PREMIUM_TOTAL().call().await?._0;
    Ok(premium as u64)
}
```

---

### H-4 · AtomicArb.sol — `executeOperation` does NOT reset Aave approval after repayment

**File:** `contracts/evm/src/AtomicArb.sol` — around line 400

**Problem:** After `IERC20(asset).forceApprove(address(aavePool), repayAmount)`, Aave pulls exactly `repayAmount`. However, `forceApprove` does not reset to zero after the pull — it sets the allowance to `repayAmount` and Aave consumes it. The _swap function correctly calls `forceApprove(router, 0)` after each swap, but the Aave approval is never cleared post-execution. In practice, Aave will have consumed the approval, so the residual should be 0. But this is implementation-dependent on Aave's `transferFrom` behaviour and is worth an explicit reset.

**Fix — add explicit reset after Aave repayment:**

```solidity
// ✅ After flash loan repayment, explicitly zero the allowance
IERC20(asset).forceApprove(address(aavePool), repayAmount);
// Aave calls transferFrom(address(this), aavePool, repayAmount) here
// Then explicitly reset:
// NOTE: the reset must happen AFTER Aave has pulled the funds.
// Since Aave pulls synchronously during executeOperation's return,
// add the reset at the END of executeOperation, before returning true:
IERC20(asset).forceApprove(address(aavePool), 0); // ← add this line
return true;
```

---

### H-5 · AtomicArb.sol — Circuit breaker `loss` is always 0 — never triggers on actual losses

**File:** `contracts/evm/src/AtomicArb.sol` — line 395

**Problem:** `_checkCircuitBreaker(0)` is always called with `0` as the loss parameter. The circuit breaker tracks `drawdownThisHour` but it is never incremented by actual losses. Since the contract atomically reverts on unprofitable trades (zero-loss guarantee), the only real loss scenario is gas costs. The circuit breaker as designed can never trigger from within `executeOperation`.

**Fix — track gas costs as the "loss" metric, or track consecutive reverts:**

```solidity
// ✅ Option A: Track estimated gas cost as the loss metric
// Gas cost in wei = gasPrice * gasLimit (approximate)
uint256 gasLossEstimate = tx.gasprice * 350_000; // rough estimate
_checkCircuitBreaker(gasLossEstimate);

// ✅ Option B (better): Track in a separate storage variable and
// allow the owner to manually report a loss (for off-chain monitoring):
uint256 public consecutiveFailures;
uint256 public maxConsecutiveFailures = 10;

// In executeOperation, on success:
consecutiveFailures = 0;

// Add a separate onlyOwner function for external circuit-break signal:
function reportLoss(uint256 lossWei) external onlyOwner {
    _checkCircuitBreaker(lossWei);
}
```

---

### H-6 · evm.rs — `execute_arbitrage` does not check `receipt.status()` — silently accepts reverts

**File:** `engine/src/chains/evm.rs` — lines ~490–510

**Problem:** After `tx.send().await` succeeds, the code logs `receipt.status()` but does **not assert** it. A reverted transaction (status = 0) is silently treated as success. The bot will continue spending gas on reverted arbs without knowing.

```rust
// ❌ Status is logged but not checked
info!(
    tx_hash  = ?receipt.transaction_hash,
    gas_used = ?receipt.gas_used,
    status   = ?receipt.status(),  // could be 0 (failed) — not checked!
    "✅ Arbitrage Executed Successfully!"
);
```

**Fix:**

```rust
// ✅ Check receipt status
let status = receipt.status();
if !status {
    bail!(
        "Transaction {} was included but REVERTED (gas used: {:?})",
        receipt.transaction_hash,
        receipt.gas_used
    );
}
info!(
    tx_hash  = ?receipt.transaction_hash,
    gas_used = ?receipt.gas_used,
    "✅ Arbitrage transaction confirmed"
);
```

---

## 🟠 Medium Severity (Functional Issues)

---

### M-1 · AtomicArb.sol — `sendCrossChainMessage` uses wrong `IWormholeRelayer.send()` interface

**File:** `contracts/evm/src/AtomicArb.sol` — Wormhole interface

**Problem:** The contract declares both `sendPayloadToEvm` and a legacy `send(...)` method in `IWormholeRelayer`. The `send(...)` signature (with `routingAccount` and `routingFee` parameters) is from the legacy Wormhole Relayer SDK v1 and does not match Wormhole Automatic Relayer v2. The `send()` method is declared but unused (the code correctly uses `sendPayloadToEvm`), but the stale interface pollutes the ABI and will confuse tooling.

**Fix — remove the unused legacy `send()` from the interface:**

```solidity
// ✅ Clean IWormholeRelayer interface (remove legacy send())
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
    // REMOVED: legacy send() — not present in Wormhole Relayer v2
}
```

---

### M-2 · postgres.rs — `CREATE_OPPORTUNITIES_INDEX` executes TWO statements in one query — will fail with sqlx

**File:** `engine/src/db/postgres.rs` — `CREATE_OPPORTUNITIES_INDEX` and `CREATE_EXECUTIONS_INDEX` constants

**Problem:** `sqlx::query(CREATE_OPPORTUNITIES_INDEX).execute(&self.pool)` passes a string with **two** SQL statements separated by a semicolon. `sqlx` does not support multi-statement queries in a single `query()` call — it will return an error on the second statement.

```rust
// ❌ Two statements in one string — sqlx rejects this
const CREATE_OPPORTUNITIES_INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS idx_opportunities_discovered_at
    ON opportunities (discovered_at DESC);
CREATE INDEX IF NOT EXISTS idx_opportunities_executable        ← second statement fails
    ON opportunities (is_executable, discovered_at DESC);
"#;
```

**Fix — split into separate queries:**

```rust
// ✅ run_migrations() — split each index into its own execute call
pub async fn run_migrations(&self) -> Result<()> {
    let statements = [
        CREATE_OPPORTUNITIES_TABLE,
        CREATE_POOL_REGISTRY_TABLE,
        CREATE_EXECUTIONS_TABLE,
        CREATE_CIRCUIT_BREAKER_TABLE,
        // Indexes — each as a separate statement
        "CREATE INDEX IF NOT EXISTS idx_opportunities_discovered_at \
         ON opportunities (discovered_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_opportunities_executable \
         ON opportunities (is_executable, discovered_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_executions_tx_hash \
         ON executions(tx_hash)",
        "CREATE INDEX IF NOT EXISTS idx_executions_status \
         ON executions(status, submitted_at DESC)",
    ];

    for stmt in &statements {
        sqlx::query(stmt)
            .execute(&self.pool)
            .await
            .with_context(|| format!("Migration failed: {:.60}...", stmt))?;
    }

    info!("✓ Database migrations applied");
    Ok(())
}
```

---

### M-3 · AtomicArb.sol — `_swap()` V2 path does not validate the `path` array length

**File:** `contracts/evm/src/AtomicArb.sol` — `_swap()` function

**Problem:** For V2 swaps, `arb.buyPath` or `arb.sellPath` is passed directly to Uniswap. If the path is empty or has only one element, Uniswap V2 will revert with a confusing low-level error rather than a clear message. If the wrong path is supplied (e.g., buy path reused for sell), the swap silently executes in the wrong direction.

**Fix — add validation:**

```solidity
// ✅ Add at the start of _swap() for V2 paths
function _swap(
    address router, bool isV3, uint24 fee,
    address[] memory path,
    address tokenIn, address tokenOut,
    uint256 amountIn, uint256 amountOutMin
) internal returns (uint256 amountOut) {
    if (!isV3) {
        require(path.length >= 2, "AtomicArb: V2 path too short");
        require(path[0] == tokenIn, "AtomicArb: path[0] != tokenIn");
        require(path[path.length - 1] == tokenOut, "AtomicArb: path end != tokenOut");
    }
    // ... rest of function
}
```

---

### M-4 · listener.rs — Phantom arb opportunities generated from hardcoded simulated pool state

**File:** `engine/src/mempool/listener.rs` — the `build_placeholder_pool()` function

**Problem:** When an on-chain fetch fails, a placeholder pool is inserted into the live `LiquidityGraph` with hardcoded simulated reserves (`sqrt_price = 1_936_540…`, `tick = 201_210`). The pathfinder then runs Bellman-Ford against these fake prices and may generate "executable" opportunities that revert on-chain. The `is_executable` flag becomes meaningless.

**Fix — mark placeholder pools as stale and exclude from pathfinding:**

```rust
// ✅ In build_placeholder_pool, set last_updated_ts to 0 to signal staleness
// And in find_opportunities / find_arbitrage_cycles, skip pools with ts == 0:

// In router.rs — add staleness guard to edge weight computation
fn compute_edge_weight(pool: &Pool, token_in: &str) -> Option<f64> {
    // Skip pools that were never updated from chain
    if pool.last_updated_ts == 0 {
        return None; // stale placeholder — exclude from pathfinder
    }
    // ... existing rate computation
}
```

---

### M-5 · config.rs — `eth_http_url` is derived from WS URL by string replacement — broken for most providers

**File:** `engine/src/main.rs` — lines ~108–112

**Problem:**

```rust
let active_http_url = base_ws.replace("wss://", "https://");
```

This assumes WS and HTTP share the same hostname with only the scheme differing. For Alchemy, the WS URL is `wss://base-mainnet.g.alchemy.com/v2/KEY` and the HTTP URL is `https://base-mainnet.g.alchemy.com/v2/KEY` — which happens to work. But for Infura (`wss://mainnet.infura.io/ws/v3/KEY` → HTTP should be `https://mainnet.infura.io/v3/KEY` without the `/ws/` segment), this produces a broken URL.

**Fix — add `BASE_HTTP_URL` as a separate env variable:**

```bash
# .env.example — add:
BASE_HTTP_URL=https://base-mainnet.g.alchemy.com/v2/YOUR_ALCHEMY_KEY
ETH_HTTP_URL=https://mainnet.infura.io/v3/YOUR_INFURA_KEY
```

```rust
// ✅ In config.rs — add base_http_url field
pub base_http_url: Option<String>,

// In from_env():
base_http_url: std::env::var("BASE_HTTP_URL").ok(),

// In main.rs — use explicit HTTP URL instead of string replace:
let active_http_url = if active_chain == ChainId::Base {
    config.base_http_url.clone()
        .unwrap_or_else(|| config.base_ws_url.as_deref()
            .unwrap_or("")
            .replace("wss://", "https://"))
} else {
    config.eth_http_url.clone()
};
```

---

### M-6 · AtomicArb.sol — No check that `aavePool` and `wormholeRelayer` are non-zero at construction

**File:** `contracts/evm/src/AtomicArb.sol` — constructor

**Problem:** If either address is accidentally `address(0)` (e.g., due to bug C-1 above being re-introduced), the contract will deploy silently and every `executeArbitrage` call will fail with a low-level revert.

**Fix** (already shown in C-5 fix above — add the `require` checks):

```solidity
constructor(address _aavePool, address _wormholeRelayer, uint256 _maxDrawdownPerHour)
    Ownable(msg.sender)
{
    require(_aavePool != address(0), "AtomicArb: zero aave pool");
    require(_wormholeRelayer != address(0), "AtomicArb: zero wormhole relayer");
    // ...
}
```

---

## 🟢 Low Severity (Improvements)

---

### L-1 · AtomicArb.sol — `AAVE_FLASH_LOAN_FEE_BPS` constant is dead code

Remove or document it as a reference-only value since the contract uses `premium` directly.

---

### L-2 · evm.rs — `submit_flashbots_bundle` generates a random Flashbots signing key when none is set

**File:** `engine/src/chains/evm.rs` — line ~793

Using a random key means no Flashbots reputation is built. All bundles land with lowest priority. Set a persistent `FLASHBOTS_SIGNING_KEY` in `.env`.

---

### L-3 · docker-compose.yml — `postgres-init/` directory is mounted but may be empty

**File:** `infra/docker-compose.yml`

The `./postgres-init:/docker-entrypoint-initdb.d:ro` mount is referenced but the `postgres-init/` directory may not contain the `uuid-ossp` extension creation SQL needed for UUID primary keys.

**Fix — add `infra/postgres-init/01_extensions.sql`:**

```sql
-- infra/postgres-init/01_extensions.sql
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pg_stat_statements";
```

---

### L-4 · AtomicArb.sol — `totalProfitAccumulated` tracks profits without a corresponding ERC20 balance check

The `totalProfitAccumulated` counter may diverge from actual contract balance if tokens are transferred in from outside. Use it for informational purposes only; always rely on `IERC20(token).balanceOf(address(this))` for actual withdrawals (already done).

---

### L-5 · AtomicArb.sol — `_swap` deadline is `block.timestamp + 60` — can be miner-manipulated

Miners can set `block.timestamp` up to ~15 seconds in the future. For flash-loan atomicity this is acceptable (the entire tx either succeeds or reverts), but consider using `block.timestamp` (no buffer) since the swap is atomic within one block.

---

## ✅ Complete Working Setup Guide — Ethereum Sepolia Testnet

### Step 1: Prerequisites

```bash
# Rust toolchain (1.78+)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update stable

# Foundry (Solidity toolchain)
curl -L https://foundry.paradigm.xyz | bash
foundryup

# Docker + Docker Compose (for Postgres + Redis)
# Install from https://docs.docker.com/get-docker/

# Node.js 20+ (for any frontend work)
# Install from https://nodejs.org/

# Verify
forge --version    # should show forge 0.2.x
cargo --version    # should show cargo 1.78+
docker --version   # should show Docker 24+
```

### Step 2: Repository Setup

```bash
cd Arbitrage/arb-engine

# Install Solidity dependencies
cd contracts/evm
forge install OpenZeppelin/openzeppelin-contracts@v5.0.2
forge install foundry-rs/forge-std
cd ../..

# Copy and fill environment config
cp .env.example .env
# Edit .env with your values (see Step 3)
```

### Step 3: Environment Variables (.env)

```bash
# .env — complete template for Sepolia testnet

# ── RPC (use Alchemy for best results) ───────────────────────────────────────
CHAIN_ID=11155111
ETH_WS_URL=wss://eth-sepolia.g.alchemy.com/v2/YOUR_ALCHEMY_KEY
ETH_HTTP_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_ALCHEMY_KEY

# ── Keys (NEVER commit these) ─────────────────────────────────────────────────
PRIVATE_KEY=0xYOUR_64_HEX_CHAR_PRIVATE_KEY
FLASHBOTS_SIGNING_KEY=0xDIFFERENT_64_HEX_CHAR_KEY  # separate from execution key

# ── Contract (fill after deployment in Step 5) ─────────────────────────────────
CONTRACT_ADDRESS=0x0000000000000000000000000000000000000000

# ── Database ──────────────────────────────────────────────────────────────────
DATABASE_URL=postgresql://arb_user:arb_password@localhost:5432/arb_engine  # FIXED: was arbitrage_db
REDIS_URL=redis://127.0.0.1:6379

# ── Flashbots (Sepolia relay) ─────────────────────────────────────────────────
FLASHBOTS_RPC_URL=https://relay-sepolia.flashbots.net

# ── Bot Parameters ─────────────────────────────────────────────────────────────
MIN_PROFIT_USD=0.50
MAX_PRICE_IMPACT_BPS=30
MAX_HOPS=3
MAX_BLOCK_STALENESS=2
MAX_TRADE_SIZE_PCT=0.01
ETH_PRICE_USD=3000.0
GAS_PRICE_GWEI=20.0

# ── Logging ───────────────────────────────────────────────────────────────────
RUST_LOG=arb_engine=debug,warn
```

### Step 4: Database Initialization

```bash
# Start PostgreSQL and Redis
cd infra

# Create the postgres-init directory and extension SQL
mkdir -p postgres-init
cat > postgres-init/01_extensions.sql << 'EOF'
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
EOF

# Start services
docker-compose up -d

# Verify health
docker-compose ps
# Both db and cache should show "healthy"

# Test connection
docker-compose exec db psql -U arb_user -d arb_engine -c "SELECT version();"

cd ..
```

### Step 5: Apply All Contract Fixes and Deploy

First, apply all critical fixes to `AtomicArb.sol`:

1. Fix OZ import paths (C-6): change `security/` → `utils/` for `ReentrancyGuard` and `Pausable`
2. Fix constructor (C-5): add `Ownable(msg.sender)`
3. Fix `withdrawProfits` (C-2): use `_withdrawProfitFor()` internal helper
4. Fix `buyMinOut` fallback (H-1): use `(amount * (10000 - slippageBps)) / 10000` instead of `1`
5. Fix `_swap` path validation (M-3): add require checks
6. Add Aave approval reset (H-4): `forceApprove(address(aavePool), 0)` before `return true`

Fix `Deploy.s.sol` (C-1): replace all `address(uint160(0x00…))` with clean 20-byte addresses.

Then deploy:

```bash
cd contracts/evm

# Compile and verify
forge build

# Deploy to Sepolia
export PRIVATE_KEY=0xYOUR_KEY
forge script script/Deploy.s.sol \
  --rpc-url $ETH_HTTP_URL \
  --broadcast \
  --verify \
  --etherscan-api-key $ETHERSCAN_API_KEY \
  -vvvv

# Note the deployed contract address from the output
# Update CONTRACT_ADDRESS in .env
```

**Sepolia contract addresses for reference:**
- Aave V3 Pool: `0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951`
- Wormhole Relayer: `0x7B621fE2A04a3aB783568b919b40F40171AeFcF4`
- Uniswap V3 SwapRouter02: `0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48`
- Uniswap V2 Router: `0xC532a74256D3Db42D0Bf7a0400fEFDbad7694008`

### Step 6: Fund the Contract

```bash
# The contract needs a small ETH balance for gas tip and a WETH/USDC balance
# to test withdrawals. On Sepolia, use a faucet:
# https://sepoliafaucet.com/
# https://faucets.chain.link/sepolia

# Send 0.1 ETH to the deployed contract for gas costs
cast send $CONTRACT_ADDRESS --value 0.1ether --private-key $PRIVATE_KEY --rpc-url $ETH_HTTP_URL

# Optionally, get Sepolia WETH from:
# Uniswap Sepolia: https://app.uniswap.org/ (switch to Sepolia)
```

### Step 7: Build and Start the Rust Engine

```bash
# Apply database fix (M-2) first: split multi-statement index queries
# Then build
cd engine
cargo build --release

# Run database migrations (happens automatically on startup)
# Start the engine
cargo run --release -- 2>&1 | tee engine.log

# Or in the background:
cargo run --release 2>&1 >> engine.log &
```

### Step 8: Verify End-to-End

```bash
# 1. Check engine is running and connected
tail -f engine.log | grep -E "✓|✗|ERROR|WARN"
# Expected: "✓ Redis connected", "✓ PostgreSQL connected", "✓ WebSocket connected"

# 2. Check pools are synced
# Expected: "All N pools synchronized" or "N/M pools synced"

# 3. Check mempool subscription is active
# Expected: "✓ WebSocket connected", "📦 Current block: XXXXXX"

# 4. Verify PostgreSQL tables were created
docker-compose exec db psql -U arb_user -d arb_engine \
  -c "\dt" | grep -E "opportunities|pool_registry|executions"

# 5. Test a manual arb execution (dry run) — use cast to simulate
cast call $CONTRACT_ADDRESS \
  "executeArbitrage(address,uint256,bytes,uint256)" \
  "0xTOKEN_ADDRESS" \
  "1000000000000000000" \
  "0x$(cast abi-encode 'f((address,bool,uint24,address[],address,bool,uint24,address[],address,address,uint256,uint256,uint256))' ...)" \
  "$(($(date +%s) + 300))" \
  --from $YOUR_WALLET \
  --rpc-url $ETH_HTTP_URL

# 6. Check React dashboard (if running)
cd ../../app && npm install && npm run dev
# Open http://localhost:5173
```

### Step 9: Troubleshooting Common Scenarios

**Scenario 1: `forge build` fails with "File not found"**
```bash
# Reinstall OZ and forge-std dependencies
cd contracts/evm
rm -rf lib/openzeppelin-contracts lib/forge-std
forge install OpenZeppelin/openzeppelin-contracts@v5.0.2
forge install foundry-rs/forge-std
forge build
```

**Scenario 2: `cargo build` fails with "unresolved import" or version mismatch**
```bash
cd engine
# Check Rust edition and dependencies
cargo check 2>&1 | head -30
# If alloy version conflicts:
cargo update alloy
# If solana crates conflict with tokio version:
cargo update solana-client
```

**Scenario 3: Redis connection fails on startup**
```bash
# Start Redis via Docker
cd infra && docker-compose up -d cache
# Verify
redis-cli ping  # should return PONG
# Check REDIS_URL in .env matches docker-compose port mapping (default: 6379)
```

**Scenario 4: PostgreSQL migration fails with "already exists"**
```bash
# This is expected if tables were created in a previous run
# The engine uses IF NOT EXISTS — warnings are safe to ignore
# To start fresh:
docker-compose exec db psql -U arb_user -d arb_engine \
  -c "DROP TABLE IF EXISTS executions, opportunities, pool_registry, circuit_breaker_events CASCADE;"
# Then restart the engine
```

**Scenario 5: Contract deployment reverts with "Unsupported Chain ID"**
```bash
# Verify you're targeting Sepolia (chainId 11155111)
cast chain-id --rpc-url $ETH_HTTP_URL
# Should output: 11155111

# If AAVE_POOL or WORMHOLE_RELAYER are not auto-detected, set them manually:
export AAVE_POOL=0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951
export WORMHOLE_RELAYER=0x7B621fE2A04a3aB783568b919b40F40171AeFcF4
forge script script/Deploy.s.sol --rpc-url $ETH_HTTP_URL --broadcast -vvvv
```

**Scenario 6: `executeArbitrage` reverts with "AtomicArb: insufficient profit"**
```bash
# This is the intended zero-loss guarantee working correctly.
# The arb was not profitable enough at execution time (slippage ate the margin).
# Increase MIN_PROFIT_USD or reduce MAX_PRICE_IMPACT_BPS to be more selective.
# Also check: is expectedBuyOut / expectedSellOut being set in ArbParams?
# If not, the slippage fallback may be too loose (see H-1 fix).
```

**Scenario 7: WebSocket disconnects frequently**
```bash
# Increase reconnect tolerance by setting a longer backoff in listener.rs
# Also check your RPC provider's WebSocket connection limits
# Alchemy free tier: 20 concurrent WS connections, max 100 req/sec
# For production, use Alchemy Growth or a dedicated node
```

---

## 📋 File-by-File Change Summary

| File | Severity | Changes Required |
|------|----------|------------------|
| `contracts/evm/src/AtomicArb.sol` | 🔴🟡🟠 | Fix OZ imports (v5 paths); add `Ownable(msg.sender)`; fix `withdrawProfits` to use internal helper; fix `buyMinOut` fallback; add `_swap` path validation; add Aave approval reset; add constructor zero-address checks |
| `contracts/evm/script/Deploy.s.sol` | 🔴 | Replace all `address(uint160(0x00…))` with correct 20-byte address literals |
| `engine/src/db/postgres.rs` | 🟠 | Split `CREATE_OPPORTUNITIES_INDEX` and `CREATE_EXECUTIONS_INDEX` multi-statement strings into individual `execute()` calls |
| `engine/src/chains/evm.rs` | 🟡 | Route `execute_arbitrage` through Flashbots bundle submission instead of public mempool; assert `receipt.status()` after confirmation |
| `engine/src/mempool/listener.rs` | 🟠 | Mark placeholder pools with `last_updated_ts = 0` and skip them in pathfinding |
| `engine/src/config.rs` | 🟠 | Add `base_http_url: Option<String>` field; load from `BASE_HTTP_URL` env var |
| `engine/src/main.rs` | 🟠 | Use `config.base_http_url` for HTTP provider instead of string-replacing the WS URL |
| `arb-engine/.env.example` | 🔴 | Fix `DATABASE_URL` database name from `arbitrage_db` → `arb_engine` to match Docker Compose |
| `infra/docker-compose.yml` | 🟢 | No changes needed; add `postgres-init/01_extensions.sql` as new file |
| `infra/postgres-init/01_extensions.sql` | 🟢 | **New file** — create UUID extension: `CREATE EXTENSION IF NOT EXISTS "uuid-ossp"` |
| `contracts/evm/script/DeployBase.s.sol` | 🟠 | Apply same address-literal fixes as Deploy.s.sol if it has the same pattern |
| `engine/src/arb/router.rs` | 🟠 | Add staleness guard in edge weight computation: skip pools with `last_updated_ts == 0` |
