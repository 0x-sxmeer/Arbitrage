pub mod defillama;
pub mod factory_watcher;
pub mod subgraph;

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::arb::router::LiquidityGraph;
use crate::chains::evm::EvmAdapter;
use crate::db::postgres::PostgresStore;


pub fn start_discovery_services(
    graph: Arc<RwLock<LiquidityGraph>>,
    pg: Option<Arc<PostgresStore>>,
    evm_adapter: Arc<EvmAdapter>,
) {
    info!("Starting automated pool discovery services...");

    // Method 1: DeFiLlama (Every 6 hours)
    let graph_clone = graph.clone();
    let pg_clone = pg.clone();
    tokio::spawn(async move {
        defillama::run_defillama_discovery(graph_clone, pg_clone).await;
    });

    // Method 2: The Graph Protocol (Every 30 mins)
    let graph_clone2 = graph.clone();
    let pg_clone2 = pg.clone();
    tokio::spawn(async move {
        subgraph::run_subgraph_discovery(graph_clone2, pg_clone2).await;
    });

    // Method 3: Factory Contract Watcher (Real-time)
    let graph_clone3 = graph.clone();
    let pg_clone3 = pg.clone();
    tokio::spawn(async move {
        factory_watcher::run_factory_watcher(graph_clone3, pg_clone3, evm_adapter).await;
    });
}
