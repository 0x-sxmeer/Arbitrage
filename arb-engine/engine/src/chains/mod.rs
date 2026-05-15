// ─────────────────────────────────────────────────────────────────────────────
//  chains/mod.rs — Chain adapter module
//
//  Re-exports the chain adapters for easy access from other modules.
//  Solana adapter is disabled (requires solana-sdk, which has Windows build issues).
// ─────────────────────────────────────────────────────────────────────────────
pub mod cosmos;
pub mod evm;
// pub mod solana;  // Phase 2: enable when solana-sdk builds on Windows
