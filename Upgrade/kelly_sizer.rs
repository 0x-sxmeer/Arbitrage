// ─────────────────────────────────────────────────────────────────────────────
//  cex_dex/kelly_sizer.rs — Kelly Criterion Position Sizer
//
//  Computes optimal position size using fractional Kelly:
//    f* = (edge / odds) * fraction
//  where:
//    edge = expected profit as fraction of bet size (spread - fees)
//    odds = win/loss ratio (assumed ~1 for mean-reverting spread strategies)
//    fraction = Kelly fraction (0.25 = quarter-Kelly for safety)
// ─────────────────────────────────────────────────────────────────────────────

pub struct KellySizer {
    /// Maximum loan/bet size in USD
    max_size_usd: f64,
    /// Kelly fraction (0.25 = conservative quarter-Kelly)
    kelly_fraction: f64,
    /// Historical win rate (updated live)
    win_rate: f64,
    /// Historical average win (as pct of bet)
    avg_win_pct: f64,
    /// Historical average loss (as pct of bet)
    avg_loss_pct: f64,
    /// Trade history for updating stats
    trade_count: u64,
    total_wins: u64,
    total_win_pct: f64,
    total_loss_pct: f64,
}

impl KellySizer {
    pub fn new(max_size_usd: f64) -> Self {
        Self {
            max_size_usd,
            kelly_fraction: 0.25,     // quarter-Kelly for safety
            win_rate:       0.65,     // conservative bootstrap assumption
            avg_win_pct:    0.002,    // 0.2% average win
            avg_loss_pct:   0.001,    // 0.1% average loss (tight stops)
            trade_count:    0,
            total_wins:     0,
            total_win_pct:  0.0,
            total_loss_pct: 0.0,
        }
    }

    /// Compute optimal position size in USD
    pub fn size_position(&self, spread_frac: f64, confidence: f64, max_loan: f64) -> f64 {
        // Kelly formula: f = (p*b - q) / b
        // where p = win_rate, q = 1-p, b = win/loss ratio
        let p = self.win_rate * confidence;
        let q = 1.0 - p;
        let b = self.avg_win_pct / self.avg_loss_pct.max(0.0001);

        let kelly_f = (p * b - q) / b;
        let kelly_f = kelly_f.max(0.0); // no negative bet size

        // Apply fractional Kelly and confidence scaling
        let fraction = kelly_f * self.kelly_fraction * confidence;
        let size_usd = self.max_size_usd * fraction;

        // Cap at max loan and scale by spread attractiveness
        let spread_multiplier = (spread_frac / 0.002).min(2.0); // scale up to 2x for large spreads
        (size_usd * spread_multiplier).min(max_loan).max(10_000.0)
    }

    /// Update internal statistics after each trade
    pub fn record_trade(&mut self, won: bool, pnl_pct: f64) {
        self.trade_count += 1;
        let n = self.trade_count as f64;

        if won {
            self.total_wins += 1;
            self.total_win_pct += pnl_pct;
        } else {
            self.total_loss_pct += pnl_pct.abs();
        }

        // Update running averages (minimum 10 trades)
        if self.trade_count >= 10 {
            self.win_rate     = self.total_wins as f64 / n;
            self.avg_win_pct  = self.total_win_pct / self.total_wins.max(1) as f64;
            self.avg_loss_pct = self.total_loss_pct / (n - self.total_wins as f64).max(1.0);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  cex_dex/position_manager.rs content (inline for simplicity)
// ─────────────────────────────────────────────────────────────────────────────
