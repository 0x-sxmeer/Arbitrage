import { useState, useEffect, useRef, useCallback } from "react";
import ExecutionDashboard from "./ExecutionDashboard";

// ─── DATA ────────────────────────────────────────────────────────────────────

const phases = [
  {
    month: "M1",
    title: "Data Infrastructure",
    subtitle: "Low-Latency Ingestion Engine",
    color: "#00FFD1",
    status: "ACTIVE",
    milestones: [
      {
        id: "m1-1",
        title: "Private Node Setup",
        detail:
          "Bare-metal Ethereum + Solana RPC nodes. Helius & Alchemy as fallback. Target <5ms block propagation.",
        stack: ["Erigon", "Geth", "Helius", "Alchemy"],
        status: "done",
        completion: 100,
      },
      {
        id: "m1-2",
        title: "WebSocket Mempool Monitor",
        detail:
          "Stream pending transactions via ws://. Detect large swaps before finalization to anticipate AMM price shifts.",
        stack: ["Rust", "alloy-rs", "tokio"],
        status: "done",
        completion: 100,
      },
      {
        id: "m1-3",
        title: "Custom Rust Indexer",
        detail:
          "Index pool depth, reserves, and tick data for V2/V3 AMMs. Avoid The Graph latency for live execution.",
        stack: ["Rust", "PostgreSQL", "Redis"],
        status: "done",
        completion: 100,
      },
      {
        id: "m1-4",
        title: "Multi-Chain Adapters",
        detail:
          "Unified abstraction layer for EVM (Ethereum/Base/Arbitrum), SVM (Solana), and IBC (Cosmos/Osmosis).",
        stack: ["ethers-rs", "solana-client", "CosmRS"],
        status: "done",
        completion: 100,
      },
    ],
    risk: "RPC node downtime. Mitigate: geo-redundant failover with health checks every 500ms.",
  },
  {
    month: "M2",
    title: "Quant Engine",
    subtitle: "Net Expected Value Calculator",
    color: "#FFD700",
    status: "ACTIVE",
    milestones: [
      {
        id: "m2-1",
        title: "AMM Math Library",
        detail:
          "Implement exact TickMath and SwapMath in Rust (v3_math) matching Uniswap concentrated liquidity rules.",
        stack: ["Rust", "v3_math", "uint256"],
        status: "done",
        completion: 100,
      },
      {
        id: "m2-2",
        title: "Net Profit Algorithm",
        detail:
          "Gross spread − EIP-1559 base fee − priority tip − multi-hop swap fees (0.01%–1%) − price impact. Only execute if NEV > threshold.",
        stack: ["Rust", "router.rs", "tokio"],
        status: "done",
        completion: 100,
      },
      {
        id: "m2-3",
        title: "Pathfinding Router",
        detail:
          "Bellman-Ford on a directed liquidity graph. Find optimal multi-hop route: Token A → B → C → A with max NEV.",
        stack: ["Graph algorithms", "Rust", "petgraph"],
        status: "done",
        completion: 100,
      },
      {
        id: "m2-4",
        title: "Slippage Estimator & GSS",
        detail:
          "Golden Section Search optimizer over profit curve f(borrow) to find optimal trade size.",
        stack: ["Golden Section Search", "optimizer.rs"],
        status: "done",
        completion: 100,
      },
    ],
    risk: "Stale price data causing false positives. Mitigate: timestamp validation with max 2-block staleness.",
  },
  {
    month: "M3",
    title: "Execution Layer",
    subtitle: "Atomic Contracts & MEV Shield",
    color: "#FF6B6B",
    status: "ACTIVE",
    milestones: [
      {
        id: "m3-1",
        title: "Atomic Arbitrage Contracts",
        detail:
          "Solidity (EVM) AtomicArb deployed at 0xeeD13772f4eCb6c74F1E585d2c2e472CB04994b8. Zero loss guarantee.",
        stack: ["Solidity", "Foundry", "AtomicArb.sol"],
        status: "done",
        completion: 100,
      },
      {
        id: "m3-2",
        title: "Flashloan Integration",
        detail:
          "Integrate Aave V3 flashloans. Execute $100k+ arb with zero upfront capital. Repay in same block.",
        stack: ["Aave V3", "Solidity"],
        status: "done",
        completion: 100,
      },
      {
        id: "m3-3",
        title: "MEV Protection",
        detail:
          "Submit bundles via Flashbots RPC to avoid sandwich attacks. Stable FLASHBOTS_SIGNING_KEY identity.",
        stack: ["Flashbots", "submit_flashbots_bundle"],
        status: "done",
        completion: 100,
      },
      {
        id: "m3-4",
        title: "Simulation Harness",
        detail:
          "Full dry-run simulation pipeline using eth_call against live state before executing flash loan.",
        stack: ["Anvil", "eth_call", "simulate_arbitrage"],
        status: "done",
        completion: 100,
      },
    ],
    risk: "Failed flashloan = gas loss only (tx reverts). Mitigate: pre-simulate every bundle in local fork before submission.",
  },
  {
    month: "M4",
    title: "Cross-Chain",
    subtitle: "Bridge Arbitrage & Inventory",
    color: "#A78BFA",
    status: "PLANNED",
    milestones: [
      {
        id: "m4-1",
        title: "Bridge Aggregator",
        detail:
          "Integrate Li.Fi + Stargate for EVM↔EVM. IBC relayers for Cosmos. Never build custom bridge validators.",
        stack: ["Li.Fi SDK", "Stargate", "IBC relayer"],
        status: "planned",
        completion: 0,
      },
      {
        id: "m4-2",
        title: "Inventory Management",
        detail:
          "Delta-neutral balances on each chain. Pre-fund wallets so both legs of cross-chain arb execute simultaneously without bridge wait.",
        stack: ["Redis", "Rust", "multi-sig"],
        status: "planned",
        completion: 0,
      },
      {
        id: "m4-3",
        title: "Latency Hedger",
        detail:
          "Model bridge finality time (Ethereum: ~15 blocks, Solana: ~400ms). Only enter cross-chain arb if spread > latency risk.",
        stack: ["Statistical models", "Rust"],
        status: "planned",
        completion: 0,
      },
      {
        id: "m4-4",
        title: "CEX-DEX Bridge",
        detail:
          "Optional: Monitor Binance/OKX via WebSocket. Exploit price gaps between CEX spot and DEX AMM price.",
        stack: ["CCXT", "Binance WS", "Rust"],
        status: "planned",
        completion: 0,
      },
    ],
    risk: "Bridge latency causes spread to close mid-flight. Mitigate: minimum profitability threshold × bridge time factor.",
  },
  {
    month: "M5",
    title: "Frontend & UI",
    subtitle: "Crafted Trading Interface",
    color: "#34D399",
    status: "PLANNED",
    milestones: [
      {
        id: "m5-1",
        title: "Core App Architecture",
        detail:
          "Next.js 14 (App Router) + TypeScript. tRPC for type-safe API. Zustand for real-time state. Turbopack for speed.",
        stack: ["Next.js 14", "TypeScript", "tRPC", "Zustand"],
        status: "planned",
        completion: 0,
      },
      {
        id: "m5-2",
        title: "Real-Time Data Feeds",
        detail:
          "WebSocket hooks pushing live pool reserves, opportunity alerts, and execution logs directly to the UI <100ms.",
        stack: ["React Query", "WebSocket", "SWR"],
        status: "planned",
        completion: 0,
      },
      {
        id: "m5-3",
        title: "3D Liquidity Visualizer",
        detail:
          "Three.js/WebGL render of liquidity depth as a 3D surface. Heartbeat animations on each new block. Tick heatmaps.",
        stack: ["Three.js", "WebGL", "D3.js", "Recharts"],
        status: "planned",
        completion: 0,
      },
      {
        id: "m5-4",
        title: "Execution Dashboard",
        detail:
          "Real-time P&L tracker, opportunity scanner table, gas estimator widget, and one-click manual execution interface.",
        stack: ["Tailwind CSS", "Radix UI", "Framer Motion"],
        status: "planned",
        completion: 0,
      },
    ],
    risk: "UI lag from high-frequency WebSocket events. Mitigate: throttle renders to 60fps with requestAnimationFrame batching.",
  },
  {
    month: "M6",
    title: "Security & Launch",
    subtitle: "Hardening, Audit & Mainnet",
    color: "#FB923C",
    status: "PLANNED",
    milestones: [
      {
        id: "m6-1",
        title: "Rug-Pull Detection",
        detail:
          "Before entering any new pool: verify contract for honeypot code, blacklist functions, malicious tax mechanisms, and minting rights.",
        stack: ["GoPlus API", "custom static analysis", "Rust"],
        status: "planned",
        completion: 0,
      },
      {
        id: "m6-2",
        title: "Toxic Flow Filter",
        detail:
          "Identify and blacklist wallets associated with MEV bots, wash trading, and protocol exploits using on-chain heuristics.",
        stack: ["Dune Analytics", "Nansen", "custom ML"],
        status: "planned",
        completion: 0,
      },
      {
        id: "m6-3",
        title: "Smart Contract Audit",
        detail:
          "External audit by Certik or Spearbit. Internal Slither + Echidna fuzzing. 48hr bug bounty before mainnet.",
        stack: ["Slither", "Echidna", "Certik"],
        status: "planned",
        completion: 0,
      },
      {
        id: "m6-4",
        title: "Production Launch",
        detail:
          "Deploy to mainnet. Start with $1k capital cap for first 2 weeks. Gradual scaling as performance is validated.",
        stack: ["AWS", "Cloudflare", "monitoring"],
        status: "planned",
        completion: 0,
      },
    ],
    risk: "Smart contract exploit. Mitigate: circuit breaker that pauses all execution if drawdown > 5% of portfolio in 1 hour.",
  },
];

