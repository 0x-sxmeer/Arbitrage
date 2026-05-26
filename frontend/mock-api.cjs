const http = require('http');

// ─────────────────────────────────────────────────────────────────────────────
//  Mock API v2 — MEV Engine Phases 1-4
//  Simulates all strategy outputs for the frontend dashboard
// ─────────────────────────────────────────────────────────────────────────────

// ── State ─────────────────────────────────────────────────────────────────────
let txCount       = 15000;
let oppCount      = 12;
let totalProfit   = 847.32;
let sessionStart  = Date.now();
let blockNumber   = 20493800;

// ── Phase 1: DEX arb opportunities ───────────────────────────────────────────
const oppTemplates = [
  { route: "WETH → USDC (Uniswap V3) → WETH (Aerodrome)", startToken: "WETH", baseProfit: 35.0, baseGas: 1.2 },
  { route: "USDC → WBTC (Uniswap V3) → WETH (SushiSwap) → USDC (Aerodrome)", startToken: "USDC", baseProfit: 120.0, baseGas: 2.1 },
  { route: "WETH → USDT (Uniswap V3) → WETH (Aerodrome)", startToken: "WETH", baseProfit: 55.4, baseGas: 0.9 },
  { route: "DAI → USDC (Curve) → WETH (Uniswap V2) → DAI (Balancer)", startToken: "DAI", baseProfit: 22.5, baseGas: 1.5 },
];

const mempoolTemplates = [
  { type: "SWAP", dex: "Uniswap V3", token: "USDC→ETH", color: "#00FFD1" },
  { type: "SWAP", dex: "Curve", token: "USDT→USDC", color: "#00FFD1" },
  { type: "ARB",  dex: "Flashbots", token: "ETH→USDC→ETH", color: "#FF6B6B" },
  { type: "ADD_LIQ", dex: "Aerodrome", token: "WETH/USDC", color: "#FFD700" },
  { type: "SWAP", dex: "Balancer", token: "WBTC→ETH", color: "#00FFD1" },
];

let recentOpps = [
  {
    id: "a1b2c3d4-e5f6-7a8b-9c0d-1e2f3a4b5c6d",
    route: "WETH → USDC (Uniswap V3) → WETH (Aerodrome)",
    input: "2.50 WETH", output: "2.52 WETH",
    nevUsd: 42.50, gasUsd: 1.20, isExecutable: true,
    block: 20493810, status: "Executed", ts: Date.now() - 4000
  },
];

// ── Phase 2: CEX-DEX opportunities ───────────────────────────────────────────
let cexDexOpps = [
  {
    id: "cex-001", symbol: "ETHUSDT", direction: "BuyDexSellCex",
    cexPrice: 3245.80, dexPrice: 3239.40, spreadPct: 0.197,
    sizeUsd: 500000, expectedProfitUsd: 985.0,
    confidence: 0.92, status: "Detected", ts: Date.now() - 2000
  },
];

const cexSymbols = ["ETHUSDT", "BTCUSDT", "WBTCUSDT"];
let binancePrices = { ETHUSDT: 3245.80, BTCUSDT: 94800.0, WBTCUSDT: 94750.0 };
let dexPrices     = { ETHUSDT: 3239.40, BTCUSDT: 94820.0, WBTCUSDT: 94730.0 };

// ── Phase 3: Liquidation events ───────────────────────────────────────────────
let liquidations = [
  {
    id: "liq-001", borrower: "0xf39Fd6e5...4Aa9", protocol: "AaveV3",
    healthFactor: 0.94, debtUsd: 42000, bonusUsd: 2100,
    netProfitUsd: 2095.0, status: "Executed", ts: Date.now() - 12000
  },
];

// ── Phase 4: Cross-chain opportunities ───────────────────────────────────────
let crossChainOpps = [
  {
    id: "cc-001", token: "ETH", buyChain: "Base", sellChain: "Optimism",
    buyPrice: 3238.40, sellPrice: 3246.20, spreadPct: 0.241,
    tradeSizeUsd: 50000, expectedProfitUsd: 120.5,
    status: "Detected", ts: Date.now() - 8000
  },
];

const chains = ["Base", "Optimism", "Arbitrum"];
let chainPrices = {
  "Base":     { ETH: 3238.40, WBTC: 94750.0 },
  "Optimism": { ETH: 3246.20, WBTC: 94810.0 },
  "Arbitrum": { ETH: 3241.80, WBTC: 94780.0 },
};

