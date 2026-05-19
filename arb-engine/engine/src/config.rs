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
    /// Current effective gas price in gwei (default; overridden by Redis cache)
    pub gas_price_gwei: f64,
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
                .unwrap_or_else(|_| "wss://mainnet.infura.io/ws/v3/demo".to_string()),

            eth_http_url: std::env::var("ETH_HTTP_URL")
                .unwrap_or_else(|_| "https://cloudflare-eth.com".to_string()),

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
        };

        cfg.validate()?;
        Ok(cfg)
    }

    /// [H-3] Comprehensive validation of all configuration values.
    fn validate(&self) -> Result<()> {
        // Validate WebSocket URL scheme
        if !self.eth_ws_url.starts_with("ws://") && !self.eth_ws_url.starts_with("wss://") {
            anyhow::bail!("ETH_WS_URL must start with ws:// or wss:// (got: {})", self.eth_ws_url);
        }

        // Validate HTTP URL scheme
        if !self.eth_http_url.starts_with("http://") && !self.eth_http_url.starts_with("https://") {
            anyhow::bail!("ETH_HTTP_URL must start with http:// or https:// (got: {})", self.eth_http_url);
        }

        // Validate optional Base WS URL
        if let Some(ref base_ws) = self.base_ws_url {
            if !base_ws.starts_with("ws://") && !base_ws.starts_with("wss://") {
                anyhow::bail!("BASE_WS_URL must start with ws:// or wss:// (got: {})", base_ws);
            }
        }

        // Validate optional Base HTTP URL
        if let Some(ref base_http) = self.base_http_url {
            if !base_http.starts_with("http://") && !base_http.starts_with("https://") {
                anyhow::bail!("BASE_HTTP_URL must start with http:// or https:// (got: {})", base_http);
            }
        }

        // Validate optional Arbitrum WS URL
        if let Some(ref arb_ws) = self.arb_ws_url {
            if !arb_ws.starts_with("ws://") && !arb_ws.starts_with("wss://") {
                anyhow::bail!("ARB_WS_URL must start with ws:// or wss:// (got: {})", arb_ws);
            }
        }

        // Validate private key format (64 hex chars with optional 0x prefix)
        if let Some(ref pk) = self.private_key {
            let pk_stripped = pk.trim_start_matches("0x");
            if pk_stripped.len() != 64 || !pk_stripped.chars().all(|c| c.is_ascii_hexdigit()) {
                anyhow::bail!("PRIVATE_KEY must be a 32-byte hex string (64 hex chars, optional 0x prefix)");
            }
        }

        // Validate Flashbots signing key format
        if let Some(ref fk) = self.flashbots_signing_key {
            let fk_stripped = fk.trim_start_matches("0x");
            if fk_stripped.len() != 64 || !fk_stripped.chars().all(|c| c.is_ascii_hexdigit()) {
                anyhow::bail!("FLASHBOTS_SIGNING_KEY must be a 32-byte hex string (64 hex chars)");
            }
        }

        // Validate contract address format (40 hex chars with optional 0x prefix)
        if let Some(ref addr) = self.contract_address {
            let addr_stripped = addr.trim_start_matches("0x");
            if addr_stripped.len() != 40 || !addr_stripped.chars().all(|c| c.is_ascii_hexdigit()) {
                anyhow::bail!("CONTRACT_ADDRESS must be a valid Ethereum address (40 hex chars, optional 0x prefix)");
            }
        }

        // Validate arbitrage parameter ranges
        if self.max_price_impact_bps > 500 {
            anyhow::bail!("MAX_PRICE_IMPACT_BPS > 500 (5%) is dangerously high — likely a misconfiguration");
        }

        if self.max_hops == 0 || self.max_hops > 6 {
            anyhow::bail!("MAX_HOPS must be between 1 and 6 (got: {})", self.max_hops);
        }

        if self.max_trade_size_pct <= 0.0 || self.max_trade_size_pct > 0.10 {
            anyhow::bail!("MAX_TRADE_SIZE_PCT must be between 0 and 0.10 (10%) (got: {})", self.max_trade_size_pct);
        }

        if self.eth_price_usd <= 0.0 {
            anyhow::bail!("ETH_PRICE_USD must be positive (got: {})", self.eth_price_usd);
        }

        if self.gas_price_gwei < 0.0 {
            anyhow::bail!("GAS_PRICE_GWEI must be non-negative (got: {})", self.gas_price_gwei);
        }

        tracing::info!(
            max_hops = self.max_hops,
            max_impact_bps = self.max_price_impact_bps,
            min_profit_usd = self.min_profit_usd,
            max_trade_pct = self.max_trade_size_pct,
            "Config validated successfully"
        );
        Ok(()
        )
    }

    /// Minimum profit in wei derived from `min_profit_usd` and `eth_price_usd`.
    pub fn min_profit_wei(&self) -> i128 {
        let eth_amount = self.min_profit_usd / self.eth_price_usd;
        let wei = eth_amount * 1e18;
        if wei > i128::MAX as f64 { i128::MAX }
        else if wei < 0.0 { 0 }
        else { wei as i128 }
    }

    /// Gas units per hop estimate (used by RouterConfig).
    pub const GAS_PER_HOP: u64 = 150_000;

    /// Log a sanitised summary (no secrets).
    pub fn log_summary(&self) {
        tracing::info!(
            eth_ws        = %self.eth_ws_url,
            eth_http      = %self.eth_http_url,
            redis         = %self.redis_url,
            has_postgres  = self.database_url.is_some(),
            contract      = ?self.contract_address,
            has_solana    = self.solana_rpc_url.is_some(),
            min_profit_usd = self.min_profit_usd,
            max_hops       = self.max_hops,
            "Configuration loaded"
        );
        if self.private_key.is_none() {
            tracing::warn!("PRIVATE_KEY not set — execution disabled (monitoring only)");
        }
    }
}
