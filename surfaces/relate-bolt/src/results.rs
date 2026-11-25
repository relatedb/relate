//! RUN results buffered by the session (ADR-0311): the store of open
//! results, the byte accounting the `--max-result-bytes` cap needs
//! (ADR-0110) and the execution-failure classification (ADR-0319). The
//! session owns the RUN/PULL/DISCARD handlers, drives this store downward
//! and owns the RECORD encoding (`Session::consume`); this module never
//! touches the wire.

use std::time::Duration;

use relate::QueryResult;

use crate::fields::NotificationConfig;
use crate::stream::{StreamBody, StreamHandle};

/// Where an open result's rows stand.
pub(crate) enum Body {
    /// Buffered rows (ADR-0110); `cursor` is the next row to send. Boxed so
    /// the buffered result does not dominate the enum's size beside the
    /// small failure arm (clippy's `large_enum_variant`).
    Rows {
        result: Box<QueryResult>,
        cursor: usize,
    },
    /// A statement streaming from its worker (ADR-0682): holds at most one
    /// batch, never the result.
    Stream(Box<StreamBody>),
    /// An execution failure, reported at the first PULL or DISCARD
    /// (ADR-0319).
    Failed(relate::Error),
}

/// One RUN's result, open until its ending SUCCESS or its FAILURE is sent.
pub(crate) struct OpenResult {
    /// The qid the RUN SUCCESS reported (ADR-0311); PULL and DISCARD
    /// address it through [`OpenResults::find`].
    qid: i64,
    pub(crate) body: Body,
    /// Logical bytes held against `--max-result-bytes` (ADR-0311).
    bytes: u64,
    /// Time spent streaming or discarding so far, summed over the result's
    /// PULL and DISCARD messages: the ending `t_last` (ADR-0309).
    pub(crate) streamed: Duration,
    /// The notifications filter this statement's ending summary runs
    /// under (ADR-0484 decision 8): resolved at RUN from the RUN extra
    /// over the BEGIN or HELLO config; the default filters nothing, and a
    /// 4.4 connection never leaves it.
    pub(crate) notification_config: NotificationConfig,
}

/// A result that does not fit the connection's `--max-result-bytes` budget
/// (ADR-0110): the store is left untouched and the refused body comes back
/// to the caller (a streamed one holds the session's connection).
pub(crate) struct OverCap(pub(crate) Body);

impl std::fmt::Debug for OverCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OverCap")
    }
}

/// The session's open results (ADR-0311): one per RUN in an explicit
/// transaction, at most one in auto-commit. Cleared by COMMIT, ROLLBACK,
/// RESET, BEGIN and every auto-commit RUN.
#[derive(Default)]
pub(crate) struct OpenResults {
    open: Vec<OpenResult>,
    /// The qid the next RUN gets: 0 for the first RUN after a clear.
    next_qid: i64,
}

impl OpenResults {
    pub(crate) fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// Drop every open result and restart qids at 0.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// The logical bytes all open results hold (ADR-0311).
    fn bytes(&self) -> u64 {
        self.open.iter().map(|open| open.bytes).sum()
    }

    /// Open a result under the next qid and return that qid (ADR-0311) —
    /// the one the RUN SUCCESS reports. The reservation covers everything
    /// the open result retains (ADR-0110/0311): the statement text kept
    /// for the ending summary's plan pair (ADR-0357) in both arms, plus
    /// the rows' logical bytes for a buffered result. Both are measured against the budget the open
    /// results leave under `limit` (`0` disables the cap, expressed as the
    /// budget `u64::MAX`); a result that does not fit fails with
    /// [`OverCap`] and is not stored. `notification_config` is the filter
    /// the statement's ending summary runs under (ADR-0484 decision 8).
    pub(crate) fn open(
        &mut self,
        body: Body,
        limit: u64,
        statement: String,
        notification_config: NotificationConfig,
    ) -> Result<i64, OverCap> {
        let budget = if limit == 0 {
            u64::MAX
        } else {
            limit.saturating_sub(self.bytes())
        };
        // The RUN's statement text is counted against the cap like the rows
        // are (ADR-0357 §5): the ending summary's plan pair derives its
        // echo from that text (the tree carries it, ADR-0359), so the text
        // is bounded here even though only the tree's echo is retained past
        // the call — a transaction full of huge statements cannot park
        // unbounded text outside the cap. The rows get what remains:
        // checked, so a statement alone over the budget is the same
        // failure, stated once.
        let statement_bytes = statement.len() as u64;
        let Some(rows_budget) = budget.checked_sub(statement_bytes) else {
            return Err(OverCap(body));
        };
        // The rows the body holds now: the whole buffered result, or the
        // first batch of a streamed one (later batches are held to the same
        // cap as they arrive).
        let held = match &body {
            Body::Rows { result, .. } => {
                relate_core::logical_bytes_within(result.rows.iter().flatten(), rows_budget)
            }
            Body::Stream(stream) => {
                relate_core::logical_bytes_within(stream.rows.iter().flatten(), rows_budget)
            }
            Body::Failed(_) => Some(0),
        };
        let Some(held) = held else {
            return Err(OverCap(body));
        };
        let bytes = statement_bytes + held;
        let qid = self.next_qid;
        self.next_qid += 1;
        self.open.push(OpenResult {
            qid,
            body,
            bytes,
            streamed: Duration::ZERO,
            notification_config,
        });
        Ok(qid)
    }

    /// The index of the result `qid` addresses (`-1`: the most recent
    /// RUN's), if it is still open.
    fn find(&self, qid: i64) -> Option<usize> {
        // `-1` derives the most recent qid from the counter: every qid
        // below `next_qid` was handed out by `open`, and `clear` restarts
        // both at 0.
        let qid = if qid == -1 {
            self.next_qid.checked_sub(1)?
        } else {
            qid
        };
        self.open.iter().position(|open| open.qid == qid)
    }

    /// Take the result `qid` addresses out of the store (`None` when it is
    /// not open), for the caller to stream or discard.
    pub(crate) fn take(&mut self, qid: i64) -> Option<OpenResult> {
        let index = self.find(qid)?;
        Some(self.open.remove(index))
    }

    /// Take the live worker of the streamed result out of the store, for
    /// the session to stop it and get its connection back (ADR-0682).
    pub(crate) fn take_stream_handle(&mut self) -> Option<StreamHandle> {
        self.open.iter_mut().find_map(|open| match &mut open.body {
            Body::Stream(body) => body.handle.take(),
            _ => None,
        })
    }

    /// Put a taken result back, still open.
    pub(crate) fn restore(&mut self, open: OpenResult) {
        self.open.push(open);
    }
}

/// Whether a failure raised while executing a planned statement still fails
/// the RUN (ADR-0319): exactly the compile-class engine errors, from the
/// engine's one classification (ADR-0502 §2). Every other variant is
/// reported at the first PULL or DISCARD.
pub(crate) fn fails_run(e: &relate::Error) -> bool {
    relate_error::error_info(e).stage == relate_error::ErrorStage::Compile
}

#[cfg(test)]
#[path = "results_tests.rs"]
mod tests;
