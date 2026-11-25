//! The batch-consumer seam (ADR-0682): [`Connection::query_streaming`]'s
//! public event vocabulary, the adapter that serves a caller's closure as
//! the engine's [`RowConsumer`], and the replay that feeds a materialized
//! result (writes, `EXPLAIN`, catalog commands, `UNION` folds, breakers,
//! explicit-transaction statements) through the same consumer after the
//! run.
//!
//! [`Connection::query_streaming`]: crate::Connection::query_streaming

use std::collections::BTreeMap;
use std::sync::Mutex;

use relate_core::tokens::WorkspaceCharge;
use relate_executor::{BatchFlow, RowConsumer, StreamSink};

use super::{Connection, Executed};
use crate::{CancelToken, QueryResult, Result, Value};

/// One delivery to a streaming consumer (ADR-0682): the result columns,
/// announced exactly once before the first batch, then one
/// [`StreamEvent::Batch`] per bounded batch of result rows, in row order.
#[derive(Debug)]
pub enum StreamEvent {
    /// The statement's result columns. A `RETURN *` learns its columns
    /// only at its first batch, so the announcement may arrive together
    /// with it — never later, never twice. The answer to this event is
    /// ignored: only a batch's [`BatchFlow::Stop`] ends the statement.
    Columns(Vec<String>),
    /// One bounded batch of result rows (at most
    /// [`relate_executor::BATCH_ROWS`]), each row one value per announced
    /// column, in order.
    Batch(Vec<Vec<Value>>),
}

/// Whether [`query_streaming_impl`](Connection::query_streaming_impl)
/// charges the rows it hands the consumer to the statement's workspace
/// reservation. `query`'s collecting consumer retains every row, so it
/// pays the same charge the materialized result did; a streaming surface
/// that retains nothing (a socket writer) runs uncharged — what the
/// consumer itself retains is its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Charge {
    /// Charge each engine-delivered batch to the statement's ledger — the
    /// collecting consumer's policy, keeping `query`'s pool accounting
    /// identical to the materialized path's.
    Collect,
    /// Never charge: the consumer owns whatever it retains.
    Consumer,
}

/// A streamed statement's authored column spellings (ADR-0663, ADR-0682),
/// by planned-column position: `None` when no column is renamed, else one
/// entry per announced column (`Some` where the spelling changed). The
/// admission that knows the plan writes it; the [`EventSink`] that owns
/// the consumer reads it at the columns announcement — a slot both share
/// because the engine holds the sink exclusively for the run.
type ColumnRenames = Mutex<Option<Vec<Option<String>>>>;

/// The connection's handle on a streamed statement (ADR-0682): the
/// engine's [`StreamSink`] plus the renames slot its consumer reads, so
/// the authored-column policy stays in the connection — the executor
/// announces the plan's names and never re-spells them.
pub(crate) struct ConsumerStream<'a> {
    sink: StreamSink<'a>,
    renames: &'a ColumnRenames,
}

impl<'a> ConsumerStream<'a> {
    /// The engine-side sink the dispatch hands the executor.
    pub(super) fn sink(&mut self) -> &mut StreamSink<'a> {
        &mut self.sink
    }

    /// Whether the columns announcement went out ([`StreamSink::engaged`]).
    pub(super) fn engaged(&self) -> bool {
        self.sink.engaged()
    }

    /// Set this attempt's renames — a retried attempt re-admits, so each
    /// attempt overwrites the slot with its own plan's spellings.
    pub(super) fn rename_columns(&self, renames: Option<Vec<Option<String>>>) {
        *self
            .renames
            .lock()
            .expect("the column renames slot is never poisoned: no holder panics") = renames;
    }
}

