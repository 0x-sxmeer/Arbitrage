// ─────────────────────────────────────────────────────────────────────────────
//  cex_dex/binance_feed.rs — Real-time Binance WebSocket Price Feeder
//
//  Subscribes to Binance mark-price streams for all configured symbols.
//  Uses exponentially-weighted moving average (EWMA) to filter noise.
//  Reconnects with exponential backoff on disconnect.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// Binance WebSocket endpoints
const BINANCE_WS_BASE: &str = "wss://fstream.binance.com/stream";
const RECONNECT_INITIAL_MS: u64 = 500;
const RECONNECT_MAX_MS:     u64 = 30_000;
const EWMA_ALPHA: f64 = 0.15;  // smoothing factor — lower = more stable, higher = more reactive

// ─────────────────────────────────────────────────────────────────────────────
//  Shared price state
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CexQuote {
    /// Raw mark price from Binance (USD)
    pub mark_price: f64,
    /// EWMA-smoothed price
    pub smooth_price: f64,
    /// Funding rate (8h annualized)
    pub funding_rate: f64,
    /// 24h price change %
    pub price_change_pct: f64,
    /// Bid-ask spread on Binance (tight = good signal quality)
    pub bid_ask_spread_bps: f64,
    /// When this quote was received
    pub timestamp_ms: u64,
    /// Whether we have sufficient data confidence
    pub is_stale: bool,
}

impl Default for CexQuote {
    fn default() -> Self {
        Self {
            mark_price: 0.0,
            smooth_price: 0.0,
            funding_rate: 0.0,
            price_change_pct: 0.0,
            bid_ask_spread_bps: f64::MAX,
            timestamp_ms: 0,
            is_stale: true,
        }
    }
}

/// Thread-safe price store shared across the engine
pub type PriceFeed = Arc<RwLock<HashMap<String, CexQuote>>>;

// ─────────────────────────────────────────────────────────────────────────────
//  Binance WebSocket message types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct BinanceStreamWrapper {
    stream: String,
    data:   serde_json::Value,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct MarkPriceData {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "p")]
    mark_price: String,
    #[serde(rename = "r")]
    funding_rate: String,
    #[serde(rename = "T")]
    next_funding_time: u64,
    #[serde(rename = "E")]
    event_time: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct BookTickerData {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "b")]
    best_bid: String,
    #[serde(rename = "a")]
    best_ask: String,
}

// ─────────────────────────────────────────────────────────────────────────────
//  BinancePriceFeeder
// ─────────────────────────────────────────────────────────────────────────────

pub struct BinancePriceFeeder {
    /// Symbols to track: ["ETHUSDT", "BTCUSDT", "WBTCUSDT", ...]
    symbols: Vec<String>,
    /// Shared price feed written to
    feed: PriceFeed,
    /// Staleness threshold — mark quote stale after this many ms without update
    stale_threshold_ms: u64,
}

impl BinancePriceFeeder {
    pub fn new(symbols: Vec<String>, stale_threshold_ms: u64) -> (Self, PriceFeed) {
        let feed: PriceFeed = Arc::new(RwLock::new(HashMap::new()));
        let feeder = Self {
            symbols,
            feed: feed.clone(),
            stale_threshold_ms,
        };
        (feeder, feed)
    }

    /// Build the combined Binance multi-stream URL
    fn build_ws_url(&self) -> String {
        let streams: Vec<String> = self.symbols.iter().flat_map(|sym| {
            let s = sym.to_lowercase();
            vec![
                format!("{}@markPrice", s),
                format!("{}@bookTicker", s),
            ]
        }).collect();
        format!("{}?streams={}", BINANCE_WS_BASE, streams.join("/"))
    }

    /// Run the feeder — reconnects automatically on disconnect.
    /// Spawn this with `tokio::spawn(feeder.run())`.
    pub async fn run(self) -> Result<()> {
        let mut reconnect_delay = RECONNECT_INITIAL_MS;

        loop {
            info!("📡 Connecting to Binance WebSocket ({} symbols)...", self.symbols.len());
            let url = self.build_ws_url();

            match self.run_session(&url).await {
                Ok(()) => {
                    warn!("Binance WS disconnected cleanly — reconnecting...");
                }
                Err(e) => {
                    error!("Binance WS error: {} — reconnecting in {}ms", e, reconnect_delay);
                }
            }

            // Mark all prices stale on disconnect
            {
                let mut feed = self.feed.write().await;
                for quote in feed.values_mut() {
                    quote.is_stale = true;
                }
            }

            sleep(Duration::from_millis(reconnect_delay)).await;
            reconnect_delay = (reconnect_delay as f64 * 2.0).min(RECONNECT_MAX_MS as f64) as u64;
        }
    }

