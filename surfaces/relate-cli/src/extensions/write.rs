//! The extension host's write layer (ADR-0442 §3, over ADR-0157 and
//! ADR-0392's policies): the commit-policy framing the mount's writing
//! calls run through, and the store's one freshness read — a sibling of
//! the host it serves, under the extensions tree. The host drives the
//! same framing over the MCP tool calls, whose statements arrive with
//! their parameters.
//!
//! The runner's [`CommitFraming`] policy is the one difference between
//! import shapes: a `whole-import` extension keeps one explicit
//! transaction around its whole import, while a `per-batch` one (both
//! first-party importers — `git-to-graph`, ADR-0392; `fs-to-graph`,
//! ADR-0602) commits each batch as its own transaction — one
//! whole-import transaction would serialise its entire redo as a single
//! WAL record past the store's 64 MiB payload cap at the scales the
//! importers' caps admit.
//!
//! The framing is generic over the caller's error type — anything the
//! engine's typed error converts into: the extension host reports its
//! own typed refusals through its own `From` impl, so the engine's typed
//! error crosses the write boundary intact (ADR-0392's write-path rule:
//! callers and tests discriminate the engine's variant, never a
//! substring of the rendered message).

use relate::{Connection, Value};

use relate_wire::mcp::CommitFacts;

/// The commit framing a run's policy decides — one vocabulary, the wire
/// kit's [`CommitFacts`] (ADR-0447 §3): the manifest decodes `commit` into
/// it, the `initialize` result publishes it, and this trait is the one place
/// the policy becomes a runner action. A `whole-import` extension keeps one
/// explicit transaction around its whole import; a `per-batch` one (both
/// first-party importers — `git-to-graph`, ADR-0392; `fs-to-graph`,
/// ADR-0602) commits each call as its own transaction — one whole-import
/// transaction would serialise its entire redo as a single WAL record past
/// the store's 64 MiB payload cap at the scales the importers' caps admit.
///
/// The framing is generic over the caller's error type — anything the
/// engine's typed error converts into: the extension host reports its own
/// typed refusals through its own `From` impl, so the engine's typed error
/// crosses the write boundary intact (ADR-0392's write-path rule: callers
/// and tests discriminate the engine's variant, never a substring of the
/// rendered message).
pub(crate) trait CommitFraming {
    /// Write one batch on `conn` the way this policy commits: `WholeImport`
    /// writes inside the transaction [`Self::frame_import`] opened around
    /// the import; `PerBatch` gives the batch its own transaction.
    fn write_batch<T, E>(
        self,
        conn: &mut Connection,
        write: impl FnOnce(&mut Connection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<relate::Error>;

    /// Begin the whole-import transaction around `run_everything`, or run it
    /// bare: the run-level framing decision (ADR-0157 vs ADR-0392) in one
    /// place — the mount's session runs the whole exchange through this
    /// frame, under the policy the action's manifest declared.
    fn frame_import<E>(
        self,
        conn: &mut Connection,
        run_everything: impl FnOnce(&mut Connection) -> Result<(), E>,
    ) -> Result<(), E>
    where
        E: From<relate::Error>;
}

impl CommitFraming for CommitFacts {
    fn write_batch<T, E>(
        self,
        conn: &mut Connection,
        write: impl FnOnce(&mut Connection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<relate::Error>,
    {
        match self {
            Self::WholeImport => write(conn),
            Self::PerBatch => in_transaction(conn, write),
        }
    }

    fn frame_import<E>(
        self,
        conn: &mut Connection,
        run_everything: impl FnOnce(&mut Connection) -> Result<(), E>,
    ) -> Result<(), E>
    where
        E: From<relate::Error>,
    {
        match self {
            Self::WholeImport => in_transaction(conn, run_everything),
            Self::PerBatch => run_everything(conn),
        }
    }
}

/// Run `f` as one explicit transaction: on failure the transaction's
/// writes roll back instead of staying half-applied (ADR-0157; under
/// [`CommitFacts::PerBatch`] that is one batch's writes, ADR-0392). A
/// failed `begin`/`commit` crosses the boundary as the engine's typed
/// [`Error`](relate::Error) through the caller's `From` impl, never
/// a flattened string.
pub(crate) fn in_transaction<T, E>(
    conn: &mut Connection,
    f: impl FnOnce(&mut Connection) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<relate::Error>,
{
    conn.begin()?;
    match f(conn) {
        // ADR-0502 §7: the engine commit returns the transaction's
        // bookmark; the extension's result value is the caller's own —
        // the bookmark is not carried across the extension wire.
        Ok(value) => conn.commit().map_err(E::from).map(|_| value),
        Err(e) => {
            // Discard the partial writes; if the rollback itself fails,
            // the connection's drop is the implicit final rollback.
            let _ = conn.rollback();
            Err(e)
        }
    }
}

/// The store's node count — the one freshness read every "is this store
/// fresh" decision runs: the extension host's `requires-fresh-store`
/// refusal (ADR-0429 §2) reads this fact. A shape other than exactly
/// one row of one integer is an engine invariant break, not a store
/// mode.
pub(crate) fn node_count(conn: &mut Connection) -> Result<i64, relate::Error> {
    let result = conn.query("MATCH (n) RETURN count(n) AS n")?;
    // `count(n)` is exactly one row of one integer; every other shape is
    // an engine invariant break, not a store mode.
    let [row] = result.rows.as_slice() else {
        panic!("count(n) did not return one row: {result:?}")
    };
    match row.first() {
        Some(Value::Int(count)) => Ok(*count),
        cell => panic!("count(n) did not return an integer: {cell:?}"),
    }
}