const techStack = [
  { layer: "Backend Engine", tech: "Rust + Tokio", note: "Max speed, memory safe" },
  { layer: "EVM Contracts", tech: "Solidity + Foundry", note: "Atomic arb + flashloans" },
  { layer: "Solana Contracts", tech: "Rust + Anchor", note: "High-freq Solana DEXs" },
  { layer: "Cosmos Logic", tech: "CosmWasm + Go", note: "IBC arbitrage" },
  { layer: "Frontend", tech: "Next.js 14 + Tailwind", note: "Crafted trading UI" },
  { layer: "Database", tech: "PostgreSQL + Redis", note: "Pool state + hot cache" },
  { layer: "Infra", tech: "AWS + Cloudflare", note: "Low-latency global nodes" },
];

// ─── PHASE 1 NODE INFRASTRUCTURE DATA ────────────────────────────────────────

const rpcNodes = [
  { id: "eth-erigon", label: "ETH / Erigon", chain: "EVM", role: "Primary", latency: 2.1, status: "online" },
  { id: "eth-geth", label: "ETH / Geth", chain: "EVM", role: "Fallback", latency: 3.4, status: "online" },
  { id: "base-alchemy", label: "Base / Alchemy", chain: "EVM", role: "Primary", latency: 4.8, status: "online" },
  { id: "arb-alchemy", label: "Arb / Alchemy", chain: "EVM", role: "Primary", latency: 3.9, status: "online" },
  { id: "sol-helius", label: "SOL / Helius", chain: "SVM", role: "Primary", latency: 1.3, status: "online" },
  { id: "osmo-ibc", label: "Osmosis / IBC", chain: "IBC", role: "Primary", latency: 8.2, status: "degraded" },
];

const chainAdapters = [
  { name: "EVM Adapter", chains: ["Ethereum", "Base", "Arbitrum"], lib: "ethers-rs", status: "operational" },
  { name: "SVM Adapter", chains: ["Solana"], lib: "solana-client", status: "operational" },
  { name: "IBC Adapter", chains: ["Osmosis", "Cosmos Hub"], lib: "CosmRS", status: "partial" },
];

// ─── STATUS BADGE ─────────────────────────────────────────────────────────────

