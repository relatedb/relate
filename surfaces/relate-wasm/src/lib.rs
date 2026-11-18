//! Relate for the browser: a thin `wasm-bindgen` wrapper around
//! `relate::Database`. `new` opens `:memory:`. `open` opens `opfs://`
//! on a sync-access-handle VFS (ADR-0145).
//!
//! `execute` and `executeScript` return the canonical `relate_model::wire`
//! JSON envelope (`{columns, columnTypes, rows, stats, notifications,
//! elapsed_ms, error}`) as JavaScript strings, and `renderResult` renders
//! envelopes as a recording (ADR-0131). Engine errors
//! flow through typed `Result`s; a Rust panic on wasm32 traps the module
//! (`panic = abort` is the wasm target default and `catch_unwind` is a no-op),
//! which JS observes as a `WebAssembly.RuntimeError`. Hosts classify that with
//! `isWasmTrap` (`js/trap.js`, shipped with the bundle): a host latches a
//! poisoned engine, or, running the engine in a worker, discards that
//! worker.
//!
//! The crate's split: the OPFS slot pool's plain-Rust core (`opfs/`) compiles
//! on every target so its catalog and `Vfs` semantics are unit-tested
//! natively, while the exported JavaScript surface (`bindings`) and the
//! JS-facing prepare glue compile for wasm32 only.

// The OPFS core: shipped on wasm32, unit-tested natively — nothing else
// compiles it on the host.
#[cfg(any(target_arch = "wasm32", test))]
mod opfs;

#[cfg(target_arch = "wasm32")]
mod bindings;

#[cfg(target_arch = "wasm32")]
pub use bindings::*;
