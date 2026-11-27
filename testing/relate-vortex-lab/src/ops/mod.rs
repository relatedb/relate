//! Operator kernels O1..O20 (spec: `workload-mapping-findings.md`).

pub mod agg;
pub mod concurrency;
pub mod filter;
pub mod join;
pub mod memory;
pub mod point;
pub mod probe;
pub mod seam_bench;
pub mod seek;
pub mod sets;
pub mod vfs;
