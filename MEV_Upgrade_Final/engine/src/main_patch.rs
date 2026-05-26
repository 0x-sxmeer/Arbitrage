// ═══════════════════════════════════════════════════════════════════════════════
//  PATCH FILE: engine/src/main.rs (ADDITIONS ONLY)
//
//  Add these blocks to the EXISTING main.rs.
//  Location markers show exactly where each block goes.
//
//  DO NOT replace main.rs — append/insert the sections below.
// ═══════════════════════════════════════════════════════════════════════════════

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 1: Add these mod declarations at the TOP of main.rs
//  (after the existing mod declarations around line 20)
// ─────────────────────────────────────────────────────────────────────────────
/*
mod discovery {
    pub mod mega_scanner;
}
mod scoring {
    pub mod mega_scorer;
}
mod cex_dex {
    pub mod binance_feed;
    pub mod spread_engine;
}
mod liquidations {
    pub mod liquidation_monitor;
    pub mod backrun;
}
mod cross_chain {
    pub mod cross_chain_engine;
    pub mod inventory_manager;
}
*/

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 2: Add these imports after existing `use` statements
// ─────────────────────────────────────────────────────────────────────────────
/*
use discovery::mega_scanner::MegaScanner;
use scoring::mega_scorer::{MegaScorer, PhaseListsArc, WhaleScores};
use std::collections::HashMap;
*/

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 3: Add this block in main() AFTER `let graph = ...` (around line 168)
//  and BEFORE the pool warm-up section.
//
//  This starts the mega scanner and scorer as background tasks.
//  The `phase_lists` Arc is shared with all phase execution engines.
// ─────────────────────────────────────────────────────────────────────────────
/*
    // ── Mega Token Universe (Phase 1-4 dynamic token discovery) ──────────────
    let whale_scores: WhaleScores = Arc::new(RwLock::new(HashMap::new()));
    let (mega_scanner, pool_registry, binance_listed) = MegaScanner::new();
    let (mega_scorer, phase_lists) = MegaScorer::new(
        pool_registry.clone(),
        binance_listed.clone(),
        whale_scores.clone(),
    );

    // Spawn MegaScanner (fetches from DeFiLlama, GeckoTerminal, subgraphs)
    tokio::spawn(async move {
        if let Err(e) = mega_scanner.run().await {
            tracing::error!("MegaScanner crashed: {}", e);
        }
    });

    // Spawn MegaScorer (re-ranks all tokens every 30s)
    tokio::spawn(async move {
        mega_scorer.run().await;
    });

    // Spawn phase list logger (every 60s)
    {
        let pl = phase_lists.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                t.tick().await;
                let lists = pl.read().await;
                tracing::info!(
                    "📊 Token Universe | pools:{} tokens:{} | P1:{} P2:{} P3:{} P4:{}",
                    lists.total_pools_scanned, lists.total_tokens_scored,
                    lists.phase1.len(), lists.phase2.len(),
                    lists.phase3.len(), lists.phase4.len(),
                );
            }
        });
    }

    info!("✅ Mega Token Universe scanner started (10+ data sources, 30s rescore)");
*/

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 4: CEX-DEX engine (Phase 2)
//  Add AFTER the mega scanner block above, if CEX_DEX_ENABLED=true
// ─────────────────────────────────────────────────────────────────────────────
/*
    if config.cex_dex_enabled {
        use cex_dex::binance_feed::BinancePriceFeeder;
        use cex_dex::spread_engine::SpreadEngine;

        // Get Binance-listed symbols from the live phase 2 list
        let binance_syms: Vec<String> = {
            let lists = phase_lists.read().await;
            lists.phase2.iter()
                .map(|t| format!("{}USDT", t.symbol.to_uppercase()))
                .take(400) // Binance WS supports up to 400 streams
                .collect()
        };
        info!("📡 CEX-DEX: subscribing to {} Binance symbols", binance_syms.len());

        let (feeder, cex_feed) = BinancePriceFeeder::new(binance_syms, 5_000);
        let spread_engine = SpreadEngine::new(
            cex_feed,
            phase_lists.clone(),
            config.cex_dex_min_spread_pct,
            config.cex_dex_loan_size_usd,
        );

        tokio::spawn(async move {
            if let Err(e) = feeder.run().await {
                tracing::error!("BinanceFeeder crashed: {}", e);
            }
        });

        let exec = config.execute_enabled;
        tokio::spawn(async move {
            if let Err(e) = spread_engine.run(exec).await {
                tracing::error!("SpreadEngine crashed: {}", e);
            }
        });
        info!("✅ CEX-DEX engine started");
    }
*/

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 5: Liquidation monitor (Phase 3)
//  Add AFTER Phase 2 block
// ─────────────────────────────────────────────────────────────────────────────
/*
    if config.liquidations_enabled {
        use liquidations::liquidation_monitor::LiquidationMonitor;

        let monitor = LiquidationMonitor::new(
            config.liquidation_min_profit_usd,
            whale_scores.clone(),
        );

        tokio::spawn(async move {
            monitor.run().await;
        });
        info!("✅ Liquidation monitor started (Aave V3 + Moonwell on Base)");
    }
*/

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 6: Cross-chain engine (Phase 4)
//  Add AFTER Phase 3 block
// ─────────────────────────────────────────────────────────────────────────────
/*
    if config.cross_chain_enabled {
        use cross_chain::cross_chain_engine::CrossChainEngine;

        let xc_engine = CrossChainEngine::new(
            phase_lists.clone(),
            config.cross_chain_trade_size_usd,
            config.op_http_url.clone(),
            config.arb_http_url.clone(),
        );

        let exec = config.execute_enabled;
        tokio::spawn(async move {
            if let Err(e) = xc_engine.run(exec).await {
                tracing::error!("CrossChainEngine crashed: {}", e);
            }
        });
        info!("✅ Cross-chain engine started (Base↔OP↔ARB)");
    }
*/

// ─────────────────────────────────────────────────────────────────────────────
//  SECTION 7: Pass phase_lists to MempoolListener
//  Modify the MempoolListener::new() call (around line 290) to include
//  the phase_lists so it can use them for backrun matching:
//
//  Change:
//    let listener = MempoolListener::new(...)
//
//  To include phase_lists in the args (you'll need to update
//  MempoolListener::new signature accordingly — see listener_patch.rs)
// ─────────────────────────────────────────────────────────────────────────────
/*
    // The phase_lists are passed to the mempool listener so that
    // backrun detection uses the live P3 token list for matching.
    // See mempool/listener.rs patch for how to consume it.
    let _phase_lists_for_mempool = phase_lists.clone();
*/
