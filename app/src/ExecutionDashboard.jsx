import React, { useState, useEffect, useRef } from "react";
import { MempoolStream } from './ArbRoadmap';

// Precision Time Helper
const formatTime = (ts) => {
  if (!ts) return "-";
  const date = new Date(Number(ts));
  const hrs = String(date.getHours()).padStart(2, "0");
  const mins = String(date.getMinutes()).padStart(2, "0");
  const secs = String(date.getSeconds()).padStart(2, "0");
  const ms = String(date.getMilliseconds()).padStart(3, "0");
  return `${hrs}:${mins}:${secs}.${ms}`;
};

// Removed MOCK_OPP to enforce real data only

export default function ExecutionDashboard({ color = "#A78BFA" }) {
  // 'txs' state removed to fix lint error
  const [opportunities, setOpportunities] = useState([]);
  const [metrics, setMetrics] = useState(null);
  const [pools, setPools] = useState([]);
  const [isOnline, setIsOnline] = useState(false);
  const [now, setNow] = useState(0);
  const [logs, setLogs] = useState([]);
  const [autoScroll, setAutoScroll] = useState(true);
  const metricsRef = useRef(null);
  
  // Filters
  const [oppSearch, setOppSearch] = useState("");
  const [execOnly, setExecOnly] = useState(false);
  const [poolSearch, setPoolSearch] = useState("");

  const terminalContainerRef = useRef(null);
  const loggedIds = useRef(new Set());

  // Poll local REST API on port 3000 (Live Rust Bot)
  useEffect(() => {
    const fetchData = async () => {
      setNow(Date.now());
      try {
        const [txsRes, oppsRes, metricsRes, poolsRes] = await Promise.all([
          fetch("http://localhost:3000/api/mempool"),
          fetch("http://localhost:3000/api/opportunities"),
          fetch("http://localhost:3000/api/metrics"),
          fetch("http://localhost:3000/api/pools")
        ]);
        
        if (txsRes.ok && oppsRes.ok && metricsRes.ok && poolsRes.ok) {
          await txsRes.json(); // Consumed but not stored to fix lint error
          const oData = await oppsRes.json();
          const mData = await metricsRes.json();
          const pData = await poolsRes.json();
          
          // setTxs(tData) removed since 'txs' is unused
          setOpportunities(oData);
          setMetrics(mData);
          setPools(pData);
          metricsRef.current = mData;
          setIsOnline(true);
        } else {
          setIsOnline(false);
        }
      } catch {
        setIsOnline(false);
      }
    };

    fetchData();
    const iv = setInterval(fetchData, 800);
    return () => clearInterval(iv);
  }, []);

  // Sync hacker logs based on opportunities
  useEffect(() => {
    setLogs(prev => {
      let next = [...prev];
      let changed = false;

      const oppsToLog = Array.isArray(opportunities) ? opportunities : [];

      oppsToLog.forEach(opp => {
        if (!loggedIds.current.has(opp.id)) {
          loggedIds.current.add(opp.id);
          changed = true;
          const isProfitable = opp.nevUsd > 0;
          
          next.push({
            type: "exec",
            text: `[${formatTime(opp.ts)}] BELLMAN-FORD: cycle found ${Array.isArray(opp.route) ? opp.route.map(r=>r.dex).join("->") : "UNK"}`
          });
          
          if (isProfitable) {
            next.push({
              type: "success",
              text: `[${formatTime(opp.ts)}] PGA SOLVER: Bidding ${opp.optimalGasGwei || "MAX"} Gwei to secure block. Est NEV: $${opp.nevUsd?.toFixed(2)}`
            });
            next.push({
              type: "executable",
              text: `[${formatTime(opp.ts)}] EXECUTOR: Dispatching atomic bundle via Flashbots RPC...`
            });
          } else {
            next.push({
              type: "unprofitable",
              text: `[${formatTime(opp.ts)}] RISK ENGINE: Discarded. Gas > Spread. (${opp.gasUsd} > ${opp.grossUsd})`
            });
          }
        }
      });
      
      if (next.length > 200) next = next.slice(-200);
      return changed ? next : prev;
    });
  }, [opportunities]);

  // Heartbeat logs for Hacker Telemetry when no opportunities are found
  useEffect(() => {
    if (!isOnline) return;
    const interval = setInterval(() => {
      const currentMetrics = metricsRef.current || {};
      setLogs(prev => {
        const next = [...prev, {
          type: "pending",
          text: `[${formatTime(Date.now())}] ORCHESTRATOR: Graph synchronized. ${currentMetrics.graph_pools || 0} active pools | ${currentMetrics.txs_decoded || 0} mempool txs parsed | Bellman-Ford listening...`
        }];
        return next.length > 200 ? next.slice(-200) : next;
      });
    }, 2500);
    return () => clearInterval(interval);
  }, [isOnline]);

  // Auto-scroll terminal
  useEffect(() => {
    if (autoScroll && terminalContainerRef.current) {
      terminalContainerRef.current.scrollTo({
        top: terminalContainerRef.current.scrollHeight,
        behavior: "smooth"
      });
    }
  }, [logs, autoScroll]);

  // Visual helper
  const renderRouteChevrons = (route) => {
    if (!Array.isArray(route)) return null;
    return (
      <div style={{ display: "flex", alignItems: "center", gap: 4, flexWrap: "wrap" }}>
        {route.map((node, idx) => (
          <React.Fragment key={idx}>
            <div style={{
              background: "rgba(255,255,255,0.05)",
              border: "1px solid rgba(255,255,255,0.1)",
              padding: "2px 6px",
              borderRadius: 4,
              fontSize: 10,
              fontFamily: "monospace",
              color: "#CBD5E1"
            }}>
              <span style={{ color: color }}>{node.dex}</span>
              <span style={{ color: "#475569", margin: "0 4px" }}>|</span>
              {node.tokenOut}
            </div>
            {idx < route.length - 1 && (
              <div style={{ color: "#475569", fontSize: 10 }}>{">"}</div>
            )}
          </React.Fragment>
        ))}
      </div>
    );
  };

  const filteredOpps = (Array.isArray(opportunities) ? opportunities : []).filter(opp => {
    const routeArr = Array.isArray(opp.route) ? opp.route : [];
    const routeStr = routeArr.map(r => `${r.dex}-${r.tokenOut}`).join(" ");
    const inputStr = String(opp.input || "");
    const search = String(oppSearch || "").toLowerCase();
    const matchesSearch = routeStr.toLowerCase().includes(search) ||
                          inputStr.toLowerCase().includes(search);
    const matchesExec = !execOnly || opp.isExecutable;
    return matchesSearch && matchesExec;
  });

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 24 }} className="phase-content">
      {/* Premium Connection Status Banner */}
      <div style={{
        display: "flex",
        justifyContent: "space-between",
        alignItems: "center",
        padding: "12px 20px",
        background: isOnline ? "rgba(16, 185, 129, 0.05)" : "rgba(239, 68, 68, 0.05)",
        border: `1px solid ${isOnline ? "#10B98130" : "#EF444430"}`,
        borderRadius: 8,
        boxShadow: "inset 0 1px 0 rgba(255,255,255,0.03)",
      }}>
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <div style={{
              width: 8, height: 8, borderRadius: "50%",
              background: isOnline ? "#10B981" : "#EF4444",
              boxShadow: `0 0 10px ${isOnline ? "#10B981" : "#EF4444"}`,
              animation: isOnline ? "pulse 2s infinite" : "none"
            }} />
            <span style={{ fontSize: 11, fontWeight: 700, color: isOnline ? "#10B981" : "#EF4444", letterSpacing: "0.05em" }}>
              {isOnline ? "ENGINE CONNECTED" : "ENGINE OFFLINE"}
            </span>
          </div>
          <span style={{ color: "#475569" }}>|</span>
          <span style={{ fontSize: 10, color: "#94A3B8", fontFamily: "monospace" }}>
            MEMPOOL TXS: <strong style={{ color: "#E2E8F0" }}>{metrics?.txs_decoded || 0}</strong>
          </span>
          <span style={{ color: "#475569" }}>|</span>
          <span style={{ fontSize: 10, color: "#94A3B8", fontFamily: "monospace" }}>
            CYCLES FOUND: <strong style={{ color: "#E2E8F0" }}>{metrics?.opportunities_found || 0}</strong>
          </span>
        </div>
      </div>

      {!isOnline ? (
        <div style={{ padding: "40px", textAlign: "center", border: "1px dashed #1A2233", borderRadius: 8 }}>
          <div style={{ fontSize: 24, marginBottom: 12 }}>🔌</div>
          <div style={{ fontSize: 13, color: "#94A3B8", marginBottom: 6 }}>Rust Execution Engine is currently offline.</div>
          <div style={{ fontSize: 11, color: "#475569" }}>Run <code>cargo run --bin arb-engine</code> to start receiving mempool data.</div>
        </div>
      ) : (
        <>
          <div style={{ display: "flex", gap: 20 }}>
            <div style={{ flex: 1 }}>
              <MempoolStream />
            </div>
            <div style={{ flex: 1, border: "1px solid #1A2233", borderRadius: 8, overflow: "hidden", display: "flex", flexDirection: "column" }}>
              <div style={{ padding: "12px 16px", borderBottom: "1px solid #1A2233", background: "#080B10", display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                    <div style={{ width: 8, height: 8, borderRadius: "50%", background: "#FF007A", boxShadow: "0 0 8px #FF007A" }} className="live-dot" />
                    <span style={{ fontSize: 11, fontWeight: 700, color: "#E2E8F0", letterSpacing: "0.1em" }}>HACKER TELEMETRY CLI</span>
                  </div>
                  <span style={{ fontSize: 8, color: "#64748B", fontFamily: "monospace" }}>Real-time sandbox simulation & core orchestration logs</span>
                </div>
              </div>
              <div 
                ref={terminalContainerRef}
                style={{ padding: "14px", flex: 1, maxHeight: "344px", overflowY: "auto", fontFamily: "'Fira Code', 'IBM Plex Mono', 'Courier New', monospace", fontSize: "10px", lineHeight: "1.5", background: "#020305", display: "flex", flexDirection: "column", gap: 6 }}
                onScroll={(e) => {
                  const { scrollTop, scrollHeight, clientHeight } = e.target;
                  setAutoScroll(scrollHeight - scrollTop - clientHeight < 20);
                }}
              >
                {logs.map((log, idx) => {
                  let logColor = "#94A3B8";
                  if (log.type === "success") logColor = "#10B981";
                  else if (log.type === "swap") logColor = "#00FFD1";
                  else if (log.type === "pending") logColor = "#64748B";
                  else if (log.type === "executable") logColor = "#FF007A";
                  else if (log.type === "unprofitable") logColor = "#EF4444";
                  else if (log.type === "console") logColor = "#FFD700";
                  else if (log.type === "warn") logColor = "#F59E0B";
                  else if (log.type === "exec") logColor = "#A78BFA";
                  return (
                    <div key={idx} style={{ color: logColor, whiteSpace: "pre-wrap", wordBreak: "break-all" }}>{log.text}</div>
                  );
                })}
              </div>
            </div>
          </div>

          <div style={{ border: "1px solid #1A2233", borderRadius: 8, overflow: "hidden", background: "#0D1117" }}>
            <div style={{ padding: "16px 20px", borderBottom: "1px solid #1A2233", background: "#0A0E14", display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: 12 }}>
              <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                  <div className="live-dot" style={{ width: 6, height: 6, borderRadius: "50%", background: "#A78BFA" }} />
                  <span style={{ fontSize: 12, fontWeight: 600, color: "#E2E8F0", letterSpacing: "0.08em" }}>DETECTED ARBITRAGE OPPORTUNITIES</span>
                </div>
                <span style={{ fontSize: 9, color: "#64748B", fontFamily: "monospace" }}>Live Bellman-Ford pathfinding results. Displays profitable cyclic routes across DEXs.</span>
              </div>
              <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
                <input 
                  type="text" 
                  placeholder="Search route/path..." 
                  value={oppSearch}
                  onChange={e => setOppSearch(e.target.value)}
                  style={{ background: "#080B0F", border: "1px solid #1A2233", borderRadius: 4, padding: "4px 10px", fontSize: 10, color: "#E2E8F0", fontFamily: "monospace", outline: "none", width: 160 }}
                />
                <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 10, color: "#64748B", cursor: "pointer", userSelect: "none" }}>
                  <input type="checkbox" checked={execOnly} onChange={e => setExecOnly(e.target.checked)} style={{ accentColor: "#A78BFA", cursor: "pointer" }} />
                  Simulated Only
                </label>
              </div>
            </div>

            <div style={{ maxHeight: 350, overflowY: "auto" }}>
              <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 11, textAlign: "left" }}>
                <thead>
                  <tr style={{ borderBottom: "1px solid #1A2233", background: "rgba(0,0,0,0.2)" }}>
                    <th style={{ padding: "10px 16px", color: "#475569", fontWeight: 600 }}>TIME</th>
                    <th style={{ padding: "10px 16px", color: "#475569", fontWeight: 600 }} title="The sequential DEX path evaluated by Bellman-Ford">PATHWAY / LIQUIDITY ROUTE</th>
                    <th style={{ padding: "10px 16px", color: "#475569", fontWeight: 600 }}>BLOCK</th>
                    <th style={{ padding: "10px 16px", color: "#475569", fontWeight: 600 }} title="Amount of tokens inputted (e.g. flash loan size)">INPUT</th>
                    <th style={{ padding: "10px 16px", color: "#475569", fontWeight: 600 }} title="Total output token value before gas deduction">GROSS YIELD</th>
                    <th style={{ padding: "10px 16px", color: "#475569", fontWeight: 600 }} title="Estimated base L2 gas cost in USD">GAS COST</th>
                    <th style={{ padding: "10px 16px", color: "#A78BFA", fontWeight: 700 }} title="Maximum gwei we can bid (Priority Gas Auction) and still break even">MAX PGA TIP</th>
                    <th style={{ padding: "10px 16px", color: "#475569", fontWeight: 600 }} title="Net Expected Value in USD after gas costs (must be >0 for execution)">NEV (PROFIT)</th>
                    <th style={{ padding: "10px 16px", color: "#475569", fontWeight: 600 }}>STATUS</th>
                  </tr>
                </thead>
                <tbody>
                  {filteredOpps.length === 0 ? (
                    <tr>
                      <td colSpan="9">
                        <div style={{ padding: "50px", textAlign: "center", color: "#475569", display: "flex", flexDirection: "column", alignItems: "center", gap: 12 }}>
                          <div style={{ fontSize: 24 }}>🔍</div>
                          <div style={{ fontSize: 11, fontWeight: 600, color: color }}>SCANNING GRAPH FOR NEGATIVE CYCLES...</div>
                          <div style={{ fontSize: 10, maxWidth: 350, lineHeight: 1.5 }}>The Bellman-Ford algorithm is analyzing the updated reserves from the mempool feed to find profitable triangular arbitrage opportunities.</div>
                        </div>
                      </td>
                    </tr>
                  ) : (
                    filteredOpps.map((opp, i) => {
                      const isProfitable = opp.nevUsd > 0;
                      const isNew = now - opp.ts < 1200;
                      const flashClass = isNew ? "row-flash-purple" : "";
                      return (
                        <tr key={opp.id || i} style={{ 
                          borderBottom: i < filteredOpps.length - 1 ? "1px solid #0D1117" : "none",
                          background: opp.isExecutable ? "rgba(0,255,209,0.02)" : "transparent",
                          transition: "background 0.2s"
                        }} className={`${flashClass} milestone-card`}>
                          <td style={{ padding: "10px 16px", fontFamily: "monospace", color: "#64748B" }}>{formatTime(opp.ts)}</td>
                          <td style={{ padding: "10px 16px", color: "#CBD5E1" }}>{renderRouteChevrons(opp.route)}</td>
                          <td style={{ padding: "10px 16px", fontFamily: "monospace" }}>
                            <a href={`https://basescan.org/block/${opp.block}`} target="_blank" rel="noopener noreferrer" style={{ color: color, textDecoration: "none", borderBottom: `1px dashed ${color}50` }}>{opp.block}</a>
                          </td>
                          <td style={{ padding: "10px 16px", fontFamily: "monospace", color: "#94A3B8" }}>{opp.input}</td>
                          <td style={{ padding: "10px 16px", fontFamily: "monospace", color: "#E2E8F0" }}>{opp.output}</td>
                          <td style={{ padding: "10px 16px", fontFamily: "monospace", color: "#FF6B6B" }}>${opp.gasUsd?.toFixed(2)}</td>
                          <td style={{ padding: "10px 16px", fontFamily: "monospace", color: "#E2E8F0" }}>
                            {opp.optimalGasGwei !== undefined ? (
                              <span style={{ 
                                background: opp.optimalGasGwei > (opp.baseGasGwei || 0) ? "rgba(255, 215, 0, 0.15)" : "transparent",
                                color: opp.optimalGasGwei > (opp.baseGasGwei || 0) ? "#FFD700" : "#94A3B8",
                                padding: "2px 6px", borderRadius: "4px"
                              }}>
                                {opp.optimalGasGwei.toFixed(1)} gwei
                              </span>
                            ) : "—"}
                          </td>
                          <td style={{ padding: "10px 16px", fontFamily: "monospace", color: isProfitable ? "#00FFD1" : "#FF6B6B", fontWeight: 700 }}>
                            {isProfitable ? "+" : ""}${opp.nevUsd?.toFixed(2)}
                          </td>
                          <td style={{ padding: "10px 16px" }}>
                              <span style={{
                                fontSize: 8, padding: "1px 5px", borderRadius: 2,
                                background: opp.isExecutable ? "rgba(0,255,209,0.1)" : "rgba(239,68,68,0.1)",
                                color: opp.isExecutable ? "#00FFD1" : "#FF6B6B",
                                border: `1px solid ${opp.isExecutable ? "#00FFD130" : "#FF6B6B20"}`
                              }}>
                                {opp.status}
                              </span>
                          </td>
                        </tr>
                      );
                    })
                  )}
                </tbody>
              </table>
            </div>
          </div>

          {/* ACTIVE POOLS REGISTRY */}
          <div style={{ border: "1px solid #1A2233", borderRadius: 8, overflow: "hidden", background: "#0D1117" }}>
            <div style={{ padding: "16px 20px", borderBottom: "1px solid #1A2233", background: "#0A0E14", display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: 12 }}>
              <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                  <div className="live-dot" style={{ width: 6, height: 6, borderRadius: "50%", background: "#00FFD1" }} />
                  <span style={{ fontSize: 12, fontWeight: 600, color: "#E2E8F0", letterSpacing: "0.08em" }}>ACTIVE POOLS REGISTRY</span>
                </div>
                <span style={{ fontSize: 9, color: "#64748B", fontFamily: "monospace" }}>Loaded pools currently included in Bellman-Ford execution arbitrage.</span>
              </div>
              <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
                <span style={{ fontSize: 10, color: "#94A3B8", fontFamily: "monospace", background: "rgba(255,255,255,0.05)", padding: "4px 8px", borderRadius: 4 }}>
                  Total Pools: <strong style={{ color: "#00FFD1" }}>{pools.length}</strong>
                </span>
                <input 
                  type="text" 
                  placeholder="Filter token or dex..." 
                  value={poolSearch}
                  onChange={e => setPoolSearch(e.target.value)}
                  style={{ background: "#080B0F", border: "1px solid #1A2233", borderRadius: 4, padding: "4px 10px", fontSize: 10, color: "#E2E8F0", fontFamily: "monospace", outline: "none", width: 160 }}
                />
              </div>
            </div>

            <div style={{ maxHeight: 250, overflowY: "auto", background: "#06080A" }}>
              <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 11, textAlign: "left" }}>
                <thead>
                  <tr style={{ borderBottom: "1px solid #1A2233", background: "rgba(0,0,0,0.4)" }}>
                    <th style={{ padding: "12px 20px", color: "#64748B", fontWeight: 600, letterSpacing: "0.05em", fontSize: 10 }}>POOL ID</th>
                    <th style={{ padding: "12px 20px", color: "#64748B", fontWeight: 600, letterSpacing: "0.05em", fontSize: 10 }}>CHAIN</th>
                    <th style={{ padding: "12px 20px", color: "#64748B", fontWeight: 600, letterSpacing: "0.05em", fontSize: 10 }}>DEX ENGINE</th>
                    <th style={{ padding: "12px 20px", color: "#64748B", fontWeight: 600, letterSpacing: "0.05em", fontSize: 10 }}>PAIRING</th>
                    <th style={{ padding: "12px 20px", color: "#64748B", fontWeight: 600, letterSpacing: "0.05em", fontSize: 10 }}>FEE TIER</th>
                  </tr>
                </thead>
                <tbody>
                  {pools.length === 0 ? (
                    <tr>
                      <td colSpan="5">
                        <div style={{ padding: "40px", textAlign: "center", color: "#475569", fontSize: 11, display: "flex", flexDirection: "column", gap: 8, alignItems: "center" }}>
                          <div style={{ fontSize: 20 }}>📊</div>
                          <div>No pools actively loaded.</div>
                        </div>
                      </td>
                    </tr>
                  ) : (
                    pools
                      .filter(p => {
                        if (!poolSearch) return true;
                        const search = String(poolSearch).toLowerCase();
                        return String(p.tokenA || "").toLowerCase().includes(search) || 
                               String(p.tokenB || "").toLowerCase().includes(search) ||
                               String(p.dex || "").toLowerCase().includes(search) ||
                               String(p.id || "").toLowerCase().includes(search);
                      })
                      .slice(0, 100) // Show up to 100 for performance
                      .map((pool) => {
                        // Dynamically resolve DEX styling
                        const dexLower = String(pool.dex).toLowerCase();
                        let dexTheme = { bg: "rgba(148, 163, 184, 0.1)", text: "#94A3B8", border: "rgba(148, 163, 184, 0.2)" };
                        if (dexLower.includes("uniswap")) dexTheme = { bg: "rgba(255, 0, 122, 0.1)", text: "#FF007A", border: "rgba(255, 0, 122, 0.2)" };
                        else if (dexLower.includes("aerodrome")) dexTheme = { bg: "rgba(0, 82, 255, 0.1)", text: "#4C82FF", border: "rgba(0, 82, 255, 0.2)" };
                        else if (dexLower.includes("sushiswap")) dexTheme = { bg: "rgba(250, 82, 160, 0.1)", text: "#FA52A0", border: "rgba(250, 82, 160, 0.2)" };
                        else if (dexLower.includes("pancake")) dexTheme = { bg: "rgba(31, 199, 212, 0.1)", text: "#1FC7D4", border: "rgba(31, 199, 212, 0.2)" };

                        return (
                          <tr key={pool.id} style={{ borderBottom: "1px solid rgba(26, 34, 51, 0.5)", transition: "background 0.2s" }} onMouseEnter={(e) => e.currentTarget.style.background = "rgba(255,255,255,0.02)"} onMouseLeave={(e) => e.currentTarget.style.background = "transparent"}>
                            <td style={{ padding: "12px 20px", fontFamily: "monospace", color: "#94A3B8" }}>
                              <a 
                                href={`https://dexscreener.com/base/${pool.id}`} 
                                target="_blank" 
                                rel="noopener noreferrer" 
                                style={{ color: "#38BDF8", textDecoration: "none", display: "inline-flex", alignItems: "center", gap: 6, transition: "color 0.2s" }}
                                onMouseEnter={(e) => e.currentTarget.style.color = "#BAE6FD"}
                                onMouseLeave={(e) => e.currentTarget.style.color = "#38BDF8"}
                                title="View real-time charts & liquidity on DexScreener"
                              >
                                {pool.id.substring(0, 8)}...{pool.id.slice(-6)}
                                <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"></path><polyline points="15 3 21 3 21 9"></polyline><line x1="10" y1="14" x2="21" y2="3"></line></svg>
                              </a>
                            </td>
                            <td style={{ padding: "12px 20px" }}>
                              <div style={{ display: "inline-flex", alignItems: "center", gap: 6, background: "rgba(255, 255, 255, 0.05)", padding: "2px 8px", borderRadius: 12, border: "1px solid rgba(255,255,255,0.05)" }}>
                                <div style={{ width: 12, height: 12, borderRadius: "50%", background: "linear-gradient(135deg, #0052FF, #00D1FF)" }} />
                                <span style={{ color: "#E2E8F0", fontSize: 10, fontWeight: 600 }}>{pool.chain}</span>
                              </div>
                            </td>
                            <td style={{ padding: "12px 20px" }}>
                              <span style={{
                                background: dexTheme.bg,
                                color: dexTheme.text,
                                border: `1px solid ${dexTheme.border}`,
                                padding: "4px 10px",
                                borderRadius: 12,
                                fontSize: 10,
                                fontWeight: 700,
                                letterSpacing: "0.02em"
                              }}>
                                {pool.dex}
                              </span>
                            </td>
                            <td style={{ padding: "12px 20px" }}>
                              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                                <div style={{ display: "flex", alignItems: "center", gap: 6, background: "#0F172A", padding: "3px 8px", borderRadius: 6, border: "1px solid #1E293B" }}>
                                  <span style={{ fontSize: 11, fontWeight: 600, color: "#F8FAFC" }}>{pool.tokenA}</span>
                                </div>
                                <span style={{ color: "#475569", fontSize: 10 }}>/</span>
                                <div style={{ display: "flex", alignItems: "center", gap: 6, background: "#0F172A", padding: "3px 8px", borderRadius: 6, border: "1px solid #1E293B" }}>
                                  <span style={{ fontSize: 11, fontWeight: 600, color: "#F8FAFC" }}>{pool.tokenB}</span>
                                </div>
                              </div>
                            </td>
                            <td style={{ padding: "12px 20px" }}>
                              <span style={{
                                background: "rgba(16, 185, 129, 0.1)",
                                color: "#34D399",
                                border: "1px solid rgba(16, 185, 129, 0.2)",
                                padding: "3px 8px",
                                borderRadius: 4,
                                fontSize: 10,
                                fontFamily: "monospace",
                                fontWeight: 600
                              }}>
                                {(pool.feeBps / 10000).toFixed(4).replace(/\.?0+$/, '')}%
                              </span>
                            </td>
                          </tr>
                        );
                      })
                  )}
                </tbody>
              </table>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