/// The engine-facing half of a streaming call: serves the caller's closure
/// as the engine's [`RowConsumer`], captures the first delivered row (the
/// result header's per-column type stamp derives from it — the streamed
/// result itself carries no rows to stamp from), and — under
/// [`Charge::Collect`] — charges the delivered rows to the statement's
/// workspace ledger as they land, the materialized path's
/// `retain_result_rows` policy applied per batch.
struct EventSink<'a> {
    on_event: &'a mut (dyn FnMut(StreamEvent) -> Result<BatchFlow> + Send),
    first_row: Option<Vec<Value>>,
    charge: Option<WorkspaceCharge>,
    charged_bytes: u64,
    /// Identity set across batches: a payload shared by rows of different
    /// batches is charged once, as the materialized walk charges it. The
    /// collecting consumer keeps every row, so the counted allocations
    /// outlive the meter.
    meter: relate_executor::ResultRowsMeter,
    renames: &'a ColumnRenames,
}

impl<'a> EventSink<'a> {
    fn new(
        on_event: &'a mut (dyn FnMut(StreamEvent) -> Result<BatchFlow> + Send),
        charge: Charge,
        renames: &'a ColumnRenames,
    ) -> Self {
        Self {
            on_event,
            first_row: None,
            charge: match charge {
                Charge::Collect => Some(WorkspaceCharge::empty()),
                Charge::Consumer => None,
            },
            charged_bytes: 0,
            meter: relate_executor::ResultRowsMeter::default(),
            renames,
        }
    }

    /// Close the streamed call over its finished result: a result the
    /// engine engaged restamps its header's per-column types from the
    /// first streamed row (ADR-0224 — the streamed result carries no rows
    /// for the construction-time stamp to read); one it never engaged
    /// replays through the consumer. The retained-row charge then moves to
    /// the statement's ledger — the materialized path's
    /// `transfer_to_ledger` epilogue.
    fn finish(mut self, result: &mut QueryResult, engaged: bool) -> Result<()> {
        if engaged {
            if let Some(first) = self.first_row.as_deref() {
                result.header.column_types =
                    crate::result::stamp_column_types(result.header.columns.len(), Some(first));
            }
        } else {
            replay_result(&mut self, result)?;
        }
        if let Some(charge) = self.charge.take() {
            charge.transfer_to_ledger();
        }
        Ok(())
    }

    /// The replay half: feed a materialized result's rows without
    /// charging — they were already charged where the materialized path
    /// charges them (`retain_result_rows`, or the write's pre-commit
    /// reservation).
    fn replay_batch(&mut self, rows: Vec<Vec<Value>>) -> Result<BatchFlow> {
        (self.on_event)(StreamEvent::Batch(rows))
    }

    /// Hand the caller the columns announcement as given — the replay's
    /// header already carries the authored spellings. The announcement
    /// carries no flow answer: a consumer that wants out answers `Stop` at
    /// the first batch.
    fn announce(&mut self, columns: Vec<String>) -> Result<()> {
        (self.on_event)(StreamEvent::Columns(columns))?;
        Ok(())
    }
}

impl RowConsumer for EventSink<'_> {
    fn columns(&mut self, columns: &[String]) -> Result<()> {
        // The authored spellings (ADR-0663) replace the plan's names here,
        // before the caller sees the announcement.
        let announced = match &*self
            .renames
            .lock()
            .expect("the column renames slot is never poisoned: no holder panics")
        {
            None => columns.to_vec(),
            Some(renames) => {
                if renames.len() != columns.len() {
                    return Err(crate::Error::internal(format!(
                        "the column renames cover {} columns, the announcement {}",
                        renames.len(),
                        columns.len()
                    )));
                }
                columns
                    .iter()
                    .zip(renames)
                    .map(|(column, authored)| authored.as_ref().unwrap_or(column).clone())
                    .collect()
            }
        };
        self.announce(announced)
    }

    fn batch(&mut self, rows: Vec<Vec<Value>>) -> Result<BatchFlow> {
        if self.first_row.is_none() {
            self.first_row = rows.first().cloned();
        }
        if let Some(charge) = &mut self.charge {
            // The cumulative measure of what the collector holds: each
            // batch's bytes as it lands, the same per-batch rule the
            // inter-clause drain follows.
            self.charged_bytes += self.meter.add(&rows);
            charge.resize(self.charged_bytes)?;
        }
        (self.on_event)(StreamEvent::Batch(rows))
    }
}

