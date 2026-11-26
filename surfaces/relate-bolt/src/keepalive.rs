//! Engine calls that may outlive a driver's receive timeout (ADR-0312): the
//! connection thread waits on the engine's query worker and writes a NOOP
//! chunk each keep-alive interval until the engine returns, and a RUN runs
//! under the ADR-0110/0111 query timeout.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;

use relate::{CancelToken, Connection, Executed, Heartbeat, Value};

/// The default NOOP keep-alive interval (ADR-0312). The HELLO
/// `connection.recv_timeout_seconds` hint is twice this
/// (`summary::recv_timeout_hint`).
pub(crate) const DEFAULT_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// A Bolt 4.1+ NOOP: an empty chunk between messages.
pub(crate) const NOOP: [u8; 2] = [0x00, 0x00];

/// Run `work` with a heartbeat that writes a NOOP to `stream` each
/// `interval` the work is still running (ADR-0312). The NOOPs fall between
/// messages: nothing else is written while the engine works. A failed NOOP
/// write stops further ticks and is returned once `work` has finished, so
/// the caller closes the connection after the engine call completes.
///
/// A zero `interval` fails before `work` runs: `Heartbeat::every` rejects
/// it (ADR-0334) and the rejection surfaces as an invalid-input IO error.
pub(crate) fn with_keep_alive<W: Write, T>(
    stream: &mut W,
    interval: Duration,
    work: impl FnOnce(Heartbeat<'_>) -> T,
) -> std::io::Result<T> {
    let mut failed: Option<std::io::Error> = None;
    let mut tick = || {
        if failed.is_none() {
            if let Err(e) = stream.write_all(&NOOP).and_then(|()| stream.flush()) {
                failed = Some(e);
            }
        }
    };
    let heartbeat = Heartbeat::every(interval, &mut tick)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let out = work(heartbeat);
    match failed {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// Run one RUN statement through the engine's staged entry point (ADR-0319)
/// with the keep-alive heartbeat.
///
/// The statement runs on the engine connection's 64 MiB query worker
/// (ADR-0259) while this thread waits (ADR-0233). Its deadline is the
/// connection's statement timeout (ADR-0692), which the engine enforces:
/// it cancels cooperatively and returns its typed error promptly, so the
/// call below is the ADR-0111 cancel-then-await — the `Connection` stays
/// owned by this thread either way, and the session stays usable for the
/// next RUN.
pub(crate) fn run_statement<W: Write>(
    conn: &mut Connection,
    stream: &mut W,
    cypher: &str,
    params: BTreeMap<String, Value>,
    keep_alive_interval: Duration,
) -> std::io::Result<relate::Result<Executed>> {
    let token = CancelToken::new();
    with_keep_alive(stream, keep_alive_interval, |heartbeat| {
        conn.query_staged(cypher, params, token, Some(heartbeat))
    })
}

#[cfg(test)]
#[path = "keepalive_tests.rs"]
mod tests;
