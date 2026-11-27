//! The Vortex engine integration (ADR-0665): the lab's glue onto the two
//! product seams. The columnar store and the Vortex shard codec now live in
//! the product crates behind their default-off `vortex-store` feature
//! (ADR-0703); this crate keeps the measurement harness — the kernels, the
//! A/B driver, the epoch probes.
//!
//! - [`attach`] builds the columnar property store from the `neo4j`
//!   namespace's live graph through the executor's
//!   `attach_vortex_store`; point reads and the label/equality lookups then
//!   route to it.
//! - [`install_codec`] forwards to relate_storage's own Vortex shard-codec
//!   install, so a `vortex` store can be read and written.
//!
//! Both are the A/B harnesses' entry points; the database handle must be
//! quiescent when [`attach`] runs (the store is a snapshot of its state).

use relate::Database;

/// Build the Vortex property store from `db`'s `neo4j` graph and install it
/// in the executor's seam. The store answers every `ts >= commit_ts`:
/// post-attach commits reach it through the publication feed's delta (point
/// reads reflect them), while the set and aggregate arms answer from the
/// base snapshot and decline while a delta exists. The quiescence
/// requirement is about the attach itself — the store is built from the
/// graph's state at attach time.
pub fn attach(db: &Database) -> Result<(), String> {
    relate_executor::attach_vortex_store(db.sol_graph()).map_err(|e| e.to_string())
}

/// Install the Vortex shard codec into relate-storage's registry. Call it
/// before opening or creating a `vortex` store.
pub fn install_codec() {
    relate_storage::install_vortex_shard_codec();
}
