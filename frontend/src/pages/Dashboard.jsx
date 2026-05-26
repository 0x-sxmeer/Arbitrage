import { useState, useEffect, useRef, useCallback } from "react";
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
  LineChart,
  Line,
} from "recharts";

// ─────────────────────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────────────────────
const f = (n, d = 2) => (n == null ? "—" : Number(n).toFixed(d));
const fK = (n) => {
  if (n == null) return "—";
  n = Number(n);
  if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
  return String(Math.floor(n));
};
const ts = (ms) => {
  if (!ms) return "—";
  const d = new Date(Number(ms));
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(d.getSeconds()).padStart(2, "0")}`;
};

// ─────────────────────────────────────────────────────────────────────────────
//  Animated counter
// ─────────────────────────────────────────────────────────────────────────────
function useAnimNum(target, decimals = 2) {
  const [val, setVal] = useState(target);
  const cur = useRef(target);
  useEffect(() => {
    if (target === cur.current) return;
    const diff = target - cur.current,
      steps = 24;
    let i = 0;
    const id = setInterval(() => {
      i++;
      setVal(cur.current + diff * (i / steps));
      if (i >= steps) {
        clearInterval(id);
        cur.current = target;
        setVal(target);
      }
    }, 16);
    return () => clearInterval(id);
  }, [target]);
  return f(val, decimals);
}

// ─────────────────────────────────────────────────────────────────────────────
//  Metric card
// ─────────────────────────────────────────────────────────────────────────────
function MetricCard({
  label,
  value,
  prefix = "",
  suffix = "",
  decimals = 2,
  color1,
  color2,
  glowColor,
  icon,
  tag,
  tagColor,
  sub,
  children,
}) {
  const disp = useAnimNum(value || 0, decimals);
  return (
    <div className="metric-card">
      <div
        className="metric-card-glow"
        style={{ background: glowColor || color1 + "33" }}
      />
      <div className="metric-header">
        <div
          className="metric-icon"
          style={{ background: (color1 || "#fff") + "18" }}
        >
          {icon}
        </div>
        {tag && (
          <span
            className="metric-tag"
            style={{
              background: (tagColor || color1) + "18",
              color: tagColor || color1,
              border: `1px solid ${tagColor || color1}35`,
            }}
          >
            {tag}
          </span>
        )}
      </div>
      <div className="metric-label">{label}</div>
      <div
        className="metric-value"
        style={{
          "--val-color-1": color1 || "#fff",
          "--val-color-2": color2 || "#888888",
        }}
      >
        {prefix}
        {disp}
        {suffix}
      </div>
      {sub && <div className="metric-sub">{sub}</div>}
      {children}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  Status badge
// ─────────────────────────────────────────────────────────────────────────────
function StatusBadge({ status }) {
  const cls =
    {
      Executed: "s-executed",
      Simulated: "s-simulated",
      Detected: "s-detected",
      Unprofitable: "s-unprofitable",
    }[status] || "s-detected";
  return <span className={`status-badge ${cls}`}>{status?.toUpperCase()}</span>;
}

// ─────────────────────────────────────────────────────────────────────────────
//  Chain badge
// ─────────────────────────────────────────────────────────────────────────────
function ChainBadge({ chain }) {
  const cls =
    {
      Base: "chain-base",
      Optimism: "chain-optimism",
      Arbitrum: "chain-arbitrum",
    }[chain] || "chain-base";
  const short =
    { Base: "BASE", Optimism: "OP", Arbitrum: "ARB" }[chain] || chain;
  const dots = { Base: "#555555", Optimism: "#999999", Arbitrum: "#28a0f0" };
  return (
    <span className={`chain-badge ${cls}`}>
      <span
        style={{
          width: 5,
          height: 5,
          borderRadius: "50%",
          background: dots[chain],
          display: "inline-block",
        }}
      />
      {short}
    </span>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  Confidence bar
// ─────────────────────────────────────────────────────────────────────────────
function ConfBar({ value }) {
  return (
    <div className="conf-wrap">
      <div className="conf-track">
        <div className="conf-fill" style={{ width: `${value * 100}%` }} />
      </div>
      <div className="conf-label">{Math.round(value * 100)}%</div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  Live dot
// ─────────────────────────────────────────────────────────────────────────────
function LiveDot({ color = "var(--cyan)" }) {
  return (
    <span
      className="live-dot"
      style={{ background: color, boxShadow: `0 0 6px ${color}` }}
    />
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  Panel wrapper
// ─────────────────────────────────────────────────────────────────────────────
function Panel({
  phase,
  phaseColor,
  dot,
  title,
  sub,
  controls,
  children,
  style,
}) {
  const phaseColors = {
    1: "#ffffff",
    2: "#dddddd",
    3: "#999999",
    4: "#555555",
  };
  const pc = phaseColor || phaseColors[phase] || "#94a3b8";
  return (
    <div className="panel" style={style}>
      <div className="panel-header">
        <div className="panel-header-left">
          <div className="panel-title-row">
            {phase && (
              <span
                className="phase-tag"
                style={{
                  background: pc + "20",
                  color: pc,
                  border: `1px solid ${pc}35`,
                }}
              >
                PHASE {phase}
              </span>
            )}
            {dot && <LiveDot color={dot} />}
            <span className="panel-title">{title}</span>
          </div>
          {sub && <div className="panel-sub">{sub}</div>}
        </div>
        {controls && <div className="panel-controls">{controls}</div>}
      </div>
      {children}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  Custom chart tooltip
// ─────────────────────────────────────────────────────────────────────────────
function ChartTooltip({ active, payload, label }) {
  if (!active || !payload?.length) return null;
  return (
    <div
      style={{
        background: "rgba(0,0,0,0.96)",
        border: "1px solid rgba(255,255,255,0.08)",
        borderRadius: 8,
        padding: "8px 12px",
        fontSize: 11,
        fontFamily: "JetBrains Mono, monospace",
      }}
    >
      <div style={{ color: "#888888", marginBottom: 4 }}>{ts(label)}</div>
      {payload.map((p, i) => (
        <div key={i} style={{ color: p.color, fontWeight: 700 }}>
          {p.name}: ${f(p.value)}
        </div>
      ))}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  ── OVERVIEW PAGE ────────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
function OverviewPage({ metrics, profitHist, txs, logs, pools }) {
  const pp = metrics?.phase_profits || {};

  const chartData = profitHist
    .slice(-60)
    .map((p) => ({ t: p.t, profit: p.profit }));

  const termColors = {
    success: "#ffffff",
    error: "#999999",
    warn: "#bbbbbb",
    info: "#555555",
    exec: "#dddddd",
    cex: "#bbbbbb",
    liq: "#999999",
    cc: "#555555",
    pending: "#444444",
  };
  const typeColor = {
    SWAP: "#ffffff",
    ARB: "#999999",
    ADD_LIQ: "#bbbbbb",
    REMOVE_LIQ: "#888888",
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      {/* Hero metrics */}
      <div className="hero-row">
        <MetricCard
          label="SESSION PROFIT"
          value={metrics?.total_profit_usd || 0}
          prefix="$"
          color1="#ffffff"
          color2="#ffffff"
          icon="💰"
          tag="LIVE"
          tagColor="#ffffff"
          sub={
            <>
              <LiveDot color="#ffffff" /> Accumulating since engine start
            </>
          }
        />
        <MetricCard
          label="TXS MONITORED"
          value={metrics?.txs_decoded || 0}
          decimals={0}
          prefix=""
          color1="#555555"
          color2="#666666"
          icon="📡"
          tag={`BLK #${fK(metrics?.block_number || 0)}`}
          tagColor="#555555"
          sub="Base L2 mempool decoded"
        />
        <MetricCard
          label="CYCLES FOUND"
          value={metrics?.opportunities_found || 0}
          decimals={0}
          color1="#dddddd"
          color2="#cccccc"
          icon="🔄"
          tag="BELLMAN-FORD"
          tagColor="#dddddd"
          sub="Negative cycles detected"
        />
        <MetricCard
          label="POOL GRAPH"
          value={metrics?.graph_pools || 0}
          decimals={0}
          color1="#bbbbbb"
          color2="#aaaaaa"
          icon="🌊"
          tag="ACTIVE"
          tagColor="#bbbbbb"
          sub="Liquidity nodes indexed"
        />
      </div>

      {/* Profit chart + breakdown */}
      <div className="two-col">
        <Panel
          dot="var(--cyan)"
          title="SESSION P&L CURVE"
          sub="Cumulative profit across all strategy phases"
        >
          <div className="chart-panel">
            <ResponsiveContainer width="100%" height={180}>
              <AreaChart
                data={chartData}
                margin={{ top: 4, right: 8, left: 0, bottom: 0 }}
              >
                <defs>
                  <linearGradient id="pnlGrad" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="5%" stopColor="#ffffff" stopOpacity={0.25} />
                    <stop offset="95%" stopColor="#ffffff" stopOpacity={0} />
                  </linearGradient>
                </defs>
                <CartesianGrid
                  strokeDasharray="3 3"
                  stroke="rgba(255,255,255,0.04)"
                />
                <XAxis
                  dataKey="t"
                  tickFormatter={ts}
                  tick={{
                    fill: "#444444",
                    fontSize: 9,
                    fontFamily: "JetBrains Mono",
                  }}
                  axisLine={false}
                  tickLine={false}
                  minTickGap={60}
                />
                <YAxis
                  tickFormatter={(v) => `$${fK(v)}`}
                  tick={{
                    fill: "#444444",
                    fontSize: 9,
                    fontFamily: "JetBrains Mono",
                  }}
                  axisLine={false}
                  tickLine={false}
                  width={52}
                />
                <Tooltip content={<ChartTooltip />} />
                <Area
                  type="monotone"
                  dataKey="profit"
                  name="Profit"
                  stroke="#ffffff"
                  strokeWidth={2}
                  fill="url(#pnlGrad)"
                  dot={false}
                />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        </Panel>

        <Panel
          dot="var(--purple)"
          title="STRATEGY ALLOCATION"
          sub="Profit distribution by engine phase"
        >
          <div className="breakdown-grid">
            {[
              {
                label: "Phase 1 — DEX Arb",
                val: pp.dex_arb || 0,
                color: "#ffffff",
                pct: 25,
              },
              {
                label: "Phase 2 — CEX-DEX Spread",
                val: pp.cex_dex || 0,
                color: "#dddddd",
                pct: 45,
              },
              {
                label: "Phase 3 — Liquidations",
                val: pp.liquidations || 0,
                color: "#999999",
                pct: 20,
              },
              {
                label: "Phase 4 — Cross-Chain",
                val: pp.cross_chain || 0,
                color: "#555555",
                pct: 10,
              },
            ].map((p) => (
              <div className="breakdown-card" key={p.label}>
                <div className="breakdown-top">
                  <div className="breakdown-label">{p.label}</div>
                  <div className="breakdown-val" style={{ color: p.color }}>
                    ${fK(p.val)}
                  </div>
                </div>
                <div style={{ marginBottom: 6 }}>
                  <div className="bar-track">
                    <div
                      className="bar-fill"
                      style={{ width: `${p.pct}%`, background: p.color }}
                    />
                  </div>
                </div>
                <div className="breakdown-pct">
                  {p.pct}% of total session profit
                </div>
              </div>
            ))}
          </div>
        </Panel>
      </div>

      {/* Mempool + Terminal */}
      <div className="two-col">
        {/* Mempool stream */}
        <Panel
          dot="#ff007a"
          title="MEMPOOL STREAM"
          sub="Live pending transactions decoded from Base L2 node"
        >
          <div className="mempool-list">
            {txs.length === 0 ? (
              <div className="empty-state">
                <div className="empty-icon">📡</div>
                <div className="empty-title">Connecting to mempool...</div>
              </div>
            ) : (
              txs.map((tx, i) => (
                <div className="mempool-item" key={tx.id || i}>
                  <span
                    className="mempool-type"
                    style={{
                      background: (typeColor[tx.type] || "#888888") + "18",
                      color: typeColor[tx.type] || "#888888",
                      border: `1px solid ${typeColor[tx.type] || "#888888"}35`,
                    }}
                  >
                    {tx.type}
                  </span>
                  <span className="mempool-hash">{tx.hash}</span>
                  <span className="mempool-dex">{tx.dex}</span>
                  <span className="mempool-pair">{tx.token}</span>
                  <span className="mempool-gas">{tx.gasGwei}gw</span>
                </div>
              ))
            )}
          </div>
        </Panel>

        {/* Hacker terminal */}
        <Panel
          dot="#ff007a"
          title="ENGINE TELEMETRY"
          sub="Orchestration logs — all 4 phases"
        >
          <div className="terminal-wrap">
            {logs.slice(-80).map((l, i) => (
              <div
                key={i}
                className="term-line"
                style={{ color: termColors[l.type] || "#888888" }}
              >
                {l.text}
              </div>
            ))}
            <span className="term-cursor" />
          </div>
        </Panel>
      </div>

      {/* Pool Registry */}
      <Panel
        dot="#ffffff"
        title="ACTIVE POOL REGISTRY"
        sub="DEX pools actively tracked in the Bellman-Ford graph"
      >
        <div className="table-scroll" style={{ maxHeight: 300 }}>
          <table className="pro-table">
            <thead>
              <tr>
                {["CHAIN", "DEX", "TOKEN A", "TOKEN B", "FEE (BPS)", "ADDRESS"].map((h) => (
                  <th key={h}>{h}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {(!pools || pools.length === 0) ? (
                <tr>
                  <td colSpan={6}>
                    <div className="empty-state">
                      <div className="empty-icon">🌊</div>
                      <div className="empty-title">Syncing pools...</div>
                      <div className="empty-sub">Loading pools from Postgres and warming up EVM states</div>
                    </div>
                  </td>
                </tr>
              ) : (
                pools.slice(0, 50).map((p, i) => (
                  <tr key={p.id || i}>
                    <td><span className="mempool-type" style={{background: "#55555518", color: "#dddddd", border: "1px solid #55555535"}}>{p.chain}</span></td>
                    <td className="td-mono">{p.dex}</td>
                    <td className="td-mono" style={{ color: "#ffffff" }}>{p.tokenA}</td>
                    <td className="td-mono" style={{ color: "#dddddd" }}>{p.tokenB}</td>
                    <td className="td-mono" style={{ color: "#888888" }}>{p.feeBps} bps</td>
                    <td className="td-mono" style={{ color: "#555555" }}>{p.id}</td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </Panel>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  ── PHASE 1: DEX ARB ─────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
function Phase1Page({ opps, profitHist }) {
  const [search, setSearch] = useState("");
  const filtered = opps.filter(
    (o) =>
      !search || JSON.stringify(o).toLowerCase().includes(search.toLowerCase()),
  );

  const chartData = profitHist
    .slice(-40)
    .map((p) => ({ t: p.t, profit: (p.profit || 0) * 0.25 }));

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <div className="hero-row">
        <MetricCard
          label="DEX ARB PROFIT"
          value={opps
            .filter((o) => o.nevUsd > 0)
            .reduce((s, o) => s + (o.nevUsd || 0), 0)}
          prefix="$"
          color1="#ffffff"
          color2="#ffffff"
          icon="🔄"
          tag="FLASH LOANS"
          tagColor="#ffffff"
        />
        <MetricCard
          label="PROFITABLE CYCLES"
          value={opps.filter((o) => o.nevUsd > 0).length}
          decimals={0}
          color1="#ffffff"
          color2="#eeeeee"
          icon="✅"
        />
        <MetricCard
          label="TOTAL ROUTES SCANNED"
          value={opps.length}
          decimals={0}
          color1="#dddddd"
          icon="🔍"
        />
        <MetricCard
          label="AVG PROFIT/CYCLE"
          value={
            opps.filter((o) => o.nevUsd > 0).length
              ? opps
                  .filter((o) => o.nevUsd > 0)
                  .reduce((s, o) => s + (o.nevUsd || 0), 0) /
                opps.filter((o) => o.nevUsd > 0).length
              : 0
          }
          prefix="$"
          color1="#bbbbbb"
          icon="📊"
        />
      </div>

      <div className="two-col">
        <Panel
          dot="#ffffff"
          title="DEX ARB P&L TREND"
          sub="Bellman-Ford cycle detection over time"
        >
          <div className="chart-panel">
            <ResponsiveContainer width="100%" height={160}>
              <AreaChart
                data={chartData}
                margin={{ top: 4, right: 8, left: 0, bottom: 0 }}
              >
                <defs>
                  <linearGradient id="dexGrad" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="5%" stopColor="#ffffff" stopOpacity={0.3} />
                    <stop offset="95%" stopColor="#ffffff" stopOpacity={0} />
                  </linearGradient>
                </defs>
                <CartesianGrid
                  strokeDasharray="3 3"
                  stroke="rgba(255,255,255,0.04)"
                />
                <XAxis
                  dataKey="t"
                  tickFormatter={ts}
                  tick={{
                    fill: "#444444",
                    fontSize: 9,
                    fontFamily: "JetBrains Mono",
                  }}
                  axisLine={false}
                  tickLine={false}
                />
                <YAxis
                  tickFormatter={(v) => `$${fK(v)}`}
                  tick={{
                    fill: "#444444",
                    fontSize: 9,
                    fontFamily: "JetBrains Mono",
                  }}
                  axisLine={false}
                  tickLine={false}
                  width={52}
                />
                <Tooltip content={<ChartTooltip />} />
                <Area
                  type="monotone"
                  dataKey="profit"
                  name="DEX P&L"
                  stroke="#ffffff"
                  strokeWidth={2}
                  fill="url(#dexGrad)"
                  dot={false}
                />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        </Panel>
        <Panel
          dot="#dddddd"
          title="CYCLE STATUS DISTRIBUTION"
          sub="Breakdown of detected route outcomes"
        >
          <div
            style={{
              padding: "20px 24px",
              display: "flex",
              flexDirection: "column",
              gap: 14,
            }}
          >
            {[
              {
                label: "Executed",
                val: opps.filter((o) => o.status === "Executed").length,
                color: "#ffffff",
              },
              {
                label: "Simulated",
                val: opps.filter((o) => o.status === "Simulated").length,
                color: "#dddddd",
              },
              {
                label: "Unprofitable",
                val: opps.filter((o) => o.status === "Unprofitable").length,
                color: "#999999",
              },
            ].map((s) => {
              const total = opps.length || 1;
              return (
                <div key={s.label}>
                  <div
                    style={{
                      display: "flex",
                      justifyContent: "space-between",
                      marginBottom: 5,
                    }}
                  >
                    <span style={{ fontSize: 11, color: "#888888" }}>
                      {s.label}
                    </span>
                    <span
                      style={{
                        fontFamily: "JetBrains Mono",
                        fontSize: 11,
                        color: s.color,
                        fontWeight: 700,
                      }}
                    >
                      {s.val}{" "}
                      <span style={{ color: "#444444" }}>
                        ({Math.round((s.val / total) * 100)}%)
                      </span>
                    </span>
                  </div>
                  <div className="bar-track">
                    <div
                      className="bar-fill"
                      style={{
                        width: `${(s.val / total) * 100}%`,
                        background: s.color,
                      }}
                    />
                  </div>
                </div>
              );
            })}
          </div>
        </Panel>
      </div>

      <Panel
        phase={1}
        dot="#ffffff"
        title="BELLMAN-FORD ARBITRAGE ROUTES"
        sub="Flash-loan powered cyclic arbitrage — Uniswap V3, Aerodrome, Curve, Balancer"
        controls={
          <div className="search-box">
            <span style={{ fontSize: 12, color: "var(--t3)" }}>⌕</span>
            <input
              placeholder="Search route..."
              value={search}
              onChange={(e) => setSearch(e.target.value)}
            />
          </div>
        }
      >
        <div className="table-scroll">
          <table className="pro-table">
            <thead>
              <tr>
                {[
                  "TIME",
                  "ROUTE / PATH",
                  "BLOCK",
                  "INPUT",
                  "OUTPUT",
                  "GAS",
                  "NET PROFIT",
                  "STATUS",
                ].map((h) => (
                  <th key={h}>{h}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {filtered.length === 0 ? (
                <tr>
                  <td colSpan={8}>
                    <div className="empty-state">
                      <div className="empty-icon">🔍</div>
                      <div className="empty-title">
                        Scanning for negative cycles...
                      </div>
                      <div className="empty-sub">
                        Bellman-Ford is analyzing the mempool-updated reserve
                        graph
                      </div>
                    </div>
                  </td>
                </tr>
              ) : (
                filtered.slice(0, 40).map((o, i) => (
                  <tr
                    key={o.id || i}
                    className={`${o.isExecutable ? "highlight-row" : ""}`}
                  >
                    <td className="td-mono td-dim">{ts(o.ts)}</td>
                    <td style={{ maxWidth: 280 }}>
                      <div
                        style={{
                          fontSize: 10,
                          fontFamily: "JetBrains Mono",
                          color: "#94a3b8",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                          maxWidth: 280,
                        }}
                      >
                        {Array.isArray(o.route)
                          ? o.route
                              .map(
                                (r) =>
                                  `${r.dex || r.pool}(${r.tokenOut || r.token_out || ""})`,
                              )
                              .join(" → ")
                          : String(o.route)}
                      </div>
                    </td>
                    <td className="td-mono">
                      <a
                        href={`https://basescan.org/block/${o.block}`}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="ext-link"
                      >
                        {o.block}
                      </a>
                    </td>
                    <td className="td-mono td-dim">{o.input}</td>
                    <td className="td-mono">{o.output}</td>
                    <td className="td-mono" style={{ color: "#999999" }}>
                      ${f(o.gasUsd)}
                    </td>
                    <td
                      className="td-mono td-em"
                      style={{ color: o.nevUsd > 0 ? "#ffffff" : "#999999" }}
                    >
                      {o.nevUsd > 0 ? "+" : ""}${f(o.nevUsd)}
                    </td>
                    <td>
                      <StatusBadge status={o.status} />
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </Panel>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  ── PHASE 2: CEX-DEX ─────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
function Phase2Page({ data }) {
  const opps = data?.opportunities || [];
  const prices = data?.prices || {};

  const chartData = opps
    .slice(0, 30)
    .reverse()
    .map((o) => ({
      t: o.ts,
      profit: o.expectedProfitUsd || 0,
      spread: o.spreadPct || 0,
    }));

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <div className="hero-row">
        <MetricCard
          label="CEX-DEX PROFIT"
          value={opps
            .filter((o) => o.status === "Executed")
            .reduce((s, o) => s + (o.expectedProfitUsd || 0), 0)}
          prefix="$"
          color1="#dddddd"
          color2="#cccccc"
          icon="📊"
          tag="KELLY SIZED"
          tagColor="#dddddd"
        />
        <MetricCard
          label="SIGNALS FIRED"
          value={opps.length}
          decimals={0}
          color1="#bbbbbb"
          icon="📡"
        />
        <MetricCard
          label="AVG SPREAD"
          value={
            opps.length
              ? opps.reduce((s, o) => s + (o.spreadPct || 0), 0) / opps.length
              : 0
          }
          suffix="%"
          decimals={3}
          color1="#ffffff"
          icon="📐"
        />
        <MetricCard
          label="TRADE SIZE"
          value={500000}
          prefix="$"
          decimals={0}
          color1="#555555"
          icon="💼"
          tag="PER TRADE"
          tagColor="#555555"
        />
      </div>

      {/* Live tickers */}
      <Panel
        phase={2}
        dot="#dddddd"
        title="BINANCE vs DEX PRICE MATRIX"
        sub="Real-time mark price vs on-chain spot — spread ≥ 0.15% triggers execution"
      >
        <div className="price-ticker-row">
          {Object.entries(prices).map(([sym, p]) => {
            const sp = parseFloat(p.spread);
            const isHot = sp > 0.15;
            return (
              <div key={sym} className={`price-ticker ${isHot ? "hot" : ""}`}>
                <div className="ticker-sym">{sym}</div>
                <div className="ticker-grid">
                  <div>
                    <div className="ticker-item-label">BINANCE CEX</div>
                    <div
                      className="ticker-item-val"
                      style={{ color: "#bbbbbb" }}
                    >
                      ${fK(p.binance)}
                    </div>
                  </div>
                  <div className="ticker-spread">
                    <div className="ticker-item-label">SPREAD</div>
                    <div
                      className="spread-pct"
                      style={{ color: isHot ? "#bbbbbb" : "#888888" }}
                    >
                      {p.spread}%
                    </div>
                    {isHot && (
                      <div
                        style={{ fontSize: 8, color: "#bbbbbb", marginTop: 2 }}
                      >
                        ⚡ HOT
                      </div>
                    )}
                  </div>
                  <div style={{ textAlign: "right" }}>
                    <div className="ticker-item-label">DEX SPOT</div>
                    <div
                      className="ticker-item-val"
                      style={{ color: "#ffffff" }}
                    >
                      ${fK(p.dex)}
                    </div>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      </Panel>

      {/* Spread chart */}
      {chartData.length > 1 && (
        <Panel
          dot="#dddddd"
          title="SPREAD OVER TIME"
          sub="CEX-DEX spread % and estimated profit per signal"
        >
          <div className="chart-panel">
            <ResponsiveContainer width="100%" height={160}>
              <LineChart
                data={chartData}
                margin={{ top: 4, right: 8, left: 0, bottom: 0 }}
              >
                <CartesianGrid
                  strokeDasharray="3 3"
                  stroke="rgba(255,255,255,0.04)"
                />
                <XAxis
                  dataKey="t"
                  tickFormatter={ts}
                  tick={{
                    fill: "#444444",
                    fontSize: 9,
                    fontFamily: "JetBrains Mono",
                  }}
                  axisLine={false}
                  tickLine={false}
                />
                <YAxis
                  yAxisId="left"
                  tickFormatter={(v) => `${v.toFixed(2)}%`}
                  tick={{ fill: "#444444", fontSize: 9 }}
                  axisLine={false}
                  tickLine={false}
                  width={48}
                />
                <YAxis
                  yAxisId="right"
                  orientation="right"
                  tickFormatter={(v) => `$${fK(v)}`}
                  tick={{ fill: "#444444", fontSize: 9 }}
                  axisLine={false}
                  tickLine={false}
                  width={52}
                />
                <Tooltip content={<ChartTooltip />} />
                <Line
                  yAxisId="left"
                  type="monotone"
                  dataKey="spread"
                  name="Spread"
                  stroke="#dddddd"
                  strokeWidth={2}
                  dot={false}
                />
                <Line
                  yAxisId="right"
                  type="monotone"
                  dataKey="profit"
                  name="Profit"
                  stroke="#ffffff"
                  strokeWidth={2}
                  dot={false}
                />
              </LineChart>
            </ResponsiveContainer>
          </div>
        </Panel>
      )}

      <Panel
        phase={2}
        dot="#dddddd"
        title="CEX-DEX OPPORTUNITY LOG"
        sub="Statistical arb signals — Binance WebSocket feed vs DEX spot pricing"
      >
        <div className="table-scroll">
          <table className="pro-table">
            <thead>
              <tr>
                {[
                  "TIME",
                  "SYMBOL",
                  "DIRECTION",
                  "BINANCE",
                  "DEX",
                  "SPREAD",
                  "SIZE",
                  "PROFIT EST",
                  "CONFIDENCE",
                  "STATUS",
                ].map((h) => (
                  <th key={h}>{h}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {opps.length === 0 ? (
                <tr>
                  <td colSpan={10}>
                    <div className="empty-state">
                      <div className="empty-icon">📡</div>
                      <div className="empty-title">
                        Monitoring Binance WebSocket feed...
                      </div>
                    </div>
                  </td>
                </tr>
              ) : (
                opps.slice(0, 30).map((o, i) => (
                  <tr key={o.id || i}>
                    <td className="td-mono td-dim">{ts(o.ts)}</td>
                    <td style={{ fontWeight: 800, color: "#dddddd" }}>
                      {o.symbol}
                    </td>
                    <td>
                      <span
                        style={{
                          fontSize: 10,
                          fontWeight: 700,
                          color:
                            o.direction === "BuyDexSellCex"
                              ? "#ffffff"
                              : "#999999",
                        }}
                      >
                        {o.direction === "BuyDexSellCex"
                          ? "↑ BUY DEX"
                          : "↓ SELL DEX"}
                      </span>
                    </td>
                    <td className="td-mono" style={{ color: "#bbbbbb" }}>
                      ${f(o.cexPrice, 2)}
                    </td>
                    <td className="td-mono" style={{ color: "#ffffff" }}>
                      ${f(o.dexPrice, 2)}
                    </td>
                    <td
                      className="td-mono td-em"
                      style={{
                        color: o.spreadPct > 0.15 ? "#bbbbbb" : "#888888",
                      }}
                    >
                      {f(o.spreadPct, 3)}%
                    </td>
                    <td className="td-mono td-dim">${fK(o.sizeUsd)}</td>
                    <td className="td-mono td-em" style={{ color: "#ffffff" }}>
                      +${f(o.expectedProfitUsd)}
                    </td>
                    <td>
                      <ConfBar value={o.confidence || 0} />
                    </td>
                    <td>
                      <StatusBadge status={o.status} />
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </Panel>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  ── PHASE 3: LIQUIDATIONS ────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
function Phase3Page({ liquidations }) {
  const totalBonus = liquidations.reduce((s, l) => s + (l.bonusUsd || 0), 0);
  const aave = liquidations.filter((l) => l.protocol === "AaveV3");
  const moon = liquidations.filter((l) => l.protocol === "Moonwell");

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <div className="hero-row">
        <MetricCard
          label="LIQUIDATION PROFIT"
          value={totalBonus}
          prefix="$"
          color1="#999999"
          color2="#999999"
          icon="🏥"
          tag="FLASH LOANS"
          tagColor="#999999"
        />
        <MetricCard
          label="LIQUIDATIONS EXECUTED"
          value={liquidations.filter((l) => l.status === "Executed").length}
          decimals={0}
          color1="#ffffff"
          icon="✅"
        />
        <MetricCard
          label="AAVE V3"
          value={aave.length}
          decimals={0}
          color1="#dddddd"
          icon="Ⓐ"
        />
        <MetricCard
          label="MOONWELL"
          value={moon.length}
          decimals={0}
          color1="#555555"
          icon="🌙"
        />
      </div>

      <Panel
        phase={3}
        dot="#999999"
        title="LIQUIDATION MONITOR — AAVE V3 + MOONWELL"
        sub="Health factor scanner — executes liquidations for 5–8% bonus via Balancer flash loans"
      >
        <div className="table-scroll">
          <table className="pro-table">
            <thead>
              <tr>
                {[
                  "TIME",
                  "BORROWER",
                  "PROTOCOL",
                  "HEALTH FACTOR",
                  "DEBT USD",
                  "BONUS",
                  "NET PROFIT",
                  "STATUS",
                ].map((h) => (
                  <th key={h}>{h}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {liquidations.length === 0 ? (
                <tr>
                  <td colSpan={8}>
                    <div className="empty-state">
                      <div className="empty-icon">🏥</div>
                      <div className="empty-title">
                        Scanning health factors...
                      </div>
                      <div className="empty-sub">
                        Monitoring Aave V3 and Moonwell for undercollateralized
                        positions
                      </div>
                    </div>
                  </td>
                </tr>
              ) : (
                liquidations.map((l, i) => (
                  <tr key={l.id || i}>
                    <td className="td-mono td-dim">{ts(l.ts)}</td>
                    <td
                      className="td-mono"
                      style={{ fontSize: 10, color: "#888888" }}
                    >
                      {l.borrower}
                    </td>
                    <td>
                      <span
                        style={{
                          fontSize: 10,
                          fontWeight: 800,
                          padding: "3px 9px",
                          borderRadius: 6,
                          background:
                            l.protocol === "AaveV3"
                              ? "rgba(180,90,242,0.15)"
                              : "rgba(77,166,255,0.15)",
                          color:
                            l.protocol === "AaveV3" ? "#dddddd" : "#555555",
                          border: `1px solid ${l.protocol === "AaveV3" ? "#dddddd40" : "#55555540"}`,
                        }}
                      >
                        {l.protocol}
                      </span>
                    </td>
                    <td>
                      <span
                        className="td-mono td-em"
                        style={{
                          color:
                            l.healthFactor < 0.95
                              ? "#999999"
                              : l.healthFactor < 1.0
                                ? "#bbbbbb"
                                : "#ffffff",
                        }}
                      >
                        {f(l.healthFactor, 4)}
                      </span>
                    </td>
                    <td className="td-mono td-dim">${fK(l.debtUsd)}</td>
                    <td className="td-mono td-em" style={{ color: "#bbbbbb" }}>
                      +${f(l.bonusUsd)}
                    </td>
                    <td className="td-mono td-em" style={{ color: "#ffffff" }}>
                      +${f(l.netProfitUsd)}
                    </td>
                    <td>
                      <StatusBadge status={l.status} />
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </Panel>

      <div
        className="info-callout"
        style={{
          borderColor: "rgba(255,77,109,0.2)",
          background: "rgba(255,77,109,0.04)",
        }}
      >
        <div className="info-callout-title" style={{ color: "#999999" }}>
          ⚡ BACKRUN PIPELINE — INTEGRATED INTO MEMPOOL WORKER
        </div>
        <div className="info-callout-body">
          Backrunning is active inside the mempool worker pipeline. Every swap
          decoded from the pending pool is evaluated for price impact.
          Qualifying swaps (≥20bps impact, ≥$10 profit) trigger Flashbots
          MEV-Share bundles submitted directly after the victim transaction via
          the BundleBuilder module. Bloxroute private feed for reduced latency.
        </div>
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  ── PHASE 4: CROSS-CHAIN ─────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
function Phase4Page({ data }) {
  const opps = data?.opportunities || [];
  const prices = data?.prices || {};
  const chains = ["Base", "Optimism", "Arbitrum"];
  const chainColors = {
    Base: "#555555",
    Optimism: "#999999",
    Arbitrum: "#28a0f0",
  };
  const chainBorders = {
    Base: "rgba(77,166,255,0.25)",
    Optimism: "rgba(255,77,109,0.25)",
    Arbitrum: "rgba(40,160,240,0.25)",
  };

  const chartData = opps
    .slice(0, 30)
    .reverse()
    .map((o) => ({
      t: o.ts,
      profit: o.expectedProfitUsd || 0,
      spread: o.spreadPct || 0,
    }));

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <div className="hero-row">
        <MetricCard
          label="CROSS-CHAIN PROFIT"
          value={opps
            .filter((o) => o.status === "Executed")
            .reduce((s, o) => s + (o.expectedProfitUsd || 0), 0)}
          prefix="$"
          color1="#555555"
          color2="#666666"
          icon="🌐"
          tag="3 CHAINS"
          tagColor="#555555"
        />
        <MetricCard
          label="OPPORTUNITIES"
          value={opps.length}
          decimals={0}
          color1="#dddddd"
          icon="🔭"
        />
        <MetricCard
          label="TRADE SIZE"
          value={50000}
          prefix="$"
          decimals={0}
          color1="#ffffff"
          icon="💼"
        />
        <MetricCard
          label="AVG SPREAD"
          value={
            opps.length
              ? opps.reduce((s, o) => s + (o.spreadPct || 0), 0) / opps.length
              : 0
          }
          suffix="%"
          decimals={3}
          color1="#bbbbbb"
          icon="📐"
        />
      </div>

      {/* Chain price matrix */}
      <Panel
        phase={4}
        dot="#555555"
        title="LIVE PRICE MATRIX — BASE / OPTIMISM / ARBITRUM"
        sub="Cross-chain spot prices updated every 500ms — divergence ≥ 0.20% triggers atomic execution"
      >
        <div className="chain-matrix-row">
          {chains.map((chain) => (
            <div
              key={chain}
              className="chain-card"
              style={{
                borderColor: chainBorders[chain],
                background: `${chainColors[chain]}08`,
              }}
            >
              <div className="chain-card-header">
                <div
                  className="chain-dot"
                  style={{
                    background: chainColors[chain],
                    boxShadow: `0 0 8px ${chainColors[chain]}`,
                  }}
                />
                <div
                  className="chain-name"
                  style={{ color: chainColors[chain] }}
                >
                  {chain}
                </div>
              </div>
              {prices[chain] &&
                Object.entries(prices[chain]).map(([tok, price]) => (
                  <div key={tok} className="chain-price-row">
                    <div className="chain-token">{tok}</div>
                    <div className="chain-price">
                      ${parseFloat(price).toFixed(2)}
                    </div>
                  </div>
                ))}
            </div>
          ))}
        </div>
      </Panel>

      {chartData.length > 1 && (
        <Panel
          dot="#555555"
          title="DIVERGENCE SCAN"
          sub="Cross-chain spread and profit estimate over time"
        >
          <div className="chart-panel">
            <ResponsiveContainer width="100%" height={150}>
              <AreaChart
                data={chartData}
                margin={{ top: 4, right: 8, left: 0, bottom: 0 }}
              >
                <defs>
                  <linearGradient id="ccGrad" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="5%" stopColor="#555555" stopOpacity={0.3} />
                    <stop offset="95%" stopColor="#555555" stopOpacity={0} />
                  </linearGradient>
                </defs>
                <CartesianGrid
                  strokeDasharray="3 3"
                  stroke="rgba(255,255,255,0.04)"
                />
                <XAxis
                  dataKey="t"
                  tickFormatter={ts}
                  tick={{ fill: "#444444", fontSize: 9 }}
                  axisLine={false}
                  tickLine={false}
                />
                <YAxis
                  tickFormatter={(v) => `$${fK(v)}`}
                  tick={{ fill: "#444444", fontSize: 9 }}
                  axisLine={false}
                  tickLine={false}
                  width={52}
                />
                <Tooltip content={<ChartTooltip />} />
                <Area
                  type="monotone"
                  dataKey="profit"
                  name="Cross-Chain Profit"
                  stroke="#555555"
                  strokeWidth={2}
                  fill="url(#ccGrad)"
                  dot={false}
                />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        </Panel>
      )}

      <Panel
        phase={4}
        dot="#555555"
        title="CROSS-CHAIN ARBITRAGE LOG"
        sub="Price divergence opportunities across Base ↔ Optimism ↔ Arbitrum"
      >
        <div className="table-scroll">
          <table className="pro-table">
            <thead>
              <tr>
                {[
                  "TIME",
                  "TOKEN",
                  "BUY ON",
                  "SELL ON",
                  "BUY PRICE",
                  "SELL PRICE",
                  "SPREAD",
                  "TRADE SIZE",
                  "PROFIT EST",
                  "STATUS",
                ].map((h) => (
                  <th key={h}>{h}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {opps.length === 0 ? (
                <tr>
                  <td colSpan={10}>
                    <div className="empty-state">
                      <div className="empty-icon">🌐</div>
                      <div className="empty-title">
                        Monitoring 3-chain price matrix...
                      </div>
                    </div>
                  </td>
                </tr>
              ) : (
                opps.slice(0, 25).map((o, i) => (
                  <tr key={o.id || i}>
                    <td className="td-mono td-dim">{ts(o.ts)}</td>
                    <td style={{ fontWeight: 800, color: "#666666" }}>
                      {o.token}
                    </td>
                    <td>
                      <ChainBadge chain={o.buyChain} />
                    </td>
                    <td>
                      <ChainBadge chain={o.sellChain} />
                    </td>
                    <td className="td-mono" style={{ color: "#ffffff" }}>
                      ${f(o.buyPrice)}
                    </td>
                    <td className="td-mono" style={{ color: "#dddddd" }}>
                      ${f(o.sellPrice)}
                    </td>
                    <td
                      className="td-mono td-em"
                      style={{
                        color: o.spreadPct > 0.3 ? "#bbbbbb" : "#888888",
                      }}
                    >
                      {f(o.spreadPct, 3)}%
                    </td>
                    <td className="td-mono td-dim">${fK(o.tradeSizeUsd)}</td>
                    <td className="td-mono td-em" style={{ color: "#ffffff" }}>
                      +${f(o.expectedProfitUsd)}
                    </td>
                    <td>
                      <StatusBadge status={o.status} />
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </Panel>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
//  ── NAV CONFIG ───────────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
const NAV = [
  { id: 0, icon: "⚡", label: "Overview", badge: "LIVE" },
  { id: 1, icon: "🔄", label: "Phase 1 — DEX Arb", color: "#ffffff" },
  { id: 2, icon: "📊", label: "Phase 2 — CEX-DEX", color: "#dddddd" },
  { id: 3, icon: "🏥", label: "Phase 3 — Liquidations", color: "#999999" },
  { id: 4, icon: "🌐", label: "Phase 4 — Cross-Chain", color: "#555555" },
];

const PHASE_TITLES = [
  "OVERVIEW",
  "DEX ARBITRAGE",
  "CEX-DEX SPREAD",
  "LIQUIDATIONS",
  "CROSS-CHAIN",
];

// ─────────────────────────────────────────────────────────────────────────────
//  ── ROOT ─────────────────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
export default function Dashboard() {
  const [page, setPage] = useState(0);
  const [metrics, setMetrics] = useState(null);
  const [opps, setOpps] = useState([]);
  const [cexDex, setCexDex] = useState(null);
  const [liqs, setLiqs] = useState([]);
  const [crossChain, setCC] = useState(null);
  const [profitHist, setPH] = useState([]);
  const [txs, setTxs] = useState([]);
  const [pools, setPools] = useState([]);
  const [logs, setLogs] = useState([]);
  const [online, setOnline] = useState(false);
  const seenIds = useRef(new Set());

  const addLog = useCallback((type, text) => {
    setLogs((p) => {
      const n = [...p, { type, text: `[${ts(Date.now())}] ${text}` }];
      return n.length > 300 ? n.slice(-300) : n;
    });
  }, []);

  useEffect(() => {
    const run = async () => {
      try {
        const [mR, oR, cR, lR, ccR, phR, txR, pR] = await Promise.all([
          fetch("/api/metrics"),
          fetch("/api/opportunities"),
          fetch("/api/cex-dex"),
          fetch("/api/liquidations"),
          fetch("/api/cross-chain"),
          fetch("/api/profit-history"),
          fetch("/api/mempool"),
          fetch("/api/pools"),
        ]);
        if (!mR.ok) {
          setOnline(false);
          return;
        }
        const [m, o, c, l, cc, ph, tx, poolsData] = await Promise.all([
          mR.json(),
          oR.json(),
          cR.json(),
          lR.json(),
          ccR.json(),
          phR.json(),
          txR.json(),
          pR.json(),
        ]);
        setMetrics(m);
        setOpps(o);
        setCexDex(c);
        setLiqs(l);
        setCC(cc);
        setPH(ph);
        setTxs(tx);
        setPools(poolsData);
        setOnline(true);

        // Log new opportunities
        (Array.isArray(o) ? o : []).forEach((opp) => {
          if (!seenIds.current.has(opp.id)) {
            seenIds.current.add(opp.id);
            if (opp.nevUsd > 0) {
              addLog(
                "exec",
                `BELLMAN-FORD: cycle → ${Array.isArray(opp.route) ? opp.route.map((r) => r.tokenOut || r.token_out || "?").join(" → ") : opp.route}`,
              );
              addLog(
                "success",
                `EXECUTOR: bundle fired | profit=$${f(opp.nevUsd)} | gas=$${f(opp.gasUsd)}`,
              );
            }
          }
        });
        (Array.isArray(l) ? l : []).slice(0, 1).forEach((liq) => {
          const lid = "L" + liq.id;
          if (!seenIds.current.has(lid)) {
            seenIds.current.add(lid);
            addLog(
              "liq",
              `LIQUIDATION: ${liq.borrower} | hf=${f(liq.healthFactor, 4)} | bonus=$${f(liq.bonusUsd)}`,
            );
          }
        });
        (c?.opportunities || []).slice(0, 1).forEach((cx) => {
          const cid = "C" + cx.id;
          if (!seenIds.current.has(cid)) {
            seenIds.current.add(cid);
            addLog(
              "cex",
              `CEX-DEX: ${cx.symbol} spread=${f(cx.spreadPct, 3)}% | profit est=$${f(cx.expectedProfitUsd)}`,
            );
          }
        });
      } catch {
        setOnline(false);
      }
    };
    run();
    const iv = setInterval(run, 1200);
    return () => clearInterval(iv);
  }, [addLog]);

  useEffect(() => {
    if (!online) return;
    const iv = setInterval(() => {
      const m = metrics || {};
      addLog(
        "pending",
        `ORCHESTRATOR: ${m.graph_pools || 0} pools | ${m.txs_decoded || 0} txs | block #${m.block_number || 0} | uptime ${m.uptime_secs || 0}s`,
      );
    }, 3500);
    return () => clearInterval(iv);
  }, [online, metrics, addLog]);

  const phases = metrics?.phases || {};

  return (
    <>
      {/* Animated background */}
      <div className="bg-canvas">
        <div className="bg-grid" />
        <div className="bg-orb bg-orb-1" />
        <div className="bg-orb bg-orb-2" />
        <div className="bg-orb bg-orb-3" />
        <div className="bg-noise" />
      </div>

      <div className="shell">
        {/* Sidebar */}
        <aside className="sidebar">
          <div className="sidebar-logo">
            <div className="logo-mark">
              <div className="logo-icon">⚡</div>
              <div className="logo-text">
                <div className="logo-name">MEV ENGINE</div>
                <div className="logo-sub">V2 · MULTI-STRATEGY</div>
              </div>
            </div>
          </div>

          {/* Engine status */}
          <div className={`engine-status ${online ? "live" : "dead"}`}>
            <div className="engine-dot" />
            <span>{online ? "ENGINE LIVE" : "ENGINE OFFLINE"}</span>
          </div>

          {/* Nav */}
          <nav className="sidebar-nav">
            <div className="nav-section-label">Navigation</div>
            {NAV.map((n) => (
              <div
                key={n.id}
                className={`nav-item ${page === n.id ? "active" : ""}`}
                style={{ "--nav-color": n.color || "var(--cyan)" }}
                onClick={() => setPage(n.id)}
              >
                <div className="nav-icon-wrap">{n.icon}</div>
                <span className="nav-label">{n.label}</span>
                {n.badge && page === n.id && (
                  <span className="nav-badge">{n.badge}</span>
                )}
              </div>
            ))}

            <div className="nav-section-label" style={{ marginTop: 8 }}>
              Strategies
            </div>
          </nav>

          {/* Phase status */}
          <div className="sidebar-phases">
            {[
              { label: "DEX Arbitrage", active: phases.phase1_active },
              { label: "CEX-DEX Spread", active: phases.phase2_active },
              { label: "Liquidations", active: phases.phase3_active },
              { label: "Cross-Chain", active: phases.phase4_active },
            ].map((p) => (
              <div key={p.label} className="phase-row">
                <span className="phase-row-label">{p.label}</span>
                <span className={`phase-chip ${p.active ? "on" : "off"}`}>
                  {p.active ? "LIVE" : "OFF"}
                </span>
              </div>
            ))}
          </div>

          {/* Footer */}
          <div className="sidebar-footer">
            <div className="block-counter">
              <span className="block-label">BLOCK</span>
              <span className="block-value">
                #{fK(metrics?.block_number || 0)}
              </span>
            </div>
          </div>
        </aside>

        {/* Main */}
        <main className="main">
          {/* Top bar */}
          <div className="topbar">
            <div className="topbar-title">{PHASE_TITLES[page]}</div>
            <div className="topbar-right">
              <div className="topbar-stat">
                <span className="topbar-stat-label">PROFIT</span>
                <span
                  className="topbar-stat-val"
                  style={{ color: "var(--cyan)" }}
                >
                  ${fK(metrics?.total_profit_usd || 0)}
                </span>
              </div>
              <div className="topbar-stat">
                <span className="topbar-stat-label">TXS</span>
                <span
                  className="topbar-stat-val"
                  style={{ color: "var(--blue)" }}
                >
                  {fK(metrics?.txs_decoded || 0)}
                </span>
              </div>
              <div className="execute-badge">
                <LiveDot color="var(--cyan)" />
                EXECUTE: {metrics?.execute_enabled ? "ON" : "SIM"}
              </div>
            </div>
          </div>

          <div className="page-content">
            {page === 0 && (
              <OverviewPage
                metrics={metrics}
                profitHist={profitHist}
                opps={opps}
                txs={txs}
                logs={logs}
                pools={pools}
              />
            )}
            {page === 1 && <Phase1Page opps={opps} profitHist={profitHist} />}
            {page === 2 && <Phase2Page data={cexDex} />}
            {page === 3 && <Phase3Page liquidations={liqs} />}
            {page === 4 && <Phase4Page data={crossChain} />}
          </div>
        </main>
      </div>
    </>
  );
}
