# 🚀 Cross-Chain Arbitrage Engine — Production Deployment Guide

This guide details the steps required to deploy the **EVM Executor**, **SVM Anchor Contract**, configure the backend state pipeline, and execute the arbitrage engine in **Dry-Run (zero-capital risk simulation)** or **Active Execution** modes.

---

## 🗺️ System Overview & Architecture

```mermaid
graph TD
    subgraph EVM (Arbitrum Sepolia / Mainnet)
        EVM_WS[Alchemy/Infura Websocket] -->|Mempool Tx Stream| Listener[Mempool Listener]
        Executor_EVM[EVM Executor Contract]
    end

    subgraph SVM (Solana Devnet / Mainnet)
        SVM_WS[Helius WebSocket AccountSubscribe] -->|Raydium/Orca Reserve Updates| Listener
        Executor_SVM[Anchor Arbitrage Contract]
    end

    subgraph Backend Engine
        Listener -->|Dynamic State Changes| Graph[Liquidity Graph]
        Graph -->|Bellman-Ford Pathfinder| ArbEngine[Arbitrage Engine Core]
        ArbEngine -->|Profitable Routes| Executor[Execution Manager]
    end

    Executor -->|EVM Leg| Executor_EVM
    Executor -->|SVM Leg (Jito Bundle)| Executor_SVM
```

---

## 🛠️ Step 1: Deploy EVM Executor Contract (Arbitrum)

The companion EVM executor contract utilizes atomic multicall routing to capture the EVM leg of the arbitrage.

### 1. Compile & Deploy
Navigate to the contracts directory (using Hardhat or Foundry):
```bash
cd contracts/evm
forge build
```

Deploy the contract to **Arbitrum Sepolia** (or Arbitrum Mainnet):
```bash
forge create --rpc-url <YOUR_RPC_URL> \
             --private-key <YOUR_PRIVATE_KEY> \
             src/ArbitrageExecutor.sol:ArbitrageExecutor
```

### 2. Configure Address
Copy the deployed contract address and set it in your `.env` file:
```env
CONTRACT_ADDRESS=0xYourDeployedEvmExecutorAddress
```

---

## ⚓ Step 2: Deploy Anchor Contract (Solana)

The Anchor contract executes the SVM leg by dynamically routing swaps via CPI through Raydium and Orca.

### 1. Configure Program ID
Generate a new keypair for the program:
```bash
cd contracts/solana
anchor keys list
```
Replace the program ID declared in `contracts/solana/src/lib.rs` and `Anchor.toml` with the public key of the generated program keypair.

### 2. Build & Deploy
Compile the program:
```bash
anchor build
```

Deploy to **Solana Devnet** (or Mainnet):
```bash
anchor deploy --provider.cluster devnet
```

---

## ⚙️ Step 3: Configure Environment Variables

Create/update your `.env` file in the root `arb-engine` directory:

```env
# ── RPC Endpoints (EVM & SVM)
ETH_WS_URL=wss://eth-mainnet.g.alchemy.com/v2/YOUR-KEY
ETH_HTTP_URL=https://eth-mainnet.g.alchemy.com/v2/YOUR-KEY
BASE_WS_URL=wss://base-mainnet.g.alchemy.com/v2/YOUR-KEY
ARB_WS_URL=wss://arb-mainnet.g.alchemy.com/v2/YOUR-KEY

SOLANA_RPC_URL=https://mainnet.helius-rpc.com/?api-key=YOUR-KEY
SOLANA_WS_URL=wss://mainnet.helius-rpc.com/?api-key=YOUR-KEY

# ── Database & Cache
DATABASE_URL=postgresql://arb_user:arb_password@localhost:5432/arb_engine
REDIS_URL=redis://localhost:6379

# ── Execution Wallets
PRIVATE_KEY=0xYourEvmPrivateKey
SOLANA_PRIVATE_KEY=[Your,Solana,PrivateKey,Byte,Array]

# ── MEV Protection & Block Engine
FLASHBOTS_RPC_URL=https://rpc.flashbots.net
FLASHBOTS_SIGNING_KEY=0xYourSigningKey
JITO_BLOCK_ENGINE_URL=https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles

# ── Safety Margins & Limits
MIN_PROFIT_USD=1.50
MAX_TRADE_SIZE_PCT=0.02
MAX_PRICE_IMPACT_BPS=30
MAX_BLOCK_STALENESS=2
```

---

## 🗄️ Step 4: Seed Pools Database

Before launching the engine, seed the base pools registry into your PostgreSQL database:

```bash
cargo run --bin seed-base-pools
```
*This command populates the postgres registry with default high-liquidity EVM and SVM pool profiles (tokens, AMM type, addresses).*

---

## 🏃 Step 5: Execute Integration Tests

Verify that your network, signers, and Jito Bundle interfaces are operational:

```bash
cargo run --bin cross-chain-testnet
```

---

## ⚡ Step 6: Launch Backend Engine

To run the main engine in **Dry-Run Telemetry Mode** (observe opportunities without risking funds) or **Live Mode**:

### Dry-Run Mode (Recommended First Day)
Edit your config or run the engine with dry-run telemetry mode enabled (configured in `main.rs`):
```bash
cargo run --bin arb-engine -- --dry-run
```

### Live Production Execution
```bash
cargo run --bin arb-engine --release
```

---

## 📈 Monitoring & Telemetry
- The dashboard is accessible via your web interface at `http://localhost:3000`.
- Monitor real-time logs for profit evaluation:
  `[INFO] Detected cross-chain arbitrage opportunity! Net Profit: $3.42`
- Verify Jito bundle receipts via [Jito Explorer](https://explorer.jito.wtf/).
