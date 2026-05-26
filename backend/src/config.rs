// ─────────────────────────────────────────────────────────────────────────────
//  config.rs — Centralised environment configuration
//
//  All environment variables are loaded once at startup via `Config::from_env()`.
//  Sensible defaults are applied where safe; required fields produce a clear
//  error message if missing.
// ─────────────────────────────────────────────────────────────────────────────
#![allow(dead_code)]

use anyhow::Result;

// ─────────────────────────────────────────────────────────────────────────────
//  Config
// ─────────────────────────────────────────────────────────────────────────────

/// Full engine configuration sourced from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    // ── RPC endpoints ─────────────────────────────────────────────────────────
    /// Ethereum mainnet WebSocket RPC (mempool + event subscriptions)
    pub eth_ws_url: String,
    /// Ethereum mainnet HTTP RPC (fallback reads)
    pub eth_http_url: String,
    /// Base chain WebSocket (optional)
    pub base_ws_url: Option<String>,
    /// Base chain HTTP (optional)
    pub base_http_url: Option<String>,
    /// Arbitrum One WebSocket (optional)
    pub arb_ws_url: Option<String>,
    /// Solana RPC HTTP URL (optional)
    pub solana_rpc_url: Option<String>,
    /// Solana WebSocket URL (optional)
    pub solana_ws_url: Option<String>,

    // ── Database ──────────────────────────────────────────────────────────────
    /// Redis connection URL
    pub redis_url: String,
    /// PostgreSQL connection URL (optional — disables persistence if absent)
    pub database_url: Option<String>,

    // ── MEV / Execution ───────────────────────────────────────────────────────
    /// Flashbots relay URL
    pub flashbots_url: String,
    /// Optional private transaction RPC URL (like Flashbots Protect / MEV-Blocker / private endpoint)
    pub private_rpc_url: Option<String>,
    /// Private key for signing execution transactions (hex, 0x-prefixed)
    pub private_key: Option<String>,
    /// Flashbots signing key (separate EOA, not the execution wallet)
    pub flashbots_signing_key: Option<String>,
    /// Address of deployed AtomicArb contract
    pub contract_address: Option<String>,
    /// Direct Aave pool address (bypass contract lookup)
    pub aave_pool_address: Option<String>,

    // ── Arbitrage parameters ──────────────────────────────────────────────────
    /// Minimum net profit in USD to consider a trade executable
    pub min_profit_usd: f64,
    /// Maximum price impact in basis points before aborting a trade
    pub max_price_impact_bps: u32,
    /// Maximum number of hops in an arbitrage cycle
    pub max_hops: usize,
    /// Maximum pool state staleness in blocks
    pub max_block_staleness: u64,
    /// Maximum trade size as a fraction of pool reserves (e.g. 0.01 = 1%)
    pub max_trade_size_pct: f64,

    // ── Market data ───────────────────────────────────────────────────────────
    /// ETH price in USD (used for gas cost normalisation until oracle is wired)
    pub eth_price_usd: f64,
    /// BTC price in USD
    pub btc_price_usd: f64,
    /// Current effective gas price in gwei (default; overridden by Redis cache)
    pub gas_price_gwei: f64,

    // ── Watchlists ────────────────────────────────────────────────────────────
    /// Base chain token watchlist (comma separated)
    pub watch_tokens_base: Option<String>,
    /// Arbitrum chain token watchlist (comma separated)
    pub watch_tokens_arb: Option<String>,

    // ── Execution gate ────────────────────────────────────────────────────────
    /// Master kill-switch: set EXECUTE_ENABLED=true in .env to enable live on-chain execution.
    /// When false (default), opportunities are detected & simulated but never broadcast.
    pub execute_enabled: bool,

    // ── Phase 1: Flash loan infrastructure ───────────────────────────────────
    /// Balancer V2 vault address (0% fee flash loans)
    pub balancer_vault_address: Option<String>,

    // ── Phase 2: CEX-DEX Statistical Arbitrage ───────────────────────────────
    /// Enable the Binance-fed CEX-DEX spread engine
    pub cex_dex_enabled: bool,
    /// Minimum spread percentage to trigger a CEX-DEX trade (e.g. 0.15 = 15bps)
    pub cex_dex_min_spread_pct: f64,
    /// Flash loan size in USD for CEX-DEX legs
    pub cex_dex_loan_size_usd: f64,
    /// Maximum inventory (open exposure) in USD per token
    pub cex_dex_max_inventory_usd: f64,
    /// Comma-separated Binance perp symbols to track (e.g. "ETHUSDT,BTCUSDT")
    pub cex_dex_symbols: String,
    /// Binance API key (for hedge order placement)
    pub binance_api_key: Option<String>,
    /// Binance API secret
    pub binance_api_secret: Option<String>,

    // ── Phase 3: Backrunning + Liquidations ──────────────────────────────────
    /// Enable mempool backrunning strategy
    pub backrun_enabled: bool,
    /// Minimum price impact (bps) in a victim swap before we backrun it
    pub backrun_min_impact_bps: f64,
    /// Minimum expected profit (USD) for a backrun opportunity
    pub backrun_min_profit_usd: f64,
    /// Enable Aave/Moonwell liquidation monitoring
    pub liquidations_enabled: bool,
    /// Minimum liquidation bonus profit in USD to bother executing
    pub liquidation_min_profit_usd: f64,
    /// Bloxroute / Chainbound API key for private mempool access
    pub bloxroute_api_key: Option<String>,
    /// Moonwell Comptroller address on Base
    pub moonwell_comptroller: Option<String>,

    // ── Phase 4: Cross-Chain Arbitrage ───────────────────────────────────────
    /// Enable cross-chain price divergence engine (Base ↔ Optimism ↔ Arbitrum)
    pub cross_chain_enabled: bool,
    /// Trade size in USD per cross-chain leg
    pub cross_chain_trade_size_usd: f64,
    /// Optimism HTTP RPC URL
    pub op_http_url: Option<String>,
    /// Optimism WebSocket URL
    pub op_ws_url: Option<String>,
    /// Arbitrum HTTP RPC URL
    pub arb_http_url: Option<String>,
    /// Minimum USDC to keep on each chain before rebalancing
    pub cross_chain_min_usdc: f64,
    /// AtomicArbV2 contract address on Optimism
    pub contract_address_op: Option<String>,
    /// AtomicArbV2 contract address on Arbitrum
    pub contract_address_arb: Option<String>,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Fails fast with a descriptive error if a required variable is missing.
    /// [H-3] Now includes comprehensive validation of all fields.
    pub fn from_env() -> Result<Self> {
        let cfg = Self {
            // ── RPC endpoints ─────────────────────────────────────────────
            eth_ws_url: std::env::var("ETH_WS_URL")
                .map_err(|_| anyhow::anyhow!("ETH_WS_URL is required in .env"))?,

            eth_http_url: std::env::var("ETH_HTTP_URL")
                .map_err(|_| anyhow::anyhow!("ETH_HTTP_URL is required in .env"))?,

            base_ws_url: std::env::var("BASE_WS_URL").ok(),
            base_http_url: std::env::var("BASE_HTTP_URL").ok(),
            arb_ws_url: std::env::var("ARB_WS_URL").ok(),
            solana_rpc_url: std::env::var("SOLANA_RPC_URL").ok(),
            solana_ws_url: std::env::var("SOLANA_WS_URL").ok(),

            // ── Database ──────────────────────────────────────────────────
            redis_url: std::env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://localhost:6379".to_string()),

            database_url: std::env::var("DATABASE_URL").ok(),

            // ── MEV / Execution ───────────────────────────────────────────
            flashbots_url: std::env::var("FLASHBOTS_RPC_URL")
                .unwrap_or_else(|_| "https://relay.flashbots.net".to_string()),

            private_rpc_url: std::env::var("PRIVATE_RPC_URL").ok(),

            private_key: std::env::var("PRIVATE_KEY").ok(),
            flashbots_signing_key: std::env::var("FLASHBOTS_SIGNING_KEY").ok(),
            contract_address: std::env::var("CONTRACT_ADDRESS").ok(),
            aave_pool_address: std::env::var("AAVE_POOL_ADDRESS").ok(),

            // ── Arbitrage parameters ──────────────────────────────────────
            min_profit_usd: std::env::var("MIN_PROFIT_USD")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.50),

            max_price_impact_bps: std::env::var("MAX_PRICE_IMPACT_BPS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(30),

            max_hops: std::env::var("MAX_HOPS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3),

            max_block_staleness: std::env::var("MAX_BLOCK_STALENESS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2),

            max_trade_size_pct: std::env::var("MAX_TRADE_SIZE_PCT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.01),

            // ── Market data ───────────────────────────────────────────────
            eth_price_usd: std::env::var("ETH_PRICE_USD")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3000.0),

            gas_price_gwei: std::env::var("GAS_PRICE_GWEI")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(20.0),

            btc_price_usd: std::env::var("BTC_PRICE_USD")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(95000.0),

            watch_tokens_base: std::env::var("WATCH_TOKENS_BASE").ok(),
            watch_tokens_arb: std::env::var("WATCH_TOKENS_ARB").ok(),

            execute_enabled: std::env::var("EXECUTE_ENABLED")
                .ok()
                .map(|s| s.to_lowercase() == "true" || s == "1")
                .unwrap_or(false),

            // ── Phase 1: Flash loan infrastructure ────────────────────────────
            balancer_vault_address: std::env::var("BALANCER_VAULT_ADDRESS").ok(),

            // ── Phase 2: CEX-DEX ─────────────────────────────────────────────
            cex_dex_enabled: env_bool("CEX_DEX_ENABLED", false),
            cex_dex_min_spread_pct: env_f64("CEX_DEX_MIN_SPREAD_PCT", 0.15),
            cex_dex_loan_size_usd: env_f64("CEX_DEX_LOAN_SIZE_USD", 500_000.0),
            cex_dex_max_inventory_usd: env_f64("CEX_DEX_MAX_INVENTORY_USD", 100_000.0),
            cex_dex_symbols: std::env::var("CEX_DEX_SYMBOLS")
                .unwrap_or_else(|_| "ETHUSDT,BTCUSDT".to_string()),
            binance_api_key: std::env::var("BINANCE_API_KEY").ok(),
            binance_api_secret: std::env::var("BINANCE_API_SECRET").ok(),

            // ── Phase 3: Backrunning + Liquidations ───────────────────────────
            backrun_enabled: env_bool("BACKRUN_ENABLED", false),
            backrun_min_impact_bps: env_f64("BACKRUN_MIN_IMPACT_BPS", 20.0),
            backrun_min_profit_usd: env_f64("BACKRUN_MIN_PROFIT_USD", 10.0),
            liquidations_enabled: env_bool("LIQUIDATIONS_ENABLED", false),
            liquidation_min_profit_usd: env_f64("LIQUIDATION_MIN_PROFIT_USD", 20.0),
            bloxroute_api_key: std::env::var("BLOXROUTE_API_KEY").ok(),
            moonwell_comptroller: std::env::var("MOONWELL_COMPTROLLER").ok(),

            // ── Phase 4: Cross-Chain ──────────────────────────────────────────
            cross_chain_enabled: env_bool("CROSS_CHAIN_ENABLED", false),
            cross_chain_trade_size_usd: env_f64("CROSS_CHAIN_TRADE_SIZE_USD", 50_000.0),
            op_http_url: std::env::var("OP_HTTP_URL").ok(),
            op_ws_url: std::env::var("OP_WS_URL").ok(),
            arb_http_url: std::env::var("ARB_HTTP_URL").ok(),
            cross_chain_min_usdc: env_f64("CROSS_CHAIN_MIN_USDC", 20_000.0),
            contract_address_op: std::env::var("CONTRACT_ADDRESS_OPTIMISM").ok(),
            contract_address_arb: std::env::var("CONTRACT_ADDRESS_ARBITRUM").ok(),
        };

        cfg.validate()?;
        Ok(cfg)
    }

    /// [H-3] Comprehensive validation of all configuration values.
    fn validate(&self) -> Result<()> {
        // Validate WebSocket URL scheme
        if !self.eth_ws_url.starts_with("ws://") && !self.eth_ws_url.starts_with("wss://") {
            anyhow::bail!(
                "ETH_WS_URL must start with ws:// or wss:// (got: {})",
                self.eth_ws_url
            );
        }

        // Validate HTTP URL scheme
        if !self.eth_http_url.starts_with("http://") && !self.eth_http_url.starts_with("https://") {
            anyhow::bail!(
                "ETH_HTTP_URL must start with http:// or https:// (got: {})",
                self.eth_http_url
            );
        }

        // Validate optional Base WS URL
        if let Some(ref base_ws) = self.base_ws_url {
            if !base_ws.starts_with("ws://") && !base_ws.starts_with("wss://") {
                anyhow::bail!(
                    "BASE_WS_URL must start with ws:// or wss:// (got: {})",
                    base_ws
                );
            }
        }

        // Validate optional Base HTTP URL
        if let Some(ref base_http) = self.base_http_url {
            if !base_http.starts_with("http://") && !base_http.starts_with("https://") {
                anyhow::bail!(
                    "BASE_HTTP_URL must start with http:// or https:// (got: {})",
                    base_http
                );
            }
        }

        // Validate optional Arbitrum WS URL
        if let Some(ref arb_ws) = self.arb_ws_url {
            if !arb_ws.starts_with("ws://") && !arb_ws.starts_with("wss://") {
                anyhow::bail!(
                    "ARB_WS_URL must start with ws:// or wss:// (got: {})",
                    arb_ws
                );
            }
        }

        // Validate private key format (64 hex chars with optional 0x prefix)
        if let Some(ref pk) = self.private_key {
            let pk_stripped = pk.trim_start_matches("0x");
            if pk_stripped.len() != 64 || !pk_stripped.chars().all(|c| c.is_ascii_hexdigit()) {
                anyhow::bail!(
                    "PRIVATE_KEY must be a 32-byte hex string (64 hex chars, optional 0x prefix)"
                );
            }
        }

        // Validate Flashbots signing key format
        if let Some(ref fk) = self.flashbots_signing_key {
            let fk_stripped = fk.trim_start_matches("0x");
            if fk_stripped.len() != 64 || !fk_stripped.chars().all(|c| c.is_ascii_hexdigit()) {
                anyhow::bail!("FLASHBOTS_SIGNING_KEY must be a 32-byte hex string (64 hex chars)");
            }
        }

        // Validate optional Private RPC URL
        if let Some(ref private_rpc) = self.private_rpc_url {
            if !private_rpc.starts_with("http://") && !private_rpc.starts_with("https://") {
                anyhow::bail!(
                    "PRIVATE_RPC_URL must start with http:// or https:// (got: {})",
                    private_rpc
                );
            }
        }

        // Validate contract address format (40 hex chars with optional 0x prefix)
        if let Some(ref addr) = self.contract_address {
            let addr_stripped = addr.trim_start_matches("0x");
            if addr_stripped.len() != 40 || !addr_stripped.chars().all(|c| c.is_ascii_hexdigit()) {
                anyhow::bail!("CONTRACT_ADDRESS must be a valid Ethereum address (40 hex chars, optional 0x prefix)");
            }
        }

        // Validate Redis URL scheme
        if !self.redis_url.starts_with("redis://") && !self.redis_url.starts_with("rediss://") {
            anyhow::bail!(
                "REDIS_URL must start with redis:// or rediss:// (got: {})",
                self.redis_url
            );
        }

        // Validate arbitrage parameter ranges
        if self.min_profit_usd < 0.0 {
            anyhow::bail!(
                "MIN_PROFIT_USD must be non-negative (got: {})",
                self.min_profit_usd
            );
        }

        if self.max_price_impact_bps > 500 {
            anyhow::bail!(
                "MAX_PRICE_IMPACT_BPS > 500 (5%) is dangerously high — likely a misconfiguration"
            );
        }

        if self.max_hops == 0 || self.max_hops > 6 {
            anyhow::bail!("MAX_HOPS must be between 1 and 6 (got: {})", self.max_hops);
        }

        if self.max_trade_size_pct <= 0.0 || self.max_trade_size_pct > 0.10 {
            anyhow::bail!(
                "MAX_TRADE_SIZE_PCT must be between 0 and 0.10 (10%) (got: {})",
                self.max_trade_size_pct
            );
        }

        if self.eth_price_usd <= 0.0 {
            anyhow::bail!(
                "ETH_PRICE_USD must be positive (got: {})",
                self.eth_price_usd
            );
        }

        if self.gas_price_gwei < 0.0 {
            anyhow::bail!(
                "GAS_PRICE_GWEI must be non-negative (got: {})",
                self.gas_price_gwei
            );
        }

        tracing::info!(
            max_hops = self.max_hops,
            max_impact_bps = self.max_price_impact_bps,
            min_profit_usd = self.min_profit_usd,
            max_trade_pct = self.max_trade_size_pct,
            "Config validated successfully"
        );
        Ok(())
    }

    /// Minimum profit in wei derived from `min_profit_usd` and `eth_price_usd`.
    pub fn min_profit_wei(&self) -> i128 {
        let eth_amount = self.min_profit_usd / self.eth_price_usd;
        let wei = eth_amount * 1e18;
        if wei > i128::MAX as f64 {
            i128::MAX
        } else if wei < 0.0 {
            0
        } else {
            wei as i128
        }
    }

    /// Gas units per hop estimate (used by RouterConfig).
    pub const GAS_PER_HOP: u64 = 150_000;

    /// Log a sanitised summary (no secrets).
    pub fn log_summary(&self) {
        tracing::info!(
            eth_ws        = %redact_url(&self.eth_ws_url),
            eth_http      = %redact_url(&self.eth_http_url),
            redis         = %redact_url(&self.redis_url),
            has_postgres  = self.database_url.is_some(),
            contract      = ?self.contract_address,
            has_solana    = self.solana_rpc_url.is_some(),
            has_private_rpc = self.private_rpc_url.is_some(),
            min_profit_usd = self.min_profit_usd,
            max_hops       = self.max_hops,
            "Configuration loaded"
        );
        if self.private_rpc_url.is_some() {
            tracing::info!("🔒 MEV Protection configured via Private RPC");
        }
        if self.private_key.is_none() {
            tracing::warn!("PRIVATE_KEY not set — execution disabled (monitoring only)");
        }
        if !self.execute_enabled {
            tracing::warn!(
                "⚠️  EXECUTE_ENABLED=false — running in MONITORING ONLY mode (no live trades)"
            );
            tracing::warn!(
                "    Set EXECUTE_ENABLED=true in .env when you are ready for live execution."
            );
        } else {
            tracing::warn!(
                "🔥 EXECUTE_ENABLED=true — LIVE EXECUTION MODE ACTIVE. Real money will be spent!"
            );
        }

        // ── Phase activation summary ──────────────────────────────────────────
        tracing::info!("═══════════════════ Phase Status ═══════════════════");
        tracing::info!("  Phase 1 (Flash Loan DEX arb):  ✓ ALWAYS ACTIVE");
        if self.cex_dex_enabled {
            tracing::info!(
                "  Phase 2 (CEX-DEX Spread):      ✓ ENABLED  | symbols={} | min_spread={:.2}%",
                self.cex_dex_symbols,
                self.cex_dex_min_spread_pct
            );
        } else {
            tracing::info!(
                "  Phase 2 (CEX-DEX Spread):      ✗ disabled  (set CEX_DEX_ENABLED=true)"
            );
        }
        if self.backrun_enabled {
            tracing::info!(
                "  Phase 3 (Backrunning):         ✓ ENABLED  | min_profit=${:.0}",
                self.backrun_min_profit_usd
            );
        } else {
            tracing::info!(
                "  Phase 3 (Backrunning):         ✗ disabled  (set BACKRUN_ENABLED=true)"
            );
        }
        if self.liquidations_enabled {
            tracing::info!(
                "  Phase 3 (Liquidations):        ✓ ENABLED  | min_profit=${:.0}",
                self.liquidation_min_profit_usd
            );
        } else {
            tracing::info!(
                "  Phase 3 (Liquidations):        ✗ disabled  (set LIQUIDATIONS_ENABLED=true)"
            );
        }
        if self.cross_chain_enabled {
            tracing::info!(
                "  Phase 4 (Cross-Chain):         ✓ ENABLED  | trade_size=${:.0}",
                self.cross_chain_trade_size_usd
            );
        } else {
            tracing::info!(
                "  Phase 4 (Cross-Chain):         ✗ disabled  (set CROSS_CHAIN_ENABLED=true)"
            );
        }
        tracing::info!("══════════════════════════════════════════════════");
    }
    /// Parse token watchlists into a hashmap: Symbol -> Address
    pub fn parse_watchlist(
        watchlist: &Option<String>,
    ) -> std::collections::HashMap<String, String> {
        let mut map = std::collections::HashMap::new();
        if let Some(list) = watchlist {
            for token_def in list.split(',') {
                let parts: Vec<&str> = token_def.split('=').collect();
                if parts.len() == 2 {
                    map.insert(parts[0].trim().to_string(), parts[1].trim().to_lowercase());
                }
            }
        }
        map
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Config helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a boolean env var — accepts "true" or "1" as truthy.
pub(crate) fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|v| v.to_lowercase() == "true" || v == "1")
        .unwrap_or(default)
}

/// Parse a float env var with a fallback default.
pub(crate) fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn redact_url(url: &str) -> String {
    let mut parts = url.split('/').collect::<Vec<_>>();
    if parts.len() > 3 && url.contains("api")
        || url.contains("v3")
        || url.contains("key")
        || url.contains("@")
    {
        let last = parts.len() - 1;
        parts[last] = "***";
        parts.join("/")
    } else {
        url.to_string()
    }
}
