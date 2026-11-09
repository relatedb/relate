//! A small linked idea store as a library: the input validators and the
//! closed link vocabulary ([`model`]), the embedded seed set ([`seed`]),
//! the verbs over the host-session seam ([`verbs`], [`session`]) and the
//! command dispatch ([`run`]). The binary (`src/main.rs`) is the MCP
//! child `relate run ideas` spawns; everything but the seam's real
//! implementation is host-free and unit-testable.

mod model;
mod seed;
mod session;
mod verbs;

pub use session::HostSession;
pub use verbs::{run, Envelope};

#[cfg(test)]
mod tests;