/// Feed a materialized result through the consumer the engine never
/// engaged (ADR-0682): the columns once, then the rows in bounded batches,
/// and the result keeps no second copy. A consumer answering
/// [`BatchFlow::Stop`] cancels the statement, the same answer the
/// streaming terminal gives.
fn replay_result(sink: &mut EventSink<'_>, result: &mut QueryResult) -> Result<()> {
    sink.announce(result.header.columns.clone())?;
    let rows = std::mem::take(&mut result.rows);
    let mut rows = rows.into_iter();
    loop {
        let batch: Vec<Vec<Value>> = rows.by_ref().take(relate_executor::BATCH_ROWS).collect();
        if batch.is_empty() {
            break;
        }
        if matches!(sink.replay_batch(batch)?, BatchFlow::Stop) {
            return Err(crate::Error::cancelled());
        }
    }
    Ok(())
}

impl Connection {
    /// The collecting consumer behind [`Self::query_with_params`] and
    /// [`Self::query_with_params_cancellable`] (ADR-0682): `query` is the
    /// streaming call with a consumer that keeps every row, landing them
    /// exactly as the materialized path landed them, charged to the
    /// statement's workspace reservation as they arrive.
    pub(super) fn query_collecting(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
    ) -> Result<QueryResult> {
        let mut rows = Vec::new();
        let mut result = self.query_streaming_impl(
            cypher,
            params,
            token,
            &mut |event| {
                if let StreamEvent::Batch(batch) = event {
                    rows.extend(batch);
                }
                Ok(BatchFlow::Continue)
            },
            Charge::Collect,
        )?;
        result.rows = rows;
        Ok(result)
    }

    /// Run a statement, handing its result to `on_batch` as it is produced
    /// instead of buffering it (ADR-0682): the consumer sees the result
    /// columns once ([`StreamEvent::Columns`]), then each bounded batch of
    /// rows ([`StreamEvent::Batch`]), and the call returns the statement's
    /// summary — stats, plan, notifications, timings, effect, bookmark —
    /// as a [`QueryResult`] whose `rows` are empty (the consumer has
    /// them).
    ///
    /// A statement whose terminal `RETURN` maps each row (no aggregation,
    /// `DISTINCT`, `ORDER BY`, `SKIP` or `LIMIT`; no `UNION` or `NEXT`)
    /// streams: the terminal pulls the next batch only after the consumer
    /// returns from the previous one, so peak memory stays independent of
    /// the row count. Every other statement — a `UNION`, a breaker, a
    /// write, `EXPLAIN`, a catalog command, a statement in an explicit
    /// transaction — runs materialized as ever and its result replays
    /// through the same consumer after the run.
    ///
    /// The consumer runs on the statement's own thread (the connection's
    /// query worker, or the caller's thread where the statement runs
    /// inline, ADR-0504/ADR-0663). A consumer answering
    /// [`BatchFlow::Stop`] ends the walk and cancels the statement: the
    /// call fails with `Error::Cancelled` (`"query cancelled"`) and the
    /// connection remains usable. `token` cancels the statement from
    /// another thread exactly as in
    /// [`Self::query_with_params_cancellable`].
    ///
    /// ```
    /// use relate::{BatchFlow, Database, StreamEvent, Value};
    ///
    /// let db = Database::open(":memory:").unwrap();
    /// let mut conn = db.connect().unwrap();
    /// conn.query("UNWIND range(1, 5) AS i CREATE (:N {i: i})").unwrap();
    ///
    /// let mut columns = Vec::new();
    /// let mut seen = 0;
    /// let summary = conn
    ///     .query_streaming(
    ///         "MATCH (n:N) RETURN n.i AS i",
    ///         Default::default(),
    ///         relate::CancelToken::new(),
    ///         |event| {
    ///             match event {
    ///                 StreamEvent::Columns(c) => columns = c,
    ///                 StreamEvent::Batch(rows) => seen += rows.len(),
    ///             }
    ///             Ok(BatchFlow::Continue)
    ///         },
    ///     )
    ///     .unwrap();
    /// assert_eq!(columns, ["i"]);
    /// assert_eq!(seen, 5);
    /// // The summary carries the statement's metadata; the rows went to
    /// // the consumer.
    /// assert!(summary.rows.is_empty());
    /// assert_eq!(summary.header.columns, ["i"]);
    /// ```
    pub fn query_streaming(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: CancelToken,
        mut on_batch: impl FnMut(StreamEvent) -> Result<BatchFlow> + Send,
    ) -> Result<QueryResult> {
        self.query_streaming_impl(cypher, params, &token, &mut on_batch, Charge::Consumer)
    }

