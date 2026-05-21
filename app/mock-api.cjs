const http = require('http');

const templates = [
  { type: "SWAP", dex: "Uniswap V3", token: "USDC→ETH", color: "#00FFD1" },
  { type: "SWAP", dex: "Curve", token: "USDT→USDC", color: "#00FFD1" },
  { type: "ARB", dex: "Flashbots", token: "ETH→USDC→ETH", color: "#FF6B6B" },
  { type: "ADD_LIQ", dex: "Aerodrome", token: "WETH/USDC", color: "#FFD700" },
  { type: "REMOVE_LIQ", dex: "Balancer", token: "WBTC/ETH", color: "#FB923C" }
];

const oppTemplates = [
  { route: "WETH → USDC (Uniswap V3) → WETH (Aerodrome)", startToken: "WETH", baseProfit: 35.0, baseGas: 10.5 },
  { route: "USDC → WBTC (Uniswap V3) → WETH (SushiSwap) → USDC (Aerodrome)", startToken: "USDC", baseProfit: 120.0, baseGas: 22.0 },
  { route: "WBTC → WETH (Uniswap V3) → WBTC (Aerodrome)", startToken: "WBTC", baseProfit: -8.0, baseGas: 12.0 },
  { route: "DAI → USDC (Uniswap V3) → DAI (Curve)", startToken: "DAI", baseProfit: -2.5, baseGas: 8.5 },
  { route: "WETH → USDT (Uniswap V3) → WETH (Aerodrome)", startToken: "WETH", baseProfit: 55.4, baseGas: 9.8 }
];

let txCount = 15000;
let oppCount = 12;

let recentOpps = [
  {
    id: "a1b2c3d4-e5f6-7a8b-9c0d-1e2f3a4b5c6d",
    route: "WETH → USDC (Uniswap V3) → WETH (Aerodrome)",
    input: "2.50 WETH",
    output: "2.52 WETH",
    nevUsd: 42.50,
    gasUsd: 11.20,
    isExecutable: true,
    block: 20493810,
    status: "Executed",
    ts: Date.now() - 4000
  },
  {
    id: "b2c3d4e5-f6a7-8b9c-0d1e-2f3a4b5c6d7e",
    route: "USDC → WETH (Uniswap V3) → DAI (Aerodrome) → USDC (Curve)",
    input: "5000.00 USDC",
    output: "4988.50 USDC",
    nevUsd: -15.40,
    gasUsd: 14.50,
    isExecutable: false,
    block: 20493811,
    status: "Unprofitable",
    ts: Date.now() - 12000
  }
];

const server = http.createServer((req, res) => {
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type');

  if (req.method === 'OPTIONS') {
    res.writeHead(204);
    res.end();
    return;
  }

  if (req.url === '/api/metrics') {
    txCount += Math.floor(Math.random() * 15);
    
    // Periodically add new mock opportunities
    if (Math.random() > 0.8) {
      oppCount += 1;
      const tmpl = oppTemplates[Math.floor(Math.random() * oppTemplates.length)];
      const isProfitable = Math.random() > 0.45;
      const profitMult = isProfitable ? (Math.random() * 2.5 + 1.1) : (Math.random() * 0.8 - 1.2);
      const nevUsd = parseFloat((tmpl.baseProfit * profitMult).toFixed(2));
      const gasUsd = parseFloat((tmpl.baseGas * (Math.random() * 0.4 + 0.8)).toFixed(2));
      const isExecutable = nevUsd > 10.0;
      
      let inputVal = (Math.random() * 4 + 1);
      if (tmpl.startToken === "USDC" || tmpl.startToken === "DAI") inputVal *= 1000;
      if (tmpl.startToken === "WBTC") inputVal *= 0.1;
      
      const inputStr = inputVal.toFixed(tmpl.startToken === "WBTC" ? 4 : 2) + " " + tmpl.startToken;
      const ethPrice = 3000.0;
      const btcPrice = 95000.0;
      const tokenPrice = tmpl.startToken === "WBTC" ? btcPrice : (tmpl.startToken === "WETH" ? ethPrice : 1.0);
      
      const outputVal = inputVal + (nevUsd + gasUsd) / tokenPrice;
      const outputStr = outputVal.toFixed(tmpl.startToken === "WBTC" ? 4 : 2) + " " + tmpl.startToken;

      recentOpps.unshift({
        id: 'f' + Math.random().toString(16).slice(2, 10) + '-opp-4abc',
        route: tmpl.route,
        input: inputStr,
        output: outputStr,
        nevUsd: nevUsd,
        gasUsd: gasUsd,
        isExecutable: isExecutable,
        block: 20493812 + Math.floor(txCount / 10),
        status: isExecutable ? (Math.random() > 0.25 ? "Executed" : "Simulated") : "Unprofitable",
        ts: Date.now()
      });
      if (recentOpps.length > 50) recentOpps.pop();
    }
    
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({
      graph_pools: 342,
      cache_hits: 15420,
      cache_misses: 230,
      txs_seen: txCount,
      opportunities_found: oppCount,
      txs_decoded: txCount - 150
    }));
  } else if (req.url === '/api/mempool') {
    const liveTxs = Array.from({length: 6}).map((_, i) => {
      const tmpl = templates[Math.floor(Math.random() * templates.length)];
      return {
        id: Math.random().toString(),
        hash: '0x' + Math.random().toString(16).slice(2, 10) + '...',
        type: tmpl.type,
        dex: tmpl.dex,
        token: tmpl.token,
        size: '$' + Math.floor(Math.random() * 500 + 10) + 'k',
        color: tmpl.color,
        gasGwei: (15.0 + Math.random() * 35).toFixed(1)
      };
    });
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(liveTxs));
  } else if (req.url === '/api/opportunities') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(recentOpps));
  } else {
    res.writeHead(404);
    res.end();
  }
});

server.listen(3000, () => {
  console.log('Mock Telemetry API running on port 3000');
});
