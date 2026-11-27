//! The Vortex engine integration (ADR-0665): the two seams a published
//! crate exposes and this crate fills in.
//!
//! - [`attach`] builds the columnar property store from the `neo4j`
//!   namespace's live graph and installs it through
//!   `Graph::attach_prop_store`; point reads and the label/equality lookups
//!   then route to it.
//! - [`install_codec`] registers the Vortex snapshot codec's shard
//!   serialization, so relate-storage can read and write a `vortex` store.
//!
//! Both are the A/B harnesses' entry points; the database handle must be
//! quiescent when [`attach`] runs (the store is a snapshot of its state).

mod prop_store;
mod shard_codec;

pub use prop_store::VortexStore;

use std::sync::Arc;

use relate::Database;
use relate_executor::PropStore;

/// Build the Vortex property store from `db`'s `neo4j` graph and install it
/// in the executor's seam. The store answers every `ts >= commit_ts`:
/// post-attach commits reach it through the publication feed's delta (point
/// reads reflect them), while the set and aggregate arms answer from the
/// base snapshot and decline while a delta exists. The quiescence
/// requirement is about the attach itself — the store is built from the
/// graph's state at attach time.
pub fn attach(db: &Database) -> Result<(), String> {
    let graph = db.sol_graph();
    let store = VortexStore::from_graph(graph, graph.commit_ts())
        .map_err(|e| format!("building the vortex store: {e}"))?;
    let store: Arc<dyn PropStore> = Arc::new(store);
    if !graph.attach_prop_store(store) {
        return Err("a property store is already attached to this graph".into());
    }
    Ok(())
}

/// Install the Vortex shard codec into relate-storage's registry. Call it
/// before opening or creating a `vortex` store.
pub fn install_codec() {
    shard_codec::install();
}
