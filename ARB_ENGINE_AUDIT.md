# Cross-Chain Arbitrage Engine — End-to-End Audit Report

---

## Executive Summary

The engine compiles and the mathematical models are sound. However, **seven distinct bugs across four files** create an interlocking chain of failures that guarantee zero successful executions. Three of them are independently sufficient to prevent all on-chain trades. The remaining ones compound the damage with phantom opportunities, resource exhaustion, and silently wrong data.

---

## Part 1 — Root Cause Analysis

### BUG-1 🔴 CRITICAL — Fee units mismatch: every swap reverts immediately
**File:** `chains/evm.rs` — `execute_arbitrage()` and `simulate_arbitrage()`

```rust
// WRONG — passes fee_bps (e.g. 30) as Uniswap V3 uint24 fee (needs 3000)
let buy_fee  = alloy::primitives::Uint::<24, 1>::from(arb.route[0].fee_bps);
let sell_fee = alloy::primitives::Uint::<24, 1>::from(arb.route[1].fee_bps);
```

The internal representation uses **basis points** (30 = 0.30%).  
Uniswap V3's `exactInputSingle` takes **per-million fee units** (3000 = 0.30%).

Passing `fee=30` to the V3 router causes it to look up a pool with fee tier 30 — which does not exist on any chain. Both `simulate_arbitrage` (the safety gate) **and** `execute_arbitrage` carry this bug, so the simulation always reverts, and execution is gated behind a passing simulation. Net result: every arbitrage opportunity is discarded at the simulation step with "Simulation reverted — aborting execution."

**Fix:** multiply by 100 when building `ArbParams`.

---

### BUG-2 🔴 CRITICAL — Hardcoded Ethereum V1 router used on all chains
**File:** `chains/evm.rs`

```rust
// Constant at top of file
const UNISWAP_V3_ROUTER: &str = "0xE592427A0AEce92De3Edee1F18E0157C05861564";
// Used unconditionally for BOTH legs on EVERY chain
let router_addr = Address::from_str(UNISWAP_V3_ROUTER)...;
let params = ArbParams {
    buyRouter:  router_addr,
    sellRouter: router_addr, // same address
    ...
};
```

Three compounding problems here:
1. The address `0xE592427…` is Uniswap SwapRouter **V1** on Ethereum mainnet. The correct multi-chain router is SwapRouter02 (`0x68b3465…` on Ethereum/Arbitrum; `0x2626664…` on Base).
2. Both buy and sell legs always use the same router. A cross-DEX arb (e.g., Aerodrome → Uniswap) requires different routers per leg.
3. The router is resolved from a compile-time constant, completely ignoring the `pool_id` and `dex` fields on the `ArbitrageOpportunity` that correctly record which DEX each hop uses.

Any transaction constructed by the current code will be calling the wrong contract address on Base/Arbitrum and will revert with "call to non-existent contract" or the swap reverts inside the flash loan callback.

**Fix:** Look up the correct SwapRouter02 address by `ChainId`, and extract per-leg router addresses from `arb.route[n].dex`.

---

### BUG-3 🔴 CRITICAL — All Base chain DEX swaps silently ignored
**File:** `mempool/calldata_decoder.rs` — `RouterRegistry`

The engine is configured to monitor Base chain (confirmed by `BASE_WS_URL` in `.env` and the `START_TOKENS` constants that use Base-native token addresses such as `0x4200…0006` for WETH). However, the `RouterRegistry` contains only Ethereum mainnet, BSC, and one universal router address. **Zero Base chain DEX router addresses are present.**

Since `is_known_dex_router()` returns `false` for all Base routers, `WorkerCtx::process_payload()` exits immediately at the first guard:
```rust
if !is_known_dex_router(&payload.to_addr) { return; }
```

No decoded swaps → no graph updates → no path-finding → no opportunities found.

Missing Base chain routers (non-exhaustive):
| DEX | Router Address |
|-----|---------------|
| Uniswap V3 SwapRouter02 | `0x2626664c2603336E57B271c5C0b26F421741e481` |
| Uniswap Universal Router | `0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD` |
| Aerodrome V2 Router | `0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43` |
| Aerodrome Universal Router | `0x6Cb442acF35158D5eDa88fe602221b67B400Be3E` |
| BaseSwap Router | `0x327Df1E6de05895d2ab08513aaDD9313Fe505d86` |
| PancakeSwap V3 (Base) | `0x678Aa4bF4E210cf2166753e054d5b7c31cc7fa86` |
| SushiSwap V3 (Base) | `0xFB7eF66a7e61224DD6FcD0D7d9C3be5C8B049b9` |