function StatusBadge({ status }) {
  const map = {
    done: { label: "DONE", bg: "rgba(0,255,209,0.12)", color: "#00FFD1", border: "rgba(0,255,209,0.3)" },
    "in-progress": { label: "IN PROGRESS", bg: "rgba(255,215,0,0.10)", color: "#FFD700", border: "rgba(255,215,0,0.3)" },
    planned: { label: "PLANNED", bg: "rgba(100,116,139,0.10)", color: "#64748B", border: "rgba(100,116,139,0.2)" },
    operational: { label: "OPERATIONAL", bg: "rgba(0,255,209,0.10)", color: "#00FFD1", border: "rgba(0,255,209,0.25)" },
    partial: { label: "PARTIAL", bg: "rgba(255,215,0,0.10)", color: "#FFD700", border: "rgba(255,215,0,0.25)" },
    online: { label: "ONLINE", bg: "rgba(0,255,209,0.10)", color: "#00FFD1", border: "rgba(0,255,209,0.2)" },
    degraded: { label: "DEGRADED", bg: "rgba(251,146,60,0.10)", color: "#FB923C", border: "rgba(251,146,60,0.25)" },
  };
  const s = map[status] || map.planned;
  return (
    <span style={{
      fontSize: 8,
      padding: "2px 7px",
      background: s.bg,
      border: `1px solid ${s.border}`,
      borderRadius: 2,
      color: s.color,
      letterSpacing: "0.12em",
      fontWeight: 600,
      flexShrink: 0,
    }}>{s.label}</span>
  );
}

// ─── PROGRESS BAR ────────────────────────────────────────────────────────────

function ProgressBar({ value, color, height = 3 }) {
  return (
    <div style={{ width: "100%", height, background: "#1E293B", borderRadius: height }}>
      <div style={{
        height: "100%",
        width: `${value}%`,
        background: `linear-gradient(90deg, ${color}99, ${color})`,
        borderRadius: height,
        transition: "width 0.6s ease",
        boxShadow: value > 0 ? `0 0 6px ${color}60` : "none",
      }} />
    </div>
  );
}

// ─── MEMPOOL STREAM (Phase 1 live feed simulation) ────────────────────────────

const TX_TEMPLATES = [
  { type: "SWAP", dex: "Uniswap V3", token: "USDC→ETH", size: () => `$${(Math.random() * 200 + 10).toFixed(0)}k`, color: "#00FFD1" },
  { type: "SWAP", dex: "Curve", token: "USDT→USDC", size: () => `$${(Math.random() * 500 + 50).toFixed(0)}k`, color: "#00FFD1" },
  { type: "SWAP", dex: "Raydium", token: "SOL→USDC", size: () => `$${(Math.random() * 100 + 5).toFixed(0)}k`, color: "#9945FF" },
  { type: "ADD_LIQ", dex: "Uniswap V3", token: "ETH/USDC", size: () => `$${(Math.random() * 50 + 10).toFixed(0)}k`, color: "#FFD700" },
  { type: "REMOVE_LIQ", dex: "Balancer", token: "WBTC/ETH", size: () => `$${(Math.random() * 80 + 20).toFixed(0)}k`, color: "#FB923C" },
  { type: "ARB", dex: "Flashbots", token: "ETH→USDC→ETH", size: () => `$${(Math.random() * 300 + 100).toFixed(0)}k`, color: "#FF6B6B" },
];

