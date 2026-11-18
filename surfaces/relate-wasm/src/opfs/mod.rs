//! Synchronous OPFS slot pool (ADR-0145).
//!
//! JavaScript allocates the catalog and slot handles before the engine runs.
//! After that, every [`Vfs`] call is a sync access-handle operation. The
//! catalog is the virtual directory: a path maps to a slot id, and directories
//! are prefixes.
//!
//! The pool's core — the catalog, the path grammar, the slot trait, and the
//! [`Vfs`] impl — is plain Rust and compiles on every target, so its semantics
//! (including the prefix-family rename) are unit-tested natively. The JS
//! glue (`js`, the wasm-bindgen externs in `slot`, and the prepare path in
//! `vfs`) is wasm32-only.

mod catalog;
#[cfg(target_arch = "wasm32")]
mod js;
mod path;
mod slot;
mod vfs;

#[cfg(target_arch = "wasm32")]
pub(crate) use vfs::OpfsVfs;

#[cfg(test)]
pub(crate) mod tests;