// ── Profit tracker ────────────────────────────────────────────────────────────
let profitHistory = [];
for (let i = 60; i >= 0; i--) {
  profitHistory.push({
    t: Date.now() - i * 60000,
    profit: Math.max(0, 400 + Math.random() * 200 - i * 2)
  });
}

// ─────────────────────────────────────────────────────────────────────────────
//  Simulation tick — updates state every 2s
// ─────────────────────────────────────────────────────────────────────────────
setInterval(() => {
  blockNumber++;
  txCount += Math.floor(Math.random() * 15);

  // Wander prices
  for (const sym of cexSymbols) {
    binancePrices[sym] *= (1 + (Math.random() - 0.5) * 0.0004);
    dexPrices[sym]     *= (1 + (Math.random() - 0.5) * 0.0006);
  }
  for (const chain of chains) {
    chainPrices[chain].ETH  *= (1 + (Math.random() - 0.5) * 0.0005);
    chainPrices[chain].WBTC *= (1 + (Math.random() - 0.5) * 0.0003);
  }

  // Phase 1: generate DEX arb
  if (Math.random() > 0.65) {
    oppCount++;
    const tmpl = oppTemplates[Math.floor(Math.random() * oppTemplates.length)];
    const profit = tmpl.baseProfit * (Math.random() * 1.8 + 0.5);
    const gas    = tmpl.baseGas * (Math.random() * 0.4 + 0.8);
    const nev    = profit - gas;
    if (nev > 1) totalProfit += nev;

    recentOpps.unshift({
      id: 'dex-' + Math.random().toString(16).slice(2, 10),
      route: tmpl.route,
      input: (Math.random() * 4 + 0.5).toFixed(4) + " " + tmpl.startToken,
      output: (Math.random() * 4 + 0.52).toFixed(4) + " " + tmpl.startToken,
      nevUsd: parseFloat(nev.toFixed(2)),
      gasUsd: parseFloat(gas.toFixed(2)),
      isExecutable: nev > 5,
      block: blockNumber,
      status: nev > 5 ? (Math.random() > 0.3 ? "Executed" : "Simulated") : "Unprofitable",
      ts: Date.now()
    });
    if (recentOpps.length > 50) recentOpps.pop();
  }

  // Phase 2: generate CEX-DEX spread
  if (Math.random() > 0.7) {
    const sym = cexSymbols[Math.floor(Math.random() * cexSymbols.length)];
    const cex = binancePrices[sym];
    const dex = dexPrices[sym];
    const spreadPct = Math.abs(cex - dex) / cex * 100;
    if (spreadPct > 0.1) {
      const profit = 500000 * spreadPct / 100 * 0.6;
      totalProfit += profit * 0.1;
      cexDexOpps.unshift({
        id: 'cex-' + Date.now(),
        symbol: sym,
        direction: cex > dex ? "BuyDexSellCex" : "SellDexBuyCex",
        cexPrice: parseFloat(cex.toFixed(2)),
        dexPrice: parseFloat(dex.toFixed(2)),
        spreadPct: parseFloat(spreadPct.toFixed(3)),
        sizeUsd: 500000,
        expectedProfitUsd: parseFloat(profit.toFixed(2)),
        confidence: parseFloat((0.7 + Math.random() * 0.28).toFixed(2)),
        status: Math.random() > 0.3 ? "Executed" : "Simulated",
        ts: Date.now()
      });
      if (cexDexOpps.length > 30) cexDexOpps.pop();
    }
  }

  // Phase 3: liquidation
  if (Math.random() > 0.93) {
    const bonus = 1000 + Math.random() * 3000;
    totalProfit += bonus * 0.15;
    liquidations.unshift({
      id: 'liq-' + Date.now(),
      borrower: '0x' + Math.random().toString(16).slice(2, 12) + '...',
      protocol: Math.random() > 0.5 ? "AaveV3" : "Moonwell",
      healthFactor: parseFloat((0.85 + Math.random() * 0.14).toFixed(4)),
      debtUsd: parseFloat((10000 + Math.random() * 80000).toFixed(0)),
      bonusUsd: parseFloat(bonus.toFixed(2)),
      netProfitUsd: parseFloat((bonus * 0.95).toFixed(2)),
      status: "Executed",
      ts: Date.now()
    });
    if (liquidations.length > 20) liquidations.pop();
  }

  // Phase 4: cross-chain
  if (Math.random() > 0.75) {
    const buyChain  = chains[Math.floor(Math.random() * chains.length)];
    const otherChains = chains.filter(c => c !== buyChain);
    const sellChain = otherChains[Math.floor(Math.random() * otherChains.length)];
    const buyP  = chainPrices[buyChain].ETH;
    const sellP = chainPrices[sellChain].ETH;
    const spreadPct = (sellP - buyP) / buyP * 100;
    if (spreadPct > 0.2) {
      const profit = 50000 * spreadPct / 100 * 0.7;
      totalProfit += profit * 0.1;
      crossChainOpps.unshift({
        id: 'cc-' + Date.now(),
        token: "ETH",
        buyChain, sellChain,
        buyPrice: parseFloat(buyP.toFixed(2)),
        sellPrice: parseFloat(sellP.toFixed(2)),
        spreadPct: parseFloat(spreadPct.toFixed(3)),
        tradeSizeUsd: 50000,
        expectedProfitUsd: parseFloat(profit.toFixed(2)),
        status: Math.random() > 0.4 ? "Executed" : "Simulated",
        ts: Date.now()
      });
      if (crossChainOpps.length > 20) crossChainOpps.pop();
    }
  }

  // Profit history
  profitHistory.push({ t: Date.now(), profit: totalProfit });
  if (profitHistory.length > 120) profitHistory.shift();

}, 2000);

