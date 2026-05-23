// ─────────────────────────────────────────────────────────────────────────────
//  cex_dex/position_manager.rs — Inventory & Position Tracking
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use super::spread_engine::TradeDirection;

#[derive(Debug, Clone)]
pub struct Position {
    pub symbol:       String,
    pub direction:    TradeDirection,
    pub size_usd:     f64,
    pub dex_entry:    f64,
    pub cex_entry:    f64,
    pub opened_at_ms: u64,
    pub stop_loss:    f64,   // price level to exit (on DEX side)
}

pub struct PositionManager {
    max_inventory_usd: f64,
    open_positions:    HashMap<String, Vec<Position>>,
    total_exposure:    f64,
}

impl PositionManager {
    pub fn new(max_inventory_usd: f64) -> Self {
        Self {
            max_inventory_usd,
            open_positions: HashMap::new(),
            total_exposure: 0.0,
        }
    }

    pub fn can_open(&self, symbol: &str, size_usd: f64) -> bool {
        // Check per-symbol cap (50% of max per token)
        let sym_exposure: f64 = self.open_positions
            .get(symbol)
            .map(|v| v.iter().map(|p| p.size_usd).sum())
            .unwrap_or(0.0);

        sym_exposure + size_usd <= self.max_inventory_usd * 0.5
            && self.total_exposure + size_usd <= self.max_inventory_usd
    }

    pub fn open_position(
        &mut self,
        symbol:    String,
        direction: TradeDirection,
        size_usd:  f64,
        dex_entry: f64,
        cex_entry: f64,
    ) {
        let entry_spread = (cex_entry - dex_entry).abs() / cex_entry.max(0.001);
        
        // Stop-loss: if spread reverses 2x, exit
        let stop_loss = match direction {
            TradeDirection::BuyDexSellCex => dex_entry * (1.0 - entry_spread * 2.0),
            TradeDirection::SellDexBuyCex => dex_entry * (1.0 + entry_spread * 2.0),
        };

        let pos = Position {
            symbol:       symbol.clone(),
            direction,
            size_usd,
            dex_entry,
            cex_entry,
            opened_at_ms: now_ms(),
            stop_loss,
        };

        self.open_positions.entry(symbol).or_default().push(pos);
        self.total_exposure += size_usd;
    }

    pub fn close_position(&mut self, symbol: &str, pnl_usd: f64) {
        if let Some(positions) = self.open_positions.get_mut(symbol) {
            if let Some(pos) = positions.pop() {
                self.total_exposure -= pos.size_usd;
            }
        }
    }

    pub fn total_exposure_usd(&self) -> f64 { self.total_exposure }
    pub fn position_count(&self) -> usize    { self.open_positions.values().map(|v| v.len()).sum() }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
