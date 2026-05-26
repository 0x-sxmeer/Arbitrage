// engine/src/cex_dex/binance_feed.rs
//
// Connects to Binance Futures WebSocket and streams mark prices for ALL
// Phase 2 eligible tokens (200-400 symbols simultaneously).
// Uses EWMA smoothing to filter noise. Auto-reconnects on disconnect.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const BINANCE_WS: &str = "wss://fstream.binance.com/stream";
const EWMA_ALPHA: f64  = 0.15;
const STALE_MS:   u64  = 5_000;

#[derive(Debug, Clone, Default)]
pub struct CexQuote {
    pub mark_price:      f64,
    pub smooth_price:    f64,
    pub funding_rate:    f64,
    pub bid_ask_bps:     f64,
    pub timestamp_ms:    u64,
    pub is_stale:        bool,
}

pub type CexFeed = Arc<RwLock<HashMap<String, CexQuote>>>;

#[derive(Deserialize)]
struct StreamWrap { stream: String, data: serde_json::Value }

#[derive(Deserialize)]
struct MarkData {
    #[serde(rename="s")] symbol: String,
    #[serde(rename="p")] mark_price: String,
    #[serde(rename="r")] funding_rate: String,
}

#[derive(Deserialize)]
struct BookData {
    #[serde(rename="s")] symbol: String,
    #[serde(rename="b")] best_bid: String,
    #[serde(rename="a")] best_ask: String,
}

pub struct BinancePriceFeeder {
    symbols: Vec<String>,
    feed:    CexFeed,
    stale_ms: u64,
}

impl BinancePriceFeeder {
    pub fn new(symbols: Vec<String>, stale_ms: u64) -> (Self, CexFeed) {
        let feed = Arc::new(RwLock::new(HashMap::new()));
        let f = Self { symbols, feed: feed.clone(), stale_ms };
        (f, feed)
    }

    fn ws_url(&self) -> String {
        // Binance allows up to 400 streams per connection
        let streams: Vec<String> = self.symbols.iter().flat_map(|s| {
            let sl = s.to_lowercase();
            vec![format!("{}@markPrice", sl), format!("{}@bookTicker", sl)]
        }).collect();
        format!("{}?streams={}", BINANCE_WS, streams.join("/"))
    }

    pub async fn run(self) -> Result<()> {
        let mut delay = 500u64;
        info!("📡 BinancePriceFeeder: connecting ({} symbols)", self.symbols.len());
        loop {
            match self.session().await {
                Ok(()) => warn!("Binance WS closed cleanly — reconnecting"),
                Err(e) => error!("Binance WS error: {} — retry in {}ms", e, delay),
            }
            { let mut f = self.feed.write().await; for q in f.values_mut() { q.is_stale = true; } }
            sleep(Duration::from_millis(delay)).await;
            delay = (delay * 2).min(30_000);
        }
    }

    async fn session(&self) -> Result<()> {
        let url = self.ws_url();
        let (ws, _) = connect_async(&url).await?;
        let (mut write, mut read) = ws.split();
        info!("✓ Binance WS connected ({} symbols)", self.symbols.len());

        // Staleness checker
        let feed2 = self.feed.clone();
        let stale = self.stale_ms;
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_millis(stale / 2)).await;
                let now = now_ms();
                let mut f = feed2.write().await;
                for q in f.values_mut() {
                    if now - q.timestamp_ms > stale { q.is_stale = true; }
                }
            }
        });

        while let Some(msg) = read.next().await {
            match msg? {
                Message::Text(t)   => { self.process(&t).await.ok(); }
                Message::Ping(d)   => { write.send(Message::Pong(d)).await?; }
                Message::Close(_)  => break,
                _                  => {}
            }
        }
        Ok(())
    }

    async fn process(&self, text: &str) -> Result<()> {
        let w: StreamWrap = serde_json::from_str(text)?;
        let now = now_ms();

        if w.stream.ends_with("@markPrice") {
            let d: MarkData = serde_json::from_value(w.data)?;
            let price: f64  = d.mark_price.parse()?;
            let fr: f64     = d.funding_rate.parse()?;
            let mut f = self.feed.write().await;
            let q = f.entry(d.symbol).or_default();
            q.smooth_price = if q.smooth_price == 0.0 { price }
                             else { EWMA_ALPHA*price + (1.0-EWMA_ALPHA)*q.smooth_price };
            q.mark_price = price; q.funding_rate = fr;
            q.timestamp_ms = now; q.is_stale = false;

        } else if w.stream.ends_with("@bookTicker") {
            let d: BookData = serde_json::from_value(w.data)?;
            let bid: f64 = d.best_bid.parse()?;
            let ask: f64 = d.best_ask.parse()?;
            let mid = (bid + ask) / 2.0;
            let spread_bps = if mid > 0.0 { (ask-bid)/mid*10_000.0 } else { f64::MAX };
            let mut f = self.feed.write().await;
            f.entry(d.symbol).or_default().bid_ask_bps = spread_bps;
        }
        Ok(())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_millis() as u64
}
