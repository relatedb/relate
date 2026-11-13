//! Minimal Bolt client for the harnesses (loadtests, memory cases): Bolt 5.4 with a 4.4 fallback.
//!
//! Speaks just enough of the wire to run statements against a
//! `relate-bolt` child: handshake, HELLO, LOGON, RUN, PULL, RESET,
//! GOODBYE. The handshake proposes Bolt 5.4 first and 4.4 second
//! (ADR-0484 decision 2); a server answering 5.4 authenticates at LOGON, one
//! answering 4.4 in the HELLO map (ADR-0126). A failed RUN or PULL sends
//! RESET so the next statement can run.
//!
//! The split follows the wire: the versions and their handshake (`version`),
//! the messages and the readers their responses go through (`messages`), and
//! the connection that speaks them (`session`).
//!
//! Import work pipelines RUN+PULL pairs: [`Session::enqueue_run_pull`]
//! queues a pair without writing it, [`Session::flush`] writes the whole
//! batch in one call, and [`Session::drain_batch`] reads the responses. A
//! failure puts the server in its failed state, so the pairs queued behind
//! it answer IGNORED and one RESET recovers the session.

mod messages;
mod session;
mod version;

pub use session::{BatchDrain, Session};
pub use version::BoltVersion;