**Fix:** Add all Base chain routers to the registry. Associate each address with the correct `DexVersion` so fee defaults and calldata dispatch work correctly.

---

### BUG-4 🔴 CRITICAL — Telemetry simulation loop contaminates production graph state
**File:** `mempool/listener.rs` — `connect_and_stream()`

Inside the production WebSocket stream handler, a background task is spawned that deliberately injects **fake pool state** into the live `LiquidityGraph`:

```rust
tokio::spawn(async move {
    loop {
        // Every 5 ticks: artificially skew Aerodrome WETH/USDC reserve by 8.3%
        let mut graph = this_sim.graph.write().await;
        if let Some(pool) = graph.get_pool("0x6cdcb1...") {
            let mut p = (**pool).clone();
            p.state.reserve_b = U256::from(if sim_tx_count % 5 == 0 {
                3_250_000_000_000u128  // "huge 8.3% arb!"
            } else {
                3_000_000_000_000u128
            });
            graph.upsert_pool(p);
        }
        // Also calls evaluate_arb_opportunity() with fake token pairs
    }
});
```

Consequences:
- Every 4 seconds, the Aerodrome pool's USDC reserve is set to a value that creates a ~8.3% price gap against all V3 pools. The router correctly identifies this as a large arbitrage opportunity.
- `simulate_arbitrage` is called. It fails (BUG-1 above). The failure metric increments.
- Even if BUG-1 were fixed, the simulation would fail because the fake 8.3% gap doesn't exist on-chain. This is a phantom opportunity.
- While the simulation is running, 8 workers contend for the graph write lock alongside this background task. Throughput collapses.
- When sim_tx_count % 5 ≠ 0, the pool returns to balanced state, masking any real opportunities that were found.

The simulation/dashboard live-feed code must be completely decoupled from the `LiquidityGraph`. Dashboard data should be generated from a **separate, in-memory-only** data structure that is never read by the router.

---

### BUG-5 🟠 HIGH — V2 selector collision causes wrong ABI decode
**File:** `mempool/calldata_decoder.rs`

```rust
SEL_V2_TOKENS_FOR_ETH | SEL_V2_TOKENS_FOR_ETH_EX => {
    // WRONG: SEL_V2_TOKENS_FOR_ETH is swapTokensForExactETH, not swapExactTokensForETH
    let call = IUniswapV2Router::swapExactTokensForETHCall::abi_decode(input_data, true).ok()?;
    v2_path_to_swap(call.path, call.amountIn, fee_bps, dex)
}
```

`SEL_V2_TOKENS_FOR_ETH` (`0x4a25d94a`) is `swapTokensForExactETH(uint256 amountOut, uint256 amountInMax, address[] path, address to, uint256 deadline)`.

`SEL_V2_TOKENS_FOR_ETH_EX` (`0x18cbafe5`) is `swapExactTokensForETH(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)`.

Both are decoded using the `swapExactTokensForETH` ABI, so for `swapTokensForExactETH` calls, `amountOut` is misread as `amountIn`. This means the decoded `amount_in` is actually the **minimum output** — often a much smaller number — which produces incorrect NEV calculations. These calls also frequently fail ABI decode entirely and are silently dropped.

---

### BUG-6 🟠 HIGH — New HTTP provider instantiated on every pool state fetch
**File:** `chains/evm.rs` — `get_v3_pool_state()` and `get_v2_pool_state()`

```rust
pub async fn get_v3_pool_state(&self, pool_address: &str) -> Result<(U256, i32, u128)> {
    let provider = ProviderBuilder::new()
        .on_builtin(&self.config.http_url)
        .await  // ← creates a new TCP connection every call
        ...
}
```

With 8 concurrent workers each calling `fetch_pool_state` for every decoded swap, this spawns a new HTTP connection for every transaction. On Alchemy's free tier (300 CU/s), this saturates the connection limit in seconds, causing HTTP 429 responses. The error path returns `simulated_v3_state()`, which uses hardcoded synthetic reserves — so the engine continues operating on stale simulated data rather than live chain state.

The adapter already has a cached `ws_provider` — but the two public `get_v3_pool_state`/`get_v2_pool_state` methods bypass it entirely.

---

### BUG-7 🟡 MEDIUM — Write lock held during entire Bellman-Ford scan
**File:** `mempool/listener.rs` — `evaluate_arb_opportunity()`

