// ═══════════════════════════════════════════════════════════════════════════════
//  PATCH FILE: engine/src/config.rs (ADDITIONS ONLY)
//
//  Add these fields to the existing Config struct and from_env() method.
//  DO NOT replace config.rs — insert these additions into the existing file.
// ═══════════════════════════════════════════════════════════════════════════════

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 1: Add these fields to the Config struct
//  (after the existing `execute_enabled: bool` field)
// ─────────────────────────────────────────────────────────────────────────────
/*
    // ── Phase 2: CEX-DEX Statistical Arbitrage ────────────────────────────────
    pub cex_dex_enabled:         bool,
    pub cex_dex_min_spread_pct:  f64,
    pub cex_dex_loan_size_usd:   f64,
    pub binance_api_key:         Option<String>,
    pub binance_api_secret:      Option<String>,

    // ── Phase 3: Backrunning + Liquidations ───────────────────────────────────
    pub backrun_enabled:          bool,
    pub backrun_min_impact_bps:   f64,
    pub liquidations_enabled:     bool,
    pub liquidation_min_profit_usd: f64,
    pub bloxroute_api_key:        Option<String>,

    // ── Phase 4: Cross-Chain Arbitrage ────────────────────────────────────────
    pub cross_chain_enabled:       bool,
    pub cross_chain_trade_size_usd: f64,
    pub op_http_url:               Option<String>,
    pub op_ws_url:                 Option<String>,
    pub arb_http_url:              Option<String>,
    pub cross_chain_min_usdc:      f64,

    // ── Multi-chain contract addresses ────────────────────────────────────────
    pub contract_address_op:       Option<String>,
    pub contract_address_arb:      Option<String>,
*/

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 2: Add to from_env() method inside Config::from_env()
//  (after the existing `execute_enabled: ...` line)
// ─────────────────────────────────────────────────────────────────────────────
/*
            // ── Phase 2 ───────────────────────────────────────────────────────
            cex_dex_enabled: env_bool("CEX_DEX_ENABLED", false),
            cex_dex_min_spread_pct: env_f64("CEX_DEX_MIN_SPREAD_PCT", 0.15),
            cex_dex_loan_size_usd:  env_f64("CEX_DEX_LOAN_SIZE_USD", 500_000.0),
            binance_api_key:     std::env::var("BINANCE_API_KEY").ok(),
            binance_api_secret:  std::env::var("BINANCE_API_SECRET").ok(),

            // ── Phase 3 ───────────────────────────────────────────────────────
            backrun_enabled:        env_bool("BACKRUN_ENABLED", false),
            backrun_min_impact_bps: env_f64("BACKRUN_MIN_IMPACT_BPS", 20.0),
            liquidations_enabled:   env_bool("LIQUIDATIONS_ENABLED", false),
            liquidation_min_profit_usd: env_f64("LIQUIDATION_MIN_PROFIT_USD", 20.0),
            bloxroute_api_key: std::env::var("BLOXROUTE_API_KEY").ok(),

            // ── Phase 4 ───────────────────────────────────────────────────────
            cross_chain_enabled:        env_bool("CROSS_CHAIN_ENABLED", false),
            cross_chain_trade_size_usd: env_f64("CROSS_CHAIN_TRADE_SIZE_USD", 50_000.0),
            op_http_url:   std::env::var("OP_HTTP_URL").ok(),
            op_ws_url:     std::env::var("OP_WS_URL").ok(),
            arb_http_url:  std::env::var("ARB_HTTP_URL").ok(),
            cross_chain_min_usdc: env_f64("CROSS_CHAIN_MIN_USDC", 20_000.0),
            contract_address_op:  std::env::var("CONTRACT_ADDRESS_OPTIMISM").ok(),
            contract_address_arb: std::env::var("CONTRACT_ADDRESS_ARBITRUM").ok(),
*/

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 3: Add these helper functions at the BOTTOM of config.rs
//  (after the closing `}` of the Config impl)
// ─────────────────────────────────────────────────────────────────────────────
/*
fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|v| v.to_lowercase() == "true" || v == "1")
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
*/
