//! Contracts a query engine and a write engine share.
//!
//! The in-memory query engine and the local write engine are the only
//! implementations. A later engine implements these traits in its own crate.
//! Nothing here names a concrete graph or a concrete log.

pub mod graph_type;
pub mod redo;

mod catalog_image;
mod commit;
mod engines;
mod export;
mod query;
mod write;

pub use catalog_image::{
    map_put_database, CheckpointTheater, DatabaseEntry, SchemaSnapshot, ShowSchema, TheaterData,
};
pub use commit::CommitRecord;
pub use engines::{
    EngineIds, QueryEngineId, WriteEngineId, LOCAL_WRITE_ENGINE, MEMORY_QUERY_ENGINE,
};
pub use export::{GraphExport, GraphImport, SnapshotGraph};
pub use graph_type::{canonical_default_clause, DefaultValue, GraphTypeSpec};
pub use query::{QueryEngine, ReadRequest, WriteRequest};
pub use redo::{
    legacy_property_encoding, props_from_json, props_json, split_legacy_property_encoding,
    CatalogName, IndexDetailSpec, IndexKindSpec, OntologyOp, PullKind, RedoOp, WriteTarget,
    MAX_ONTOLOGY_SPEC_BYTES,
};
pub use write::{CommitGroup, CommittedSlot, FollowBatch, WriteEngine};