```rust
// Takes WRITE lock for the entire BF computation
let opportunities: Vec<ArbitrageOpportunity> = {
    let mut graph = self.graph.write().await;            // ← exclusive lock
    for start_token in START_TOKENS {
        opps.extend(graph.find_opportunities(...));      // read-only
    }
    opps.extend(find_arbitrage_cycles(&graph, &config)); // read-only
    reset_changed_tokens(&mut graph);                    // only this needs write
    opps
};
```

`find_opportunities` and `find_arbitrage_cycles` take `&self` / `&LiquidityGraph` — they are pure reads. Only `reset_changed_tokens` requires `&mut`. On a busy mempool with 8 workers, each BF run (potentially 10–50ms) holds a write lock, starving all other workers of both read and write access to the graph. This serializes what should be a highly concurrent system.

---

### BUG-8 🟡 MEDIUM — Redis TTL off by 12×
**File:** `mempool/listener.rs`

```rust
// Comment says "24-block TTL (≈5 min on mainnet)"
// But set_raw() takes seconds, not blocks
if let Err(e) = self.redis_cache.set_raw(&pool_cache_key, &json, 24).await {
```

`set_raw` calls Redis `SET EX ttl_secs`. Passing `24` sets a TTL of **24 seconds** (2 mainnet blocks), not the intended 24 blocks (288 seconds / ~5 minutes). Pool state is evicted from cache every 24 seconds, forcing a live on-chain fetch on almost every transaction — massively increasing RPC load and latency.

---

### BUG-9 🟡 MEDIUM — `clear_edges()` on every WS reconnect requires full cold-start
**File:** `mempool/listener.rs` — `run_evm_stream()`

```rust
// Called on every reconnect (including frequent Alchemy rate-limit disconnects)
let mut graph = self.graph.write().await;
graph.clear_edges();  // Removes ALL pools and edges from graph
```

`clear_edges()` in `router.rs` removes every pool, every edge, and the decimal cache. After a reconnect, the graph is empty. The engine cannot find any opportunities until the graph is re-populated by processing new mempool transactions — which, for illiquid pools, may take many seconds or minutes.

In production, WebSocket connections disconnect several times per hour. Each disconnect triggers a full graph cold-start.

**Fix:** Instead of clearing all edges, only mark all pools as "stale" and refresh them on-demand when they're next referenced, or keep the graph intact and rely on the stream of new events to update prices.

---

### BUG-10 🟢 LOW — AtomicArb.sol net profit accounting
**File:** `contracts/evm/src/AtomicArb.sol` — `executeOperation()`

```solidity
require(finalAmount >= repayAmount + arb.minProfitWei, "insufficient profit");
uint256 netProfit = finalAmount - repayAmount;  // ← minProfitWei not subtracted
emit ArbExecuted(..., netProfit, ...);
totalProfitAccumulated += netProfit;
```

`netProfit` includes `minProfitWei` — so the emitted event and accumulated total are inflated by the minimum profit threshold. This is an accounting/reporting bug, not a safety bug (the slippage guarantee still holds).

---

## Part 2 — Step-by-Step Action Plan

Prioritized in order: fix the bugs that gate execution first, then optimize.

| Priority | Bug | ETA | Impact |
|----------|-----|-----|--------|
| P0 | BUG-1: Fee units (× 100) in evm.rs | 30 min | Unblocks all execution |
| P0 | BUG-2: Chain-aware router in evm.rs | 1 hr | Unblocks Base/Arbitrum |
| P0 | BUG-3: Base router addresses in decoder | 1 hr | Enables swap detection |
| P0 | BUG-4: Decouple sim from graph state | 2 hr | Eliminates phantom arb |
| P1 | BUG-5: Fix V2 selector collision | 30 min | Correct amount decoding |
| P1 | BUG-6: Cache HTTP provider | 1 hr | Eliminates RPC exhaustion |
| P2 | BUG-7: Read lock for BF scan | 30 min | 8× throughput improvement |
| P2 | BUG-8: Redis TTL 24 → 288 seconds | 5 min | Correct pool caching |
| P2 | BUG-9: Soft edge invalidation on reconnect | 1 hr | Eliminates cold-start penalty |
| P3 | BUG-10: AtomicArb net profit accounting | 15 min | Correct event data |

---

## Part 3 — Exact Code Fixes

See attached fix files:
- `listener_fixed.rs` — BUGs 4, 7, 8, 9
- `calldata_decoder_fixed.rs` — BUGs 3, 5
- `evm_fixed.rs` — BUGs 1, 2, 6
- `AtomicArb_fixed.sol` — BUG 10