// ─────────────────────────────────────────────────────────────────────────────
//  HTTP Server
// ─────────────────────────────────────────────────────────────────────────────
const server = http.createServer((req, res) => {
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Methods', 'GET, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type');
  res.setHeader('Content-Type', 'application/json');

  if (req.method === 'OPTIONS') { res.writeHead(204); res.end(); return; }

  const url = req.url.split('?')[0];

  const routes = {
    '/api/metrics': () => ({
      graph_pools:          342,
      txs_seen:             txCount,
      txs_decoded:          txCount - 150,
      opportunities_found:  oppCount,
      total_profit_usd:     parseFloat(totalProfit.toFixed(2)),
      uptime_secs:          Math.floor((Date.now() - sessionStart) / 1000),
      block_number:         blockNumber,
      execute_enabled:      true,
      phases: {
        phase1_active:      true,
        phase2_active:      true,
        phase3_active:      true,
        phase4_active:      true,
      },
      phase_profits: {
        dex_arb:     parseFloat((totalProfit * 0.25).toFixed(2)),
        cex_dex:     parseFloat((totalProfit * 0.45).toFixed(2)),
        liquidations: parseFloat((totalProfit * 0.20).toFixed(2)),
        cross_chain:  parseFloat((totalProfit * 0.10).toFixed(2)),
      }
    }),

    '/api/mempool': () => Array.from({length: 8}).map(() => {
      const tmpl = mempoolTemplates[Math.floor(Math.random() * mempoolTemplates.length)];
      return {
        id: Math.random().toString(),
        hash: '0x' + Math.random().toString(16).slice(2, 10) + '...',
        type: tmpl.type, dex: tmpl.dex, token: tmpl.token,
        size: '$' + Math.floor(Math.random() * 500 + 10) + 'k',
        color: tmpl.color,
        gasGwei: (0.001 + Math.random() * 0.03).toFixed(4)
      };
    }),

    '/api/opportunities':  () => recentOpps,
    '/api/cex-dex':        () => ({
      opportunities: cexDexOpps,
      prices: Object.fromEntries(cexSymbols.map(s => [s, {
        binance: parseFloat(binancePrices[s].toFixed(2)),
        dex:     parseFloat(dexPrices[s].toFixed(2)),
        spread:  parseFloat(Math.abs(binancePrices[s] - dexPrices[s]) / binancePrices[s] * 100).toFixed(3)
      }]))
    }),
    '/api/liquidations':   () => liquidations,
    '/api/cross-chain':    () => ({
      opportunities: crossChainOpps,
      prices: chainPrices
    }),
    '/api/profit-history': () => profitHistory,
    '/api/pools':          () => [],
  };

  const handler = routes[url];
  if (handler) {
    res.writeHead(200);
    res.end(JSON.stringify(handler()));
  } else {
    res.writeHead(404);
    res.end(JSON.stringify({ error: 'Not found' }));
  }
});

server.listen(3000, () => {
  console.log('🚀 MEV Engine v2 Mock API running on http://localhost:3000');
  console.log('   Endpoints: /api/metrics | /api/mempool | /api/opportunities');
  console.log('             /api/cex-dex  | /api/liquidations | /api/cross-chain');
  console.log('             /api/profit-history | /api/pools');
});
