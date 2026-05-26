// engine/src/discovery/whale_detector.rs
//
// Detects large wallet movements BEFORE they hit pools.
// Boosts affected tokens in Phase 3 backrun ranking.
// Also feeds Phase 2 (Binance inflows signal CEX-DEX opportunity).
//
// Sources:
//   1. Pending tx decoding — large swaps in mempool
//   2. Known CEX address movements (Binance, Coinbase deposits)
//   3. Large Transfer events > $100K

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, info};

use super::super::scoring::mega_scorer::WhaleScores;

// Known exchange hot wallets — movement from/to these = CEX buy/sell pressure
pub const KNOWN_CEX_WALLETS: &[(&str, &str)] = &[
    ("0x3f5CE5FBFe3E9af3971dD833D26bA9b5C936f0bE", "Binance Hot 1"),
    ("0xD551234Ae421e3BCBA99A0Da6d736074f22192FF", "Binance Hot 2"),
    ("0xF977814e90dA44bFA03b6295A0616a897441aceC", "Binance 8"),
    ("0x564286362092D8e7936f0549571a803B203aAceD", "Binance US"),
    ("0xA910f92ACdAf488fa6eF02174fb86208Ad7722ba", "Coinbase Prime"),
    ("0x71660c4005BA85c37ccec55d0C4493E66Fe775d3", "Coinbase 1"),
    ("0x503828976D22510aad0201ac7EC88293211D23Da", "Coinbase 2"),
    ("0x0D0707963952f2fBA59dD06f2b425ace40b492Fe", "Gate.io"),
    ("0xE93381fB4c4F14bDa253907b18faD305D799241a", "Huobi 1"),
    ("0xaB5C66752a9e8167967685F1450532fB96d5d24f", "Kraken 1"),
];

const MIN_WHALE_USD: f64 = 50_000.0;
const SCORE_DECAY:   f64 = 0.82;  // per minute
const MAX_SCORE:     f64 = 100.0;

#[derive(Debug, Clone)]
pub struct WhaleEvent {
    pub token_symbol:  String,
    pub size_usd:      f64,
    pub direction:     WhaleDir,
    pub source:        WhaleSource,
    pub wallet:        String,
    pub chain_id:      u64,
    pub confidence:    f64,
    pub detected_ms:   u64,
}

#[derive(Debug, Clone)]
pub enum WhaleDir   { Buy, Sell, Bridge }
#[derive(Debug, Clone)]
pub enum WhaleSource { MempoolSwap, CexInflow, CexOutflow, LargeTransfer }

pub struct WhaleDetector {
    whale_scores:   WhaleScores,
    recent_events:  Arc<RwLock<Vec<WhaleEvent>>>,
}

