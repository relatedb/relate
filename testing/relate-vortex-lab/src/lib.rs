//! Vortex in-memory columnar property-store experiment (ADR-0665).
//!
//! The round-1 measurement kernels (`bench`, `store`, `colstore`, `seam`,
//! `ops`), plus the engine glue (`engine`): the attach and codec-install
//! calls onto the product crates' `vortex-store` feature (ADR-0703). The
//! store and the codec themselves live in relate-executor and relate-storage
//! now; this crate measures them and is never a shipped surface.
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
