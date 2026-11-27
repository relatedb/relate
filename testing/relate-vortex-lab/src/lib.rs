//! Vortex in-memory columnar property-store experiment (ADR-0665).
//!
//! The round-1 measurement kernels (`bench`, `store`, `colstore`, `seam`,
//! `ops`), plus the engine integration (`engine`): the Vortex-backed
//! `PropStore` and shard codec the executor and storage seams dispatch to.
//! Never a shipped surface — the published crates carry only the seams, and
//! this crate is the one that links the Vortex fork.
//!
//! The binaries are `relate-vortex-lab` (the kernel suite) and
//! `relate-vortex-ab` (the engine A/B: default store vs Vortex store over the
//! query-latency suite and the pokec groups).

pub mod bench;
pub mod colstore;
pub mod engine;
pub mod ops;
pub mod seam;
pub mod store;

// No `#[global_allocator]` here: a library never chooses the process's
// allocator (relate-query-latency's alloc.rs design — only the measuring
// binaries install the counting allocator). This crate's two measuring
// binaries (`main`, the kernels CLI, and `relate-vortex-ab`, the engine
// A/B) declare it themselves; a binary linking this crate for its types
// alone keeps the standard allocator and pays no per-allocation atomic.