impl WhaleDetector {
    pub fn new(whale_scores: WhaleScores) -> Self {
        Self {
            whale_scores,
            recent_events: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Start score decay loop — runs every 60s
    pub async fn run_decay(self) {
        let mut tick = interval(Duration::from_secs(60));
        info!("🐳 WhaleDetector: decay loop started");
        loop {
            tick.tick().await;
            let mut scores = self.whale_scores.write().await;
            let mut to_remove = Vec::new();
            for (sym, score) in scores.iter_mut() {
                *score *= SCORE_DECAY;
                if *score < 2.0 { to_remove.push(sym.clone()); }
            }
            for sym in to_remove { scores.remove(&sym); }
            debug!("🐳 Whale scores: {} active", scores.len());
        }
    }

    /// Called from mempool listener when a large pending swap is decoded
    pub async fn on_large_pending_swap(
        &self,
        token_in:    &str,
        token_out:   &str,
        amount_usd:  f64,
        from_wallet: &str,
        chain_id:    u64,
    ) {
        if amount_usd < MIN_WHALE_USD { return; }

        // Check if wallet is known CEX (higher confidence)
        let is_cex = KNOWN_CEX_WALLETS.iter()
            .any(|(addr, _)| addr.eq_ignore_ascii_case(from_wallet));

        let confidence = if is_cex {
            (0.6 + amount_usd / 1_000_000.0 * 0.3).min(0.95)
        } else {
            (0.3 + amount_usd / 1_000_000.0 * 0.4).min(0.80)
        };

        // Boost score for the non-stable token
        let target_sym = if is_stable_sym(token_in)  { token_out } else { token_in };
        let boost = (confidence * MAX_SCORE * amount_usd / 500_000.0).min(MAX_SCORE);

        {
            let mut scores = self.whale_scores.write().await;
            let entry = scores.entry(target_sym.to_uppercase()).or_insert(0.0);
            *entry = (*entry + boost).min(MAX_SCORE);
        }

        let dir = if is_stable_sym(token_out) { WhaleDir::Sell } else { WhaleDir::Buy };
        let source = if is_cex { WhaleSource::CexOutflow } else { WhaleSource::MempoolSwap };

        info!(
            "🐳 WHALE | {} | ${:.0} | confidence={:.0}% | score_boost={:.0} | {:?}",
            target_sym, amount_usd, confidence * 100.0, boost, dir
        );

        let event = WhaleEvent {
            token_symbol: target_sym.to_uppercase(),
            size_usd:     amount_usd,
            direction:    dir,
            source,
            wallet:       from_wallet.to_string(),
            chain_id,
            confidence,
            detected_ms:  now_ms(),
        };

        let mut events = self.recent_events.write().await;
        events.push(event);
        if events.len() > 500 { events.remove(0); }
    }

    /// Called for large on-chain Transfer events (> $100K)
    pub async fn on_large_transfer(
        &self,
        token_sym:   &str,
        amount_usd:  f64,
        from_wallet: &str,
        to_wallet:   &str,
        chain_id:    u64,
    ) {
        if amount_usd < 100_000.0 { return; }

        let from_cex = KNOWN_CEX_WALLETS.iter().any(|(a,_)| a.eq_ignore_ascii_case(from_wallet));
        let to_cex   = KNOWN_CEX_WALLETS.iter().any(|(a,_)| a.eq_ignore_ascii_case(to_wallet));

        let (dir, confidence) = if to_cex {
            (WhaleDir::Sell, 0.85) // moving to exchange = sell incoming
        } else if from_cex {
            (WhaleDir::Buy, 0.80)  // withdrawing from exchange = buy incoming
        } else {
            (WhaleDir::Bridge, 0.40)
        };

        let boost = (confidence * MAX_SCORE * amount_usd / 1_000_000.0).min(MAX_SCORE);

        let mut scores = self.whale_scores.write().await;
        let entry = scores.entry(token_sym.to_uppercase()).or_insert(0.0);
        *entry = (*entry + boost).min(MAX_SCORE);

        info!(
            "🐳 TRANSFER | {} | ${:.0} | {:?} | boost={:.0}",
            token_sym, amount_usd, dir, boost
        );
    }

    pub async fn get_score(&self, symbol: &str) -> f64 {
        self.whale_scores.read().await
            .get(&symbol.to_uppercase())
            .copied()
            .unwrap_or(0.0)
    }

    pub async fn top_whale_tokens(&self, n: usize) -> Vec<(String, f64)> {
        let scores = self.whale_scores.read().await;
        let mut pairs: Vec<_> = scores.iter().map(|(k,v)| (k.clone(), *v)).collect();
        pairs.sort_by(|a,b| b.1.partial_cmp(&a.1).unwrap());
        pairs.into_iter().take(n).collect()
    }
}

fn is_stable_sym(sym: &str) -> bool {
    matches!(sym.to_uppercase().as_str(),
        "USDC"|"USDT"|"DAI"|"FRAX"|"BUSD"|"USDBC"|"USDC.E"|"USDE"|"PYUSD"|"EURC"
    )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_millis() as u64
}
