

// Common Panel Container Style
const panelStyle = {
  flex: 1,
  minWidth: "280px",
  border: "1px solid #1A2233",
  borderRadius: "8px",
  background: "#0D1117",
  overflow: "hidden",
  display: "flex",
  flexDirection: "column",
};

const headerStyle = {
  padding: "12px 16px",
  borderBottom: "1px solid #1A2233",
  background: "#0A0E14",
  display: "flex",
  alignItems: "center",
  gap: "10px",
};

const titleStyle = {
  fontSize: "12px",
  fontWeight: 600,
  color: "#E2E8F0",
  letterSpacing: "0.08em",
  textTransform: "uppercase",
};

const bodyStyle = {
  padding: "16px",
  display: "flex",
  flexDirection: "column",
  gap: "12px",
  background: "#06080A",
  flex: 1,
};

const metricRowStyle = {
  display: "flex",
  justifyContent: "space-between",
  alignItems: "center",
};

const metricLabelStyle = {
  fontSize: "10px",
  color: "#64748B",
  fontFamily: "monospace",
  textTransform: "uppercase",
};

const metricValueStyle = {
  fontSize: "13px",
  fontWeight: 700,
  fontFamily: "monospace",
};

export function CexDexPanel({ spread, lastOpportunity, hourlyPnl }) {
  const isActive = typeof spread === "number" && spread > 0;
  return (
    <div style={panelStyle} className="milestone-card">
      <div style={headerStyle}>
        <div
          className="live-dot"
          style={{
            width: 6,
            height: 6,
            borderRadius: "50%",
            background: "#F59E0B",
            boxShadow: "0 0 8px #F59E0B",
          }}
        />
        <span style={titleStyle}>Phase 2: CEX-DEX Arb</span>
      </div>
      <div style={bodyStyle}>
        <div style={metricRowStyle}>
          <span style={metricLabelStyle}>Current Spread</span>
          <span
            style={{
              ...metricValueStyle,
              color: isActive ? "#10B981" : "#94A3B8",
            }}
          >
            {isActive ? `+${spread.toFixed(3)}%` : "0.000%"}
          </span>
        </div>
        <div style={metricRowStyle}>
          <span style={metricLabelStyle}>Last Opp</span>
          <span
            style={{ ...metricValueStyle, color: "#CBD5E1", fontSize: "11px" }}
          >
            {lastOpportunity || "None"}
          </span>
        </div>
        <div
          style={{
            ...metricRowStyle,
            marginTop: "auto",
            paddingTop: "12px",
            borderTop: "1px dashed #1A2233",
          }}
        >
          <span style={metricLabelStyle}>Est. Hourly PnL</span>
          <span style={{ ...metricValueStyle, color: "#F59E0B" }}>
            ${(hourlyPnl || 0).toFixed(2)}
          </span>
        </div>
      </div>
    </div>
  );
}

export function LiquidationPanel({
  pendingLiquidations,
  executedToday,
  totalBonus,
}) {
  const hasPending = pendingLiquidations > 0;
  return (
    <div style={panelStyle} className="milestone-card">
      <div style={headerStyle}>
        <div
          className="live-dot"
          style={{
            width: 6,
            height: 6,
            borderRadius: "50%",
            background: "#EF4444",
            boxShadow: "0 0 8px #EF4444",
          }}
        />
        <span style={titleStyle}>Phase 3: Liquidations</span>
      </div>
      <div style={bodyStyle}>
        <div style={metricRowStyle}>
          <span style={metricLabelStyle}>Pending Alerts</span>
          <span
            style={{
              ...metricValueStyle,
              color: hasPending ? "#EF4444" : "#94A3B8",
            }}
          >
            {pendingLiquidations || 0}
          </span>
        </div>
        <div style={metricRowStyle}>
          <span style={metricLabelStyle}>Executed Today</span>
          <span style={{ ...metricValueStyle, color: "#CBD5E1" }}>
            {executedToday || 0}
          </span>
        </div>
        <div
          style={{
            ...metricRowStyle,
            marginTop: "auto",
            paddingTop: "12px",
            borderTop: "1px dashed #1A2233",
          }}
        >
          <span style={metricLabelStyle}>Total Bonus (USD)</span>
          <span style={{ ...metricValueStyle, color: "#10B981" }}>
            +${(totalBonus || 0).toFixed(2)}
          </span>
        </div>
      </div>
    </div>
  );
}

export function CrossChainPanel({ priceDivergences, inventoryByChain }) {
  const hasDivergences = priceDivergences > 0;
  return (
    <div style={panelStyle} className="milestone-card">
      <div style={headerStyle}>
        <div
          className="live-dot"
          style={{
            width: 6,
            height: 6,
            borderRadius: "50%",
            background: "#3B82F6",
            boxShadow: "0 0 8px #3B82F6",
          }}
        />
        <span style={titleStyle}>Phase 4: Cross-Chain</span>
      </div>
      <div style={bodyStyle}>
        <div style={metricRowStyle}>
          <span style={metricLabelStyle}>Active Divergences</span>
          <span
            style={{
              ...metricValueStyle,
              color: hasDivergences ? "#38BDF8" : "#94A3B8",
            }}
          >
            {priceDivergences || 0}
          </span>
        </div>

        <div
          style={{
            marginTop: "8px",
            display: "flex",
            flexDirection: "column",
            gap: "6px",
          }}
        >
          <span style={{ ...metricLabelStyle, color: "#475569" }}>
            Inventory Status
          </span>
          <div style={{ display: "flex", gap: "4px", flexWrap: "wrap" }}>
            {inventoryByChain ? (
              Object.entries(inventoryByChain).map(([chain, amount]) => (
                <span
                  key={chain}
                  style={{
                    background: "rgba(56, 189, 248, 0.1)",
                    color: "#38BDF8",
                    padding: "2px 6px",
                    borderRadius: "4px",
                    fontSize: "9px",
                    fontFamily: "monospace",
                  }}
                >
                  {chain}: ${(amount || 0).toFixed(0)}
                </span>
              ))
            ) : (
              <span
                style={{
                  fontSize: "10px",
                  color: "#475569",
                  fontFamily: "monospace",
                }}
              >
                Scanning chains...
              </span>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
