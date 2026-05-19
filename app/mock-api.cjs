const http = require('http');

const templates = [
  { type: "SWAP", dex: "Uniswap V3", token: "USDC→ETH", color: "#00FFD1" },
  { type: "SWAP", dex: "Curve", token: "USDT→USDC", color: "#00FFD1" },
  { type: "ARB", dex: "Flashbots", token: "ETH→USDC→ETH", color: "#FF6B6B" },
  { type: "ADD_LIQ", dex: "Aerodrome", token: "WETH/USDC", color: "#FFD700" },
  { type: "REMOVE_LIQ", dex: "Balancer", token: "WBTC/ETH", color: "#FB923C" }
];

let txCount = 15000;
let oppCount = 12;

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
    if (Math.random() > 0.95) oppCount += 1;
    
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
        color: tmpl.color
      };
    });
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(liveTxs));
  } else {
    res.writeHead(404);
    res.end();
  }
});

server.listen(3000, () => {
  console.log('Mock Telemetry API running on port 3000');
});
