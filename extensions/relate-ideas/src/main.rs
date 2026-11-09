//! The `ideas` extension child (ADR-0431 §11): its whole input is one
//! request envelope on stdin — the resolved flag values and positional
//! the host parsed from the committed manifest — and its whole output is
//! the MCP session it drives as the client. It carries no `Database` and
//! never touches a store (ADR-0447 §4): the host owns the store, the
//! commit policy, the caps and the exit codes.

use relate_ideas::{run, Envelope};
use relate_wire::mcp::serve;

/// The mount name the handshake reports; the committed manifest declares
/// the same package.
const NAME: &str = "ideas";

fn main() {
    serve(
        NAME,
        env!("CARGO_PKG_VERSION"),
        |child, envelope: Envelope| run(child, &envelope),
    );
}