function MempoolStream() {
  const [txs, setTxs] = useState([]);

  useEffect(() => {
    const fetchTxs = async () => {
      try {
        const res = await fetch("http://localhost:3000/api/mempool");
        if (!res.ok) return;
        const data = await res.json();
        if (Array.isArray(data)) {
          setTxs(data);
        }
      } catch (err) {
        // Fallback or silent fail if backend is down
      }
    };

    fetchTxs();
    // Poll every 500ms to match backend stream interval
    const interval = setInterval(fetchTxs, 500);
    return () => clearInterval(interval);
  }, []);

  return (
    <div style={{ border: "1px solid #1A2233", borderRadius: 8, overflow: "hidden" }}>
      <div style={{
        padding: "10px 16px",
        borderBottom: "1px solid #1A2233",
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        background: "#0A0E14",
      }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <div className="live-dot" style={{ width: 6, height: 6, borderRadius: "50%", background: "#00FFD1", flexShrink: 0 }} />
          <span style={{ fontSize: 10, color: "#64748B", letterSpacing: "0.15em" }}>MEMPOOL STREAM</span>
        </div>
        <span style={{ fontSize: 10, color: "#00FFD1", fontFamily: "monospace" }}>
          LIVE FEED
        </span>
      </div>
      <div style={{ maxHeight: 210, overflowY: "hidden", padding: "8px 0" }}>
        {txs.map((tx, i) => (
          <div
            key={tx.id || i}
            style={{
              display: "grid",
              gridTemplateColumns: "65px 90px 130px 1fr 55px",
              gap: 8,
              padding: "5px 16px",
              opacity: 1 - i * 0.065,
              fontSize: 10,
              borderBottom: i < txs.length - 1 ? "1px solid #0D1117" : "none",
              transition: "opacity 0.3s",
            }}
          >
            <span style={{ color: tx.color, fontWeight: 600, letterSpacing: "0.08em" }}>{tx.type}</span>
            <span style={{ color: "#94A3B8" }}>{tx.hash}…</span>
            <span style={{ color: "#475569" }}>{tx.dex}</span>
            <span style={{ color: "#64748B" }}>{tx.token}</span>
            <span style={{ color: "#94A3B8", textAlign: "right" }}>{tx.size}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ─── RPC NODE HEALTH PANEL (Phase 1) ─────────────────────────────────────────

function NodeHealthPanel() {
  const [nodes, setNodes] = useState(rpcNodes);
  const [blockNum, setBlockNum] = useState(20_493_812);

  useEffect(() => {
    const interval = setInterval(() => {
      setNodes(prev => prev.map(n => ({
        ...n,
        latency: n.status === "degraded"
          ? 8 + Math.random() * 6
          : Math.max(0.8, n.latency + (Math.random() - 0.5) * 0.8),
      })));
      setBlockNum(prev => prev + Math.floor(Math.random() * 2));
    }, 1200);
    return () => clearInterval(interval);
  }, []);

  const chainColor = { EVM: "#627EEA", SVM: "#9945FF", IBC: "#00B5AD" };

  return (
    <div style={{ border: "1px solid #1A2233", borderRadius: 8, overflow: "hidden" }}>
      <div style={{
        padding: "10px 16px",
        borderBottom: "1px solid #1A2233",
        background: "#0A0E14",
        display: "flex",
        justifyContent: "space-between",
        alignItems: "center",
      }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <div className="live-dot" style={{ width: 6, height: 6, borderRadius: "50%", background: "#00FFD1" }} />
          <span style={{ fontSize: 10, color: "#64748B", letterSpacing: "0.15em" }}>RPC NODE HEALTH</span>
        </div>
        <span style={{ fontSize: 10, color: "#475569", fontFamily: "monospace" }}>
          BLOCK #{blockNum.toLocaleString()}
        </span>
      </div>
      <div>
        {nodes.map((node, i) => (
          <div key={node.id} style={{
            display: "grid",
            gridTemplateColumns: "1fr 55px 70px 90px",
            gap: 12,
            padding: "9px 16px",
            borderBottom: i < nodes.length - 1 ? "1px solid #0D1117" : "none",
            alignItems: "center",
          }}>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span style={{
                fontSize: 8,
                padding: "1px 5px",
                background: chainColor[node.chain] + "18",
                border: `1px solid ${chainColor[node.chain]}30`,
                color: chainColor[node.chain],
                borderRadius: 2,
                letterSpacing: "0.1em",
                flexShrink: 0,
              }}>{node.chain}</span>
              <span style={{ fontSize: 11, color: "#CBD5E1" }}>{node.label}</span>
              <span style={{ fontSize: 9, color: "#374151" }}>{node.role}</span>
            </div>
            <div style={{ textAlign: "right" }}>
              <span style={{
                fontSize: 11,
                fontFamily: "monospace",
                color: node.latency < 5 ? "#00FFD1" : node.latency < 8 ? "#FFD700" : "#FF6B6B",
              }}>
                {node.latency.toFixed(1)}ms
              </span>
            </div>
            <div>
              <ProgressBar
                value={Math.min(100, (10 - node.latency) / 10 * 100)}
                color={node.latency < 5 ? "#00FFD1" : node.latency < 8 ? "#FFD700" : "#FF6B6B"}
                height={3}
              />
            </div>
            <div style={{ display: "flex", justifyContent: "flex-end" }}>
              <StatusBadge status={node.status} />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

// ─── CHAIN ADAPTER STATUS (Phase 1) ──────────────────────────────────────────

function ChainAdapterPanel() {
  return (
    <div style={{ border: "1px solid #1A2233", borderRadius: 8, overflow: "hidden" }}>
      <div style={{
        padding: "10px 16px",
        borderBottom: "1px solid #1A2233",
        background: "#0A0E14",
      }}>
        <span style={{ fontSize: 10, color: "#64748B", letterSpacing: "0.15em" }}>CHAIN ADAPTER STATUS</span>
      </div>
      {chainAdapters.map((adapter, i) => (
        <div key={adapter.name} style={{
          padding: "12px 16px",
          borderBottom: i < chainAdapters.length - 1 ? "1px solid #0D1117" : "none",
        }}>
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 6 }}>
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <span style={{ fontSize: 11, color: "#CBD5E1", fontWeight: 600 }}>{adapter.name}</span>
              <span style={{ fontSize: 9, color: "#374151", fontFamily: "monospace" }}>{adapter.lib}</span>
            </div>
            <StatusBadge status={adapter.status} />
          </div>
          <div style={{ display: "flex", gap: 5, flexWrap: "wrap" }}>
            {adapter.chains.map(c => (
              <span key={c} style={{
                fontSize: 9,
                padding: "2px 7px",
                background: "#0D1117",
                border: "1px solid #1A2233",
                borderRadius: 3,
                color: "#475569",
              }}>{c}</span>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

// ─── RUST INDEXER STATS (Phase 1) ────────────────────────────────────────────

function IndexerStatsPanel() {
  const [stats, setStats] = useState({
    poolsIndexed: 0,
    cacheHitRate: 0,
    indexLag: 0, // txs_seen
    redisKeys: 0, // opps_found
    pgWrites: 0, // txs_decoded
  });

  useEffect(() => {
    const fetchMetrics = async () => {
      try {
        const res = await fetch("http://localhost:3000/api/metrics");
        if (!res.ok) return;
        const data = await res.json();
        const hitRate = (data.cache_hits + data.cache_misses) === 0 ? 0 : 
          (data.cache_hits / (data.cache_hits + data.cache_misses)) * 100;
          
        setStats({
          poolsIndexed: data.graph_pools,
          cacheHitRate: hitRate,
          indexLag: data.txs_seen,
          redisKeys: data.opportunities_found,
          pgWrites: data.txs_decoded,
        });
      } catch (err) {
        // Backend not running, default to 0
      }
    };

    fetchMetrics();
    const interval = setInterval(fetchMetrics, 1000);
    return () => clearInterval(interval);
  }, []);

  const metrics = [
    { label: "Pools Indexed", value: stats.poolsIndexed.toLocaleString(), unit: "", color: "#00FFD1" },
    { label: "Cache Hit Rate", value: stats.cacheHitRate.toFixed(1), unit: "%", color: stats.cacheHitRate > 90 ? "#00FFD1" : "#FFD700" },
    { label: "Txs Seen", value: stats.indexLag.toLocaleString(), unit: "", color: "#00FFD1" },
    { label: "Opps Found", value: stats.redisKeys.toLocaleString(), unit: "", color: "#A78BFA" },
    { label: "Txs Decoded", value: stats.pgWrites.toLocaleString(), unit: "", color: "#64748B" },
  ];

  return (
    <div style={{ border: "1px solid #1A2233", borderRadius: 8, overflow: "hidden" }}>
      <div style={{
        padding: "10px 16px",
        borderBottom: "1px solid #1A2233",
        background: "#0A0E14",
        display: "flex",
        alignItems: "center",
        gap: 8,
      }}>
        <div className="live-dot" style={{ width: 6, height: 6, borderRadius: "50%", background: "#00FFD1" }} />
        <span style={{ fontSize: 10, color: "#64748B", letterSpacing: "0.15em" }}>RUST INDEXER · LIVE</span>
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(5, 1fr)", gap: 0 }}>
        {metrics.map((m, i) => (
          <div key={m.label} style={{
            padding: "14px 12px",
            borderRight: i < metrics.length - 1 ? "1px solid #0D1117" : "none",
            textAlign: "center",
          }}>
            <div style={{ fontSize: 16, fontWeight: 600, color: m.color, fontFamily: "monospace", marginBottom: 4 }}>
              {m.value}{m.unit}
            </div>
            <div style={{ fontSize: 9, color: "#374151", letterSpacing: "0.08em" }}>{m.label}</div>
          </div>
        ))}
      </div>
    </div>
  );
}

// ─── NEV FORMULA (Phase 2) ────────────────────────────────────────────────────

function NevFormula({ color }) {
  return (
    <div style={{
      border: `1px solid ${color}25`,
      borderRadius: 8,
      padding: "20px 24px",
      background: `${color}05`,
    }}>
      <div style={{ fontSize: 10, color, letterSpacing: "0.2em", marginBottom: 16 }}>
        NET EXPECTED VALUE FORMULA
      </div>
      <div style={{ fontFamily: "monospace", fontSize: 13, color: "#94A3B8", lineHeight: 2.2 }}>
        <span style={{ color }}>NEV</span>
        <span style={{ color: "#64748B" }}> = </span>
        <span style={{ color: "#34D399" }}>gross_spread</span>
        <br />
        <span style={{ color: "#64748B" }}>{"      − "}</span>
        <span style={{ color: "#FF6B6B" }}>eip1559_base_fee</span>
        <span style={{ color: "#64748B" }}> × gas_units</span>
        <br />
        <span style={{ color: "#64748B" }}>{"      − "}</span>
        <span style={{ color: "#FF6B6B" }}>priority_tip</span>
        <br />
        <span style={{ color: "#64748B" }}>{"      − "}</span>
        <span style={{ color: "#FF6B6B" }}>Σ(swap_fee_i × amount_i)</span>
        <br />
        <span style={{ color: "#64748B" }}>{"      − "}</span>
        <span style={{ color: "#FF6B6B" }}>price_impact(pool_depth, trade_size)</span>
        <br /><br />
        <span style={{ color: "#64748B" }}>Execute only if </span>
        <span style={{ color }}>NEV</span>
        <span style={{ color: "#64748B" }}> {">"} </span>
        <span style={{ color: "#34D399" }}>min_profit_threshold</span>
        <span style={{ color: "#64748B" }}> ($0.50 suggested)</span>
      </div>
    </div>
  );
}

// ─── EXECUTION FLOW (Phase 3) ─────────────────────────────────────────────────

function ExecutionFlow({ color }) {
  const steps = [
    { label: "Detect Opportunity", sub: "Mempool + price feed" },
    { label: "Simulate Bundle", sub: "Anvil local fork" },
    { label: "NEV Check", sub: "Reject if NEV < $0.50" },
    { label: "Encode Flashloan TX", sub: "Aave V3 / Balancer" },
    { label: "Submit via Flashbots", sub: "Private mempool" },
    { label: "Confirm & Log", sub: "PG write + alert" },
  ];
  return (
    <div style={{ border: `1px solid ${color}25`, borderRadius: 8, padding: "20px 24px", background: `${color}04` }}>
      <div style={{ fontSize: 10, color, letterSpacing: "0.2em", marginBottom: 18 }}>EXECUTION FLOW</div>
      <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
        {steps.map((s, i) => (
          <div key={i} style={{ display: "flex", gap: 12, alignItems: "flex-start" }}>
            <div style={{ display: "flex", flexDirection: "column", alignItems: "center" }}>
              <div style={{
                width: 20, height: 20, borderRadius: "50%",
                background: `${color}15`,
                border: `1px solid ${color}40`,
                color,
                fontSize: 9,
                display: "flex", alignItems: "center", justifyContent: "center",
                flexShrink: 0, fontWeight: 700,
              }}>{i + 1}</div>
              {i < steps.length - 1 && <div style={{ width: 1, height: 24, background: `${color}20`, margin: "2px 0" }} />}
            </div>
            <div style={{ paddingTop: 2, paddingBottom: i < steps.length - 1 ? 0 : 0 }}>
              <div style={{ fontSize: 12, color: "#CBD5E1", fontWeight: 600 }}>{s.label}</div>
              <div style={{ fontSize: 10, color: "#475569", marginBottom: 4 }}>{s.sub}</div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

// ─── BRIDGE LATENCY TABLE (Phase 4) ──────────────────────────────────────────

function BridgeLatencyTable({ color }) {
  const rows = [
    { route: "ETH → Base", bridge: "Native Bridge", finality: "7 days", fast: "~3 min (Stargate)", threshold: "$450+" },
    { route: "ETH → Arbitrum", bridge: "Nitro", finality: "7 days", fast: "~10 min (Li.Fi)", threshold: "$200+" },
    { route: "ETH → Solana", bridge: "Wormhole", finality: "~13 min", fast: "~1 min (relay)", threshold: "$800+" },
    { route: "Cosmos → Osmosis", bridge: "IBC", finality: "~6s", fast: "~6s (native)", threshold: "$50+" },
  ];
  return (
    <div style={{ border: `1px solid ${color}20`, borderRadius: 8, overflow: "hidden" }}>
      <div style={{ padding: "10px 16px", background: `${color}06`, borderBottom: `1px solid ${color}15` }}>
        <span style={{ fontSize: 10, color, letterSpacing: "0.2em" }}>BRIDGE LATENCY MATRIX</span>
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "1fr 100px 90px 120px 80px", gap: 0 }}>
        {["ROUTE", "BRIDGE", "FINALITY", "FAST PATH", "MIN ARB"].map(h => (
          <div key={h} style={{ padding: "8px 12px", fontSize: 9, color: "#374151", letterSpacing: "0.1em", borderBottom: "1px solid #0D1117" }}>{h}</div>
        ))}
        {rows.map((r, i) => [
          <div key={`r${i}-0`} style={{ padding: "9px 12px", fontSize: 10, color: "#94A3B8", borderBottom: i < rows.length - 1 ? "1px solid #0D1117" : "none" }}>{r.route}</div>,
          <div key={`r${i}-1`} style={{ padding: "9px 12px", fontSize: 10, color: "#64748B", borderBottom: i < rows.length - 1 ? "1px solid #0D1117" : "none" }}>{r.bridge}</div>,
          <div key={`r${i}-2`} style={{ padding: "9px 12px", fontSize: 10, color: "#FF6B6B", fontFamily: "monospace", borderBottom: i < rows.length - 1 ? "1px solid #0D1117" : "none" }}>{r.finality}</div>,
          <div key={`r${i}-3`} style={{ padding: "9px 12px", fontSize: 10, color: "#00FFD1", fontFamily: "monospace", borderBottom: i < rows.length - 1 ? "1px solid #0D1117" : "none" }}>{r.fast}</div>,
          <div key={`r${i}-4`} style={{ padding: "9px 12px", fontSize: 10, color, fontFamily: "monospace", borderBottom: i < rows.length - 1 ? "1px solid #0D1117" : "none" }}>{r.threshold}</div>,
        ])}
      </div>
    </div>
  );
}

// ─── SECURITY CHECKLIST (Phase 6) ────────────────────────────────────────────

function SecurityChecklist({ color }) {
  const items = [
    { label: "Reentrancy guards on all external calls", done: false },
    { label: "Overflow protection (Solidity ^0.8 built-in)", done: false },
    { label: "Access control on admin functions (Ownable)", done: false },
    { label: "Circuit breaker: pause if drawdown > 5%/hr", done: false },
    { label: "Slither static analysis — zero high findings", done: false },
    { label: "Echidna fuzz testing (100k runs)", done: false },
    { label: "External audit (Certik / Spearbit)", done: false },
    { label: "48h bug bounty on testnet", done: false },
    { label: "Multi-sig for contract ownership", done: false },
    { label: "GoPlus API rug-pull screening on all pools", done: false },
  ];
  return (
    <div style={{ border: `1px solid ${color}20`, borderRadius: 8, overflow: "hidden" }}>
      <div style={{ padding: "10px 16px", background: `${color}06`, borderBottom: `1px solid ${color}15` }}>
        <span style={{ fontSize: 10, color, letterSpacing: "0.2em" }}>PRE-LAUNCH SECURITY CHECKLIST</span>
      </div>
      <div style={{ padding: "8px 4px" }}>
        {items.map((item, i) => (
          <div key={i} style={{ display: "flex", alignItems: "center", gap: 10, padding: "7px 14px" }}>
            <div style={{
              width: 12, height: 12, borderRadius: 2,
              border: `1px solid ${item.done ? color : "#2D3748"}`,
              background: item.done ? color : "transparent",
              flexShrink: 0,
            }} />
            <span style={{ fontSize: 11, color: item.done ? "#CBD5E1" : "#475569" }}>{item.label}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ─── PHASE-SPECIFIC PANEL ROUTER ─────────────────────────────────────────────

function PhasePanel({ phaseIndex, phase }) {
  if (phaseIndex === 0) {
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
        <IndexerStatsPanel />
        <NodeHealthPanel />
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 16 }}>
          <MempoolStream />
          <ChainAdapterPanel />
        </div>
        {/* Tech Stack Table */}
        <div>
          <div style={{ fontSize: 10, color: "#475569", letterSpacing: "0.2em", marginBottom: 12 }}>FULL TECH STACK</div>
          <div style={{ border: "1px solid #1A2233", borderRadius: 8, overflow: "hidden" }}>
            {techStack.map((t, i) => (
              <div key={i} style={{
                display: "grid",
                gridTemplateColumns: "160px 1fr 1fr",
                borderBottom: i < techStack.length - 1 ? "1px solid #1A2233" : "none",
                padding: "12px 20px",
                background: i % 2 === 0 ? "#0D1117" : "transparent",
              }}>
                <span style={{ fontSize: 11, color: "#475569" }}>{t.layer}</span>
                <span style={{ fontSize: 11, color: "#00FFD1", fontWeight: 600 }}>{t.tech}</span>
                <span style={{ fontSize: 11, color: "#64748B" }}>{t.note}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
    );
  }
  if (phaseIndex === 1) return <NevFormula color={phase.color} />;
  if (phaseIndex === 2) return <ExecutionFlow color={phase.color} />;
  if (phaseIndex === 3) return <BridgeLatencyTable color={phase.color} />;
  if (phaseIndex === 4) return <ExecutionDashboard color={phase.color} />;
  if (phaseIndex === 5) return <SecurityChecklist color={phase.color} />;
  return null;
}

// ─── OVERALL PROGRESS ────────────────────────────────────────────────────────

function computeOverallProgress(phases) {
  const totals = phases.flatMap(p => p.milestones.map(m => m.completion));
  return Math.round(totals.reduce((a, b) => a + b, 0) / totals.length);
}

// ─── MAIN COMPONENT ───────────────────────────────────────────────────────────

export default function ArbRoadmap() {
  const [active, setActive] = useState(4);
  const [hoveredMilestone, setHoveredMilestone] = useState(null);
  const [phaseData, setPhaseData] = useState(phases);

  const phase = phaseData[active];
  const overallProgress = computeOverallProgress(phaseData);

  const toggleMilestoneStatus = useCallback((phaseIdx, milestoneIdx) => {
    setPhaseData(prev => {
      const next = prev.map((p, pi) => ({
        ...p,
        milestones: p.milestones.map((m, mi) => {
          if (pi !== phaseIdx || mi !== milestoneIdx) return m;
          const cycle = { planned: "in-progress", "in-progress": "done", done: "planned" };
          const nextStatus = cycle[m.status];
          const nextCompletion = nextStatus === "done" ? 100 : nextStatus === "in-progress" ? 50 : 0;
          return { ...m, status: nextStatus, completion: nextCompletion };
        }),
      }));
      return next;
    });
  }, []);

  return (
    <div style={{
      fontFamily: "'Syne Mono', 'IBM Plex Mono', 'Courier New', monospace",
      background: "#080B0F",
      color: "#E2E8F0",
      minHeight: "100vh",
      overflow: "hidden",
    }}>
      <style>{`
        @import url('https://fonts.googleapis.com/css2?family=Syne+Mono&family=Syne:wght@400;500;600;700;800&display=swap');
        * { box-sizing: border-box; margin: 0; padding: 0; }
        ::-webkit-scrollbar { width: 4px; }
        ::-webkit-scrollbar-track { background: #0D1117; }
        ::-webkit-scrollbar-thumb { background: #2D3748; border-radius: 2px; }
        .phase-btn { transition: all 0.2s ease; cursor: pointer; border: none; background: none; }
        .phase-btn:hover { opacity: 1 !important; }
        .row-flash-purple {
          animation: flashPurple 1.5s cubic-bezier(0.25, 0.46, 0.45, 0.94) both;
        }
        @keyframes flashPurple {
          0% { background-color: rgba(167, 139, 250, 0.22); }
          100% { background-color: transparent; }
        }
        .milestone-card { transition: all 0.18s ease; cursor: pointer; }
        .milestone-card:hover { transform: translateY(-1px); }
        .stack-tag { display: inline-block; }
        @keyframes pulse { 0%,100%{opacity:1;box-shadow:0 0 4px currentColor} 50%{opacity:0.5;box-shadow:0 0 10px currentColor} }
        @keyframes fadeSlideIn { from{opacity:0;transform:translateY(6px)} to{opacity:1;transform:translateY(0)} }
        .live-dot { animation: pulse 1.6s ease-in-out infinite; }
        .phase-content { animation: fadeSlideIn 0.25s ease forwards; }
        .grid-bg {
          background-image: 
            linear-gradient(rgba(0,255,209,0.025) 1px, transparent 1px),
            linear-gradient(90deg, rgba(0,255,209,0.025) 1px, transparent 1px);
          background-size: 40px 40px;
        }
      `}</style>

      {/* ── Header ── */}
      <div style={{
        borderBottom: "1px solid #1A2233",
        padding: "14px 28px",
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        background: "rgba(8,11,15,0.97)",
        backdropFilter: "blur(20px)",
        position: "sticky",
        top: 0,
        zIndex: 100,
      }}>
        <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
          <div className="live-dot" style={{
            width: 8, height: 8, borderRadius: "50%",
            background: "#00FFD1", color: "#00FFD1",
          }} />
          <span style={{ fontSize: 10, color: "#64748B", letterSpacing: "0.25em" }}>ARB-DAPP</span>
          <span style={{ color: "#1A2233" }}>│</span>
          <span style={{ fontSize: 12, color: "#94A3B8", letterSpacing: "0.04em" }}>
            Cross-Chain Arbitrage Engine — 6-Month Roadmap
          </span>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 16 }}>
          {["EVM", "SVM", "IBC"].map(chain => (
            <span key={chain} style={{
              fontSize: 9,
              padding: "3px 9px",
              border: "1px solid #1E293B",
              borderRadius: 3,
              color: "#64748B",
              letterSpacing: "0.15em",
            }}>{chain}</span>
          ))}
          <div style={{
            display: "flex", alignItems: "center", gap: 8,
            padding: "4px 12px",
            border: "1px solid #00FFD130",
            borderRadius: 4,
            background: "rgba(0,255,209,0.04)",
          }}>
            <span style={{ fontSize: 9, color: "#64748B", letterSpacing: "0.1em" }}>OVERALL</span>
            <span style={{ fontSize: 12, color: "#00FFD1", fontWeight: 700, fontFamily: "monospace" }}>
              {overallProgress}%
            </span>
          </div>
        </div>
      </div>

      <div style={{ display: "flex", height: "calc(100vh - 53px)" }}>

        {/* ── Left Sidebar ── */}
        <div style={{
          width: 210,
          borderRight: "1px solid #1A2233",
          padding: "20px 0",
          flexShrink: 0,
          overflowY: "auto",
          display: "flex",
          flexDirection: "column",
        }} className="grid-bg">
          <div style={{ padding: "0 18px 14px", fontSize: 9, color: "#374151", letterSpacing: "0.2em" }}>
            PHASES
          </div>
          {phaseData.map((p, i) => {
            const avgCompletion = Math.round(p.milestones.reduce((a, m) => a + m.completion, 0) / p.milestones.length);
            return (
              <button
                key={i}
                className="phase-btn"
                onClick={() => setActive(i)}
                style={{
                  width: "100%",
                  padding: "13px 18px",
                  textAlign: "left",
                  opacity: active === i ? 1 : 0.45,
                  borderLeft: active === i ? `2px solid ${p.color}` : "2px solid transparent",
                  background: active === i ? "rgba(255,255,255,0.03)" : "transparent",
                }}
              >
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 3 }}>
                  <span style={{ fontSize: 9, color: p.color, letterSpacing: "0.2em" }}>{p.month}</span>
                  <span style={{ fontSize: 9, color: avgCompletion > 0 ? p.color : "#374151", fontFamily: "monospace" }}>
                    {avgCompletion}%
                  </span>
                </div>
                <div style={{ fontSize: 12, fontWeight: 600, color: active === i ? "#F1F5F9" : "#94A3B8", lineHeight: 1.3, marginBottom: 2, fontFamily: "'Syne', sans-serif" }}>
                  {p.title}
                </div>
                <div style={{ fontSize: 9, color: "#475569", lineHeight: 1.4, marginBottom: 6 }}>{p.subtitle}</div>
                <ProgressBar value={avgCompletion} color={p.color} height={2} />
              </button>
            );
          })}
        </div>

        {/* ── Main Content ── */}
        <div style={{ flex: 1, overflowY: "auto", padding: "28px 32px" }} key={active} className="phase-content">

          {/* Phase Header */}
          <div style={{ marginBottom: 28 }}>
            <div style={{ display: "flex", alignItems: "baseline", gap: 14, marginBottom: 10 }}>
              <span style={{ fontSize: 56, fontWeight: 800, color: phase.color, opacity: 0.12, lineHeight: 1, fontFamily: "'Syne', sans-serif" }}>
                {phase.month}
              </span>
              <div>
                <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 4 }}>
                  <h1 style={{ fontSize: 22, fontWeight: 700, color: "#F1F5F9", letterSpacing: "-0.02em", fontFamily: "'Syne', sans-serif" }}>
                    {phase.title}
                  </h1>
                  <StatusBadge status={
                    phase.milestones.every(m => m.status === "done") ? "done" :
                    phase.milestones.some(m => m.status === "done" || m.status === "in-progress") ? "in-progress" :
                    "planned"
                  } />
                </div>
                <p style={{ fontSize: 12, color: "#64748B" }}>{phase.subtitle}</p>
              </div>
            </div>
            <div style={{ height: 1, background: `linear-gradient(90deg, ${phase.color}50, transparent)` }} />
          </div>

          {/* Milestones Grid */}
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 14, marginBottom: 24 }}>
            {phase.milestones.map((m, i) => (
              <div
                key={m.id}
                className="milestone-card"
                onMouseEnter={() => setHoveredMilestone(i)}
                onMouseLeave={() => setHoveredMilestone(null)}
                onClick={() => toggleMilestoneStatus(active, i)}
                title="Click to cycle status"
                style={{
                  border: `1px solid ${hoveredMilestone === i ? phase.color + "40" : "#1A2233"}`,
                  borderRadius: 8,
                  padding: "18px 20px",
                  background: hoveredMilestone === i ? "rgba(255,255,255,0.02)" : "#0D1117",
                  boxShadow: hoveredMilestone === i ? `0 0 18px ${phase.color}08` : "none",
                  position: "relative",
                }}
              >
                {/* Status stripe */}
                <div style={{
                  position: "absolute",
                  top: 0, left: 0, right: 0,
                  height: 2,
                  background: m.status === "done" ? phase.color :
                    m.status === "in-progress" ? `linear-gradient(90deg, ${phase.color}, ${phase.color}00)` : "transparent",
                  borderRadius: "8px 8px 0 0",
                }} />

                <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 8 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                    <div style={{
                      width: 5, height: 5, borderRadius: "50%",
                      background: m.status === "done" ? phase.color :
                        m.status === "in-progress" ? "#FFD700" : "#374151",
                      flexShrink: 0,
                    }} />
                    <span style={{ fontSize: 12, fontWeight: 600, color: "#E2E8F0", letterSpacing: "-0.01em", fontFamily: "'Syne', sans-serif" }}>
                      {m.title}
                    </span>
                  </div>
                  <StatusBadge status={m.status} />
                </div>

                <p style={{ fontSize: 11, color: "#64748B", lineHeight: 1.7, marginBottom: 12 }}>
                  {m.detail}
                </p>

                {/* Completion bar */}
                <div style={{ marginBottom: 12 }}>
                  <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 5 }}>
                    <span style={{ fontSize: 8, color: "#374151", letterSpacing: "0.1em" }}>COMPLETION</span>
                    <span style={{ fontSize: 9, color: m.completion > 0 ? phase.color : "#374151", fontFamily: "monospace" }}>
                      {m.completion}%
                    </span>
                  </div>
                  <ProgressBar value={m.completion} color={phase.color} height={3} />
                </div>

                <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
                  {m.stack.map((s, j) => (
                    <span key={j} className="stack-tag" style={{
                      fontSize: 9,
                      padding: "2px 7px",
                      background: phase.color + "0E",
                      border: `1px solid ${phase.color}22`,
                      borderRadius: 3,
                      color: phase.color,
                      letterSpacing: "0.04em",
                    }}>{s}</span>
                  ))}
                </div>
              </div>
            ))}
          </div>

          {/* Risk Panel */}
          <div style={{
            border: "1px solid #FF6B6B25",
            borderLeft: "2px solid #FF6B6B",
            borderRadius: 6,
            padding: "12px 16px",
            background: "rgba(255,107,107,0.03)",
            marginBottom: 28,
            display: "flex",
            gap: 12,
            alignItems: "flex-start",
          }}>
            <span style={{ fontSize: 9, color: "#FF6B6B", letterSpacing: "0.15em", flexShrink: 0, marginTop: 1 }}>RISK</span>
            <p style={{ fontSize: 11, color: "#94A3B8", lineHeight: 1.6 }}>{phase.risk}</p>
          </div>

          {/* Phase-Specific Panel */}
          <PhasePanel phaseIndex={active} phase={phase} />

        </div>

        {/* ── Right Sidebar — Timeline ── */}
        <div style={{
          width: 170,
          borderLeft: "1px solid #1A2233",
          padding: "20px 14px",
          flexShrink: 0,
          overflowY: "auto",
        }}>
          <div style={{ fontSize: 9, color: "#374151", letterSpacing: "0.2em", marginBottom: 18 }}>TIMELINE</div>
          {phaseData.map((p, i) => (
            <div
              key={i}
              style={{ display: "flex", gap: 10, marginBottom: 18, opacity: i === active ? 1 : 0.4, cursor: "pointer" }}
              onClick={() => setActive(i)}
            >
              <div style={{ display: "flex", flexDirection: "column", alignItems: "center" }}>
                <div style={{
                  width: 10, height: 10, borderRadius: "50%",
                  background: i <= active ? p.color : "#1E293B",
                  border: `2px solid ${i === active ? p.color : "#2D3748"}`,
                  flexShrink: 0,
                  boxShadow: i === active ? `0 0 8px ${p.color}70` : "none",
                  transition: "all 0.2s",
                }} />
                {i < phaseData.length - 1 && (
                  <div style={{
                    width: 1, flex: 1, minHeight: 22,
                    background: i < active ? p.color + "50" : "#1E293B",
                    margin: "3px 0",
                  }} />
                )}
              </div>
              <div style={{ paddingBottom: 2 }}>
                <div style={{ fontSize: 9, color: p.color, letterSpacing: "0.1em" }}>{p.month}</div>
                <div style={{ fontSize: 10, color: "#94A3B8", fontWeight: 500, lineHeight: 1.3, marginTop: 2, fontFamily: "'Syne', sans-serif" }}>
                  {p.title}
                </div>
              </div>
            </div>
          ))}

          {/* Chains */}
          <div style={{ marginTop: 20, paddingTop: 18, borderTop: "1px solid #1A2233" }}>
            <div style={{ fontSize: 9, color: "#374151", letterSpacing: "0.2em", marginBottom: 12 }}>CHAINS</div>
            {[
              { name: "Ethereum", sub: "Base · Arbitrum", color: "#627EEA" },
              { name: "Solana", sub: "Raydium · Orca", color: "#9945FF" },
              { name: "Cosmos", sub: "Osmosis · IBC", color: "#00B5AD" },
            ].map((c, i) => (
              <div key={i} style={{ marginBottom: 10 }}>
                <div style={{ fontSize: 10, color: c.color, fontWeight: 600 }}>{c.name}</div>
                <div style={{ fontSize: 9, color: "#374151" }}>{c.sub}</div>
              </div>
            ))}
          </div>

          {/* Phase 1 quick summary */}
          {active === 0 && (
            <div style={{ marginTop: 20, paddingTop: 18, borderTop: "1px solid #1A2233" }}>
              <div style={{ fontSize: 9, color: "#374151", letterSpacing: "0.2em", marginBottom: 10 }}>M1 STATUS</div>
              {phaseData[0].milestones.map(m => (
                <div key={m.id} style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 7 }}>
                  <div style={{
                    width: 5, height: 5, borderRadius: "50%", flexShrink: 0,
                    background: m.status === "done" ? "#00FFD1" : m.status === "in-progress" ? "#FFD700" : "#374151",
                  }} />
                  <span style={{ fontSize: 9, color: "#475569", lineHeight: 1.3 }}>{m.title}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