    /// The unstaged form of [`Self::query_streaming_staged_impl`]: the
    /// body behind [`Self::query_streaming`] and the collecting
    /// [`Self::query_with_params`], where any failure is the `Err`.
    fn query_streaming_impl(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
        on_event: &mut (dyn FnMut(StreamEvent) -> Result<BatchFlow> + Send),
        charge: Charge,
    ) -> Result<QueryResult> {
        self.query_streaming_staged_impl(cypher, params, token, on_event, charge)?
            .into_result()
    }

    /// [`Self::query_streaming`] that tells the two failure phases apart
    /// like [`Self::query_staged`] (ADR-0319, ADR-0682): `Err` is a
    /// failure before execution; `Ok(Executed::Failed(..))` a failure
    /// while executing, with the planned result header — the output
    /// columns a surface announces before it reports the error — however
    /// many batches the consumer had already received; `Ok(Executed::Rows)`
    /// the statement summary, its `rows` empty.
    ///
    /// ```
    /// use relate::{BatchFlow, CancelToken, Database, Executed};
    ///
    /// let db = Database::open(":memory:").unwrap();
    /// let mut conn = db.connect().unwrap();
    /// let staged = conn
    ///     .query_streaming_staged(
    ///         "UNWIND [1, 0] AS x RETURN 10 / x AS v",
    ///         Default::default(),
    ///         CancelToken::new(),
    ///         |_| Ok(BatchFlow::Continue),
    ///     )
    ///     .unwrap();
    /// let Executed::Failed(failure) = staged else {
    ///     panic!("the division fails while executing");
    /// };
    /// assert_eq!(failure.header.columns, ["v"]);
    /// ```
    pub fn query_streaming_staged(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: CancelToken,
        mut on_batch: impl FnMut(StreamEvent) -> Result<BatchFlow> + Send,
    ) -> Result<Executed> {
        self.query_streaming_staged_impl(cypher, params, &token, &mut on_batch, Charge::Consumer)
    }

    /// The one streaming body behind [`Self::query_streaming`],
    /// [`Self::query_streaming_staged`] and the collecting
    /// [`Self::query_with_params`]: run the statement with the consumer
    /// attached, replay a result the engine never engaged (writes,
    /// `EXPLAIN`, catalog commands, explicit-transaction statements,
    /// `PROFILE`), and restamp the header's per-column types from the first
    /// streamed row — the streamed result carries no rows for the
    /// construction-time stamp to read (ADR-0224).
    fn query_streaming_staged_impl(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
        on_event: &mut (dyn FnMut(StreamEvent) -> Result<BatchFlow> + Send),
        charge: Charge,
    ) -> Result<Executed> {
        let renames = ColumnRenames::new(None);
        let mut sink = EventSink::new(on_event, charge, &renames);
        // The block ends the stream's borrow of the sink once the run's
        // outcome and the engagement fact are out.
        let (outcome, engaged) = {
            let mut stream = ConsumerStream {
                sink: StreamSink::new(&mut sink),
                renames: &renames,
            };
            let outcome = self.run_cancellable(cypher, params, token, None, Some(&mut stream));
            (outcome, stream.engaged())
        };
        let mut result = match outcome? {
            Executed::Rows(result) => result,
            failed @ Executed::Failed(_) => return Ok(failed),
        };
        sink.finish(&mut result, engaged)?;
        Ok(Executed::Rows(result))
    }
}
