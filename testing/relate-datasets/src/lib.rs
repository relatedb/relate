//! Dataset generators and loaders for Relate (synthetic fraud and IAM).

pub mod bench;
pub mod bulk;
pub mod checkpoint;
pub mod codec;
pub mod fraud;
pub mod harness;
pub mod identity;
pub mod load;
pub mod loadtest;
pub mod memory;
pub mod memory_controls;
pub mod memory_shape;
pub mod processing_graph;
pub mod processing_logical;
pub mod report;
pub(crate) mod serve_child;
pub mod shape_size;
pub mod shapes;

pub use bulk::{
    run_bulk as run_bulk_loadtest, BulkConfig, BulkEntry, BulkOp, BulkReport, DEFAULT_SIZES,
};
pub use checkpoint::{run as run_fraud_checkpoint, CheckpointConfig, CheckpointReport};
pub use fraud::{generate as generate_fraud, FraudConfig, FraudGraph};
pub use identity::{generate as generate_identity, IdentityConfig, IdentityGraph};
pub use load::{load_fraud, load_identity};
pub use loadtest::{
    run_fraud as run_fraud_loadtest, run_identity as run_identity_loadtest, LoadtestConfig,
    LoadtestReport,
};
