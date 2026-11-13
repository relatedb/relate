//! Bolt session server and the standalone listener.
//!
//! The library exports the session (`handle_conn`) and PackStream so another
//! process can run a Bolt connection against an embedded `Database`
//! (ADR-0281). The standalone listener entry point is [`serve`]: one bind, a
//! first-byte peek, WebSocket or raw Bolt, and preset basic auth. The
//! historical `relate-bolt` binary is retired (LABS-381, ADR-0368) —
//! `relate serve` is the one product binary, embedding this crate through
//! `serve_listener_with_cap`; the container `entrypoint` binary and the
//! e2e test harness remain [`serve`]'s direct callers.

pub mod auth;
pub mod cli_args;
mod dialect;
mod failures;
mod fields;
mod framing;
mod keepalive;
pub mod packstream;
mod param_structs;
mod patches;
pub mod protocol;
mod protocol_notification_summary;
mod protocol_plan_summary;
mod requests;
mod results;
pub mod server;
mod session;
mod session_batch;
pub mod signal;
mod summary;
pub mod tls;
pub mod transport;
mod value_codec;
mod value_structs;
mod version;
pub mod websocket;

#[cfg(test)]
pub mod testutil;

pub use server::{serve, serve_listener, serve_listener_with_cap, serve_on};
