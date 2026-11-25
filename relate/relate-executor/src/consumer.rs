//! The terminal batch consumer (ADR-0682): the final `RETURN` of a
//! streaming statement hands each bounded batch of result rows to a
//! caller-supplied consumer instead of appending to the part's `Vec<Row>`.
//! The pull itself is the back-pressure: the terminal pulls the next batch
//! only after the consumer returns from the previous one, and a consumer
//! answering [`BatchFlow::Stop`] ends the walk and cancels the statement.

use relate_core::Result;

/// Rows per result batch (ADR-0555's batch bound): the terminal's consumer
/// deliveries and the materialized path's replay chunking share the one
/// bound the inter-clause stream already batches by.
pub const BATCH_ROWS: usize = 1024;

/// What the consumer wants after a batch: the next one, or the end of the
/// walk. `Stop` is `RowStream::stop` for the statement: the terminal stops
/// its input stream and the statement fails with `Error::Cancelled`
/// (`"query cancelled"`), exactly as a cancelled token ends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchFlow {
    /// Hand over the next batch.
    Continue,
    /// End the walk; the statement is cancelled.
    Stop,
}

/// The caller-supplied sink for a streaming statement's result (ADR-0682).
/// The engine calls [`RowConsumer::columns`] exactly once, before the first
/// batch (a `RETURN *` learns its columns only at its first batch, so the
/// columns may arrive together with it), then [`RowConsumer::batch`] once
/// per bounded batch of result rows, in row order. The consumer runs on the
/// statement's own thread — the connection's query worker, or the caller's
/// thread where the statement itself runs inline — never concurrently with
/// the pull that produced the batch.
///
/// `Send`: the statement runs on the connection's worker thread, so the
/// consumer crosses to it.
pub trait RowConsumer: Send {
    /// The result columns, once, before the first batch.
    fn columns(&mut self, columns: &[String]) -> Result<()>;
    /// One bounded batch of result rows, never more than [`BATCH_ROWS`]:
    /// each row carries one value per announced column, in the columns
    /// announcement's order.
    fn batch(&mut self, rows: Vec<Vec<relate_core::Value>>) -> Result<BatchFlow>;
}

/// The engine-side handle a statement carries for its consumer: wraps the
/// caller's [`RowConsumer`] with whether it has been engaged (columns were
/// announced), so the boundary can tell a statement whose rows streamed
/// from one whose result still sits in its `QueryResult` — the materialized
/// forms (writes, `EXPLAIN`, catalog commands, `UNION` folds, breakers)
/// replay their rows through the same sink after the run.
pub struct StreamSink<'a> {
    consumer: &'a mut dyn RowConsumer,
    engaged: bool,
}

impl<'a> StreamSink<'a> {
    /// Wrap the caller's consumer for one statement.
    pub fn new(consumer: &'a mut dyn RowConsumer) -> Self {
        Self {
            consumer,
            engaged: false,
        }
    }

    /// Whether the columns announcement went out: the streaming terminal
    /// engaged the consumer, so its `QueryResult` carries no rows.
    pub fn engaged(&self) -> bool {
        self.engaged
    }

    /// Announce the columns, once. Marks the sink engaged.
    pub(crate) fn columns(&mut self, columns: &[String]) -> Result<()> {
        self.engaged = true;
        self.consumer.columns(columns)
    }

    /// Hand over one batch. Marks the sink engaged.
    pub(crate) fn batch(&mut self, rows: Vec<Vec<relate_core::Value>>) -> Result<BatchFlow> {
        self.engaged = true;
        self.consumer.batch(rows)
    }
}