    async fn run_session(&self, url: &str) -> Result<()> {
        let (ws_stream, _) = connect_async(url).await?;
        let (mut write, mut read) = ws_stream.split();
        info!("✓ Binance WS connected");

        // Spawn staleness checker
        let feed_clone = self.feed.clone();
        let stale_ms   = self.stale_threshold_ms;
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_millis(stale_ms / 2)).await;
                let now_ms = current_time_ms();
                let mut feed = feed_clone.write().await;
                for quote in feed.values_mut() {
                    if now_ms - quote.timestamp_ms > stale_ms {
                        quote.is_stale = true;
                    }
                }
            }
        });

        while let Some(msg) = read.next().await {
            match msg? {
                Message::Text(text) => {
                    if let Err(e) = self.process_message(&text).await {
                        debug!("Binance parse error: {}", e);
                    }
                }
                Message::Ping(data) => {
                    write.send(Message::Pong(data)).await?;
                }
                Message::Close(_) => {
                    info!("Binance WS closed by server");
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn process_message(&self, text: &str) -> Result<()> {
        let wrapper: BinanceStreamWrapper = serde_json::from_str(text)?;
        let stream = &wrapper.stream;
        let now_ms = current_time_ms();

        if stream.ends_with("@markPrice") {
            let data: MarkPriceData = serde_json::from_value(wrapper.data)?;
            let mark_price: f64 = data.mark_price.parse()?;
            let funding_rate: f64 = data.funding_rate.parse()?;

            let mut feed = self.feed.write().await;
            let quote = feed.entry(data.symbol.clone()).or_default();

            // EWMA smoothing
            if quote.smooth_price == 0.0 {
                quote.smooth_price = mark_price;
            } else {
                quote.smooth_price = EWMA_ALPHA * mark_price + (1.0 - EWMA_ALPHA) * quote.smooth_price;
            }

            quote.mark_price    = mark_price;
            quote.funding_rate  = funding_rate;
            quote.timestamp_ms  = now_ms;
            quote.is_stale      = false;

            debug!("💹 {} mark={:.4} smooth={:.4} fr={:.6}%",
                data.symbol, mark_price, quote.smooth_price, funding_rate * 100.0);

        } else if stream.ends_with("@bookTicker") {
            let data: BookTickerData = serde_json::from_value(wrapper.data)?;
            let bid: f64 = data.best_bid.parse()?;
            let ask: f64 = data.best_ask.parse()?;
            let mid = (bid + ask) / 2.0;
            let spread_bps = if mid > 0.0 { (ask - bid) / mid * 10_000.0 } else { f64::MAX };

            let mut feed = self.feed.write().await;
            let quote = feed.entry(data.symbol).or_default();
            quote.bid_ask_spread_bps = spread_bps;
        }

        Ok(())
    }
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ─────────────────────────────────────────────────────────────────────────────
//  DexPricePoller — reads on-chain DEX prices for comparison
// ─────────────────────────────────────────────────────────────────────────────

/// Maps Binance symbol → (token_address, USDC_pool_address, pool_type)
#[derive(Debug, Clone)]
pub struct DexPriceConfig {
    pub binance_symbol:    String,
    pub token_address:     String,
    pub quote_address:     String,  // USDC or USDT
    pub pool_address:      String,  // Uniswap V3 / Aerodrome pool
    pub pool_fee:          u32,     // V3 fee tier
    pub token_decimals:    u8,
    pub quote_decimals:    u8,
}

#[derive(Debug, Clone)]
pub struct DexQuote {
    pub price_usd:       f64,
    pub liquidity_usd:   f64,
    pub timestamp_ms:    u64,
    pub block_number:    u64,
}

pub type DexFeed = Arc<RwLock<HashMap<String, DexQuote>>>;
