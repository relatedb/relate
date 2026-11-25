//! The streamed RUN (ADR-0682): an auto-commit statement runs on a worker
//! that owns the engine connection (`stream.rs`) and delivers bounded
//! batches as PULLs ask for them, so a result of any row count is never
//! held whole. The session holds the connection only between statements:
//! every use of it goes through [`Session::conn`], which takes it back from
//! a live worker first. A child of `session`, so it keeps touching the
//! session's private state.

use std::collections::BTreeMap;
use std::time::Instant;

use relate::Executed;

use crate::results::fails_run;
use crate::stream::{Event, Finished, StreamBody, StreamHandle};

use super::session_consume::Consume;
use super::*;

/// What one PULL or DISCARD asks of a streamed result: the request, the
/// mode, the result's running `t_last` and the notifications filter of its
/// ending summary.
pub(super) struct StreamStep<'a> {
    pub(super) request: &'a ConsumeRequest,
    pub(super) mode: Consume,
    pub(super) streamed: &'a mut Duration,
    pub(super) notifications: &'a NotificationConfig,
}

impl Session {
    /// The engine connection. A live streamed result's worker holds it, so
    /// the first use after a RUN stops that statement and takes the
    /// connection back: nothing else can run while a result streams, and a
    /// message that needs the connection ends the result (ADR-0311: an
    /// auto-commit RUN replaces the previous result; RESET, BEGIN and the
    /// transaction verbs drop it).
    pub(super) fn conn(&mut self) -> &mut Connection {
        if self.conn.is_none() {
            self.reclaim_conn();
        }
        self.conn
            .as_mut()
            .expect("the session owns its connection between statements")
    }

    /// Whether an explicit transaction is open. A session whose connection
    /// is lent to a worker is in auto-commit: only an auto-commit
    /// statement streams.
    pub(super) fn in_tx(&self) -> bool {
        self.conn.as_ref().is_some_and(Connection::in_transaction)
    }

    /// Drop every open result, taking the connection back from a live
    /// worker first so it is never lost with the result.
    pub(super) fn clear_results(&mut self) {
        if self.conn.is_none() {
            self.reclaim_conn();
        }
        self.results.clear();
    }

    fn reclaim_conn(&mut self) {
        let handle = self
            .results
            .take_stream_handle()
            .expect("a lent connection belongs to one open streamed result");
        self.conn = Some(handle.abandon());
    }

    /// Stage a RUN's statement (ADR-0319): an auto-commit statement outside
    /// a pipelined batch streams; every other statement runs whole.
    /// `Err` is a failure before execution, which fails the RUN.
    pub(super) fn stage<W: Write>(
        &mut self,
        stream: &mut W,
        cypher: &str,
        params: BTreeMap<String, Value>,
        config: &ConnConfig,
        in_transaction: bool,
    ) -> std::io::Result<relate::Result<(Body, RunHeadline)>> {
        if !in_transaction && !self.pipelined {
            return self.stage_streamed(stream, cypher, params, config);
        }
        let staged = match crate::keepalive::run_statement(
            self.conn(),
            stream,
            cypher,
            params,
            config.limits.query_timeout,
            config.limits.keep_alive_interval,
        )? {
            Ok(staged) => staged,
            Err(e) => return Ok(Err(e)),
        };
        Ok(match staged {
            Executed::Rows(result) => {
                let headline = RunHeadline {
                    columns: result.header.columns.clone(),
                    column_types: result.header.column_types.clone(),
                    t_first: result.header.elapsed,
                };
                Ok((
                    Body::Rows {
                        result: Box::new(result),
                        cursor: 0,
                    },
                    headline,
                ))
            }
            Executed::Failed(failure) if fails_run(&failure.error) => Err(failure.error),
            Executed::Failed(failure) => Ok((
                Body::Failed(failure.error),
                RunHeadline {
                    columns: failure.header.columns,
                    column_types: failure.header.column_types,
                    t_first: failure.header.elapsed,
                },
            )),
        })
    }

    /// Start the worker and wait for what the RUN SUCCESS reports: the
    /// columns and the first batch (whose first row stamps the ADR-0224
    /// column types), or the statement's end — an empty result, or a
    /// failure, classified like the staged path's (ADR-0319).
    fn stage_streamed<W: Write>(
        &mut self,
        stream: &mut W,
        cypher: &str,
        params: BTreeMap<String, Value>,
        config: &ConnConfig,
    ) -> std::io::Result<relate::Result<(Body, RunHeadline)>> {
        crate::stream::require_interval(config.limits.keep_alive_interval)?;
        let started = Instant::now();
        let namespace = self.conn().namespace();
        let conn = self
            .conn
            .take()
            .expect("the connection was just read from the session");
        let mut handle = StreamHandle::start(
            conn,
            cypher.to_string(),
            params,
            config.limits.pull_idle_timeout,
        )?;
        let mut columns = Vec::new();
        loop {
            let event = handle.wait(
                stream,
                config.limits.keep_alive_interval,
                config.limits.query_timeout,
            )?;
            match event {
                Event::Columns(announced) => columns = announced,
                Event::Batch(rows) => {
                    // ADR-0224: the first row stamps each column's type, the
                    // way a buffered result's header does.
                    let column_types = (0..columns.len())
                        .map(|i| {
                            let cell = rows.first().map(|row| {
                                row.get(i)
                                    .expect("engine rows are rectangular: one value per column")
                            });
                            relate_core::column_type_name(cell.unwrap_or(&Value::Null)).to_string()
                        })
                        .collect();
                    let headline = RunHeadline {
                        columns,
                        column_types,
                        t_first: started.elapsed(),
                    };
                    let body = Body::Stream(Box::new(StreamBody {
                        handle: Some(handle),
                        rows,
                        cursor: 0,
                        namespace,
                        finished: None,
                    }));
                    return Ok(Ok((body, headline)));
                }
                Event::Done(done) => {
                    let Finished { conn, result, .. } = *done;
                    self.conn = Some(conn);
                    return Ok(match result {
                        Err(e) => Err(e),
                        Ok(Executed::Rows(result)) => {
                            let headline = RunHeadline {
                                columns: result.header.columns.clone(),
                                column_types: result.header.column_types.clone(),
                                t_first: result.header.elapsed,
                            };
                            let body = Body::Stream(Box::new(StreamBody {
                                handle: None,
                                rows: Vec::new(),
                                cursor: 0,
                                namespace: result.header.namespace,
                                finished: Some(Box::new(result)),
                            }));
                            Ok((body, headline))
                        }
                        Ok(Executed::Failed(failure)) if fails_run(&failure.error) => {
                            Err(failure.error)
                        }
                        Ok(Executed::Failed(failure)) => Ok((
                            Body::Failed(failure.error),
                            RunHeadline {
                                columns: failure.header.columns,
                                column_types: failure.header.column_types,
                                t_first: failure.header.elapsed,
                            },
                        )),
                    });
                }
            }
        }
    }

    /// Dispose of a body the store refused (over the result cap): a live
    /// worker is stopped and its connection taken back.
    pub(super) fn discard_body(&mut self, body: Body) {
        if let Body::Stream(mut stream) = body {
            if let Some(handle) = stream.handle.take() {
                self.conn = Some(handle.abandon());
            }
        }
    }

    /// One PULL or DISCARD over a streamed result (ADR-0682): serve the
    /// batch in hand, fetch the next from the worker while the request
    /// wants more — and once the batch is spent, to learn whether the
    /// result has more, so the page that takes the last row carries the
    /// ending summary exactly as a buffered result's does. DISCARD of the
    /// rest lets the worker finish without delivering rows.
    pub(super) fn consume_stream<W: Write>(
        &mut self,
        stream: &mut W,
        body: &mut StreamBody,
        step: StreamStep<'_>,
        config: &ConnConfig,
    ) -> std::io::Result<Page> {
        let StreamStep {
            request,
            mode,
            streamed,
            notifications,
        } = step;
        let started = Instant::now();
        let codec = ValueCodec::new(self.dialect, body.namespace);
        let mut left = ConsumeRequest {
            qid: request.qid,
            cap: request.cap,
        };
        loop {
            let take = left.take(body.rows.len() - body.cursor);
            let written = match mode {
                Consume::Pull => {
                    write_page(stream, &body.rows[body.cursor..body.cursor + take], codec)
                }
                Consume::Discard => Ok(()),
            };
            match written {
                Err(RecordError::Fatal(e)) => return Err(e),
                Err(RecordError::DepthExceeded) => {
                    *streamed += started.elapsed();
                    return Ok(Page::DepthExceeded);
                }
                Ok(()) => {}
            }
            body.cursor += take;
            left.cap = left.cap.map(|cap| cap - take);
            if body.cursor < body.rows.len() {
                break;
            }
            let Some(handle) = body.handle.as_mut() else {
                break;
            };
            if mode == Consume::Discard && left.cap.is_none() {
                handle.skip();
            } else {
                handle.resume();
            }
            let next = loop {
                match handle.wait(
                    stream,
                    config.limits.keep_alive_interval,
                    config.limits.query_timeout,
                )? {
                    Event::Columns(_) => {}
                    event => break event,
                }
            };
            match next {
                Event::Batch(rows) => {
                    // The cap bounds what one streamed batch retains
                    // (ADR-0110): the statement ran on, but the result is
                    // never held whole.
                    if exceeds_cap(&rows, config.limits.max_result_bytes) {
                        // The result closes with the FAILURE, so the lent
                        // connection comes back first: dropping the live
                        // handle would discard it with the worker.
                        if let Some(handle) = body.handle.take() {
                            self.conn = Some(handle.abandon());
                        }
                        return Ok(Page::OverCap);
                    }
                    body.rows = rows;
                    body.cursor = 0;
                }
                Event::Done(done) => {
                    body.handle = None;
                    let Finished {
                        conn,
                        result,
                        stalled,
                    } = *done;
                    self.conn = Some(conn);
                    match result {
                        Ok(Executed::Rows(result)) => body.finished = Some(Box::new(result)),
                        Err(_) if stalled => return Ok(Page::Stalled),
                        Ok(Executed::Failed(_)) if stalled => return Ok(Page::Stalled),
                        Err(e)
                        | Ok(Executed::Failed(relate::ExecutionFailure { error: e, .. })) => {
                            return Ok(Page::Failed(e))
                        }
                    }
                    body.rows.clear();
                    body.cursor = 0;
                    break;
                }
                Event::Columns(_) => unreachable!("columns are skipped above"),
            }
        }
        *streamed += started.elapsed();
        match &body.finished {
            Some(result) if body.cursor == body.rows.len() => Ok(Page::Ending(summary::ending(
                result,
                *streamed,
                codec,
                notifications,
            )?)),
            _ => Ok(Page::HasMore),
        }
    }
}

/// Whether `rows` alone exceed the `--max-result-bytes` cap (`0` disables).
fn exceeds_cap(rows: &[Vec<Value>], limit: u64) -> bool {
    limit != 0 && relate_core::logical_bytes_within(rows.iter().flatten(), limit).is_none()
}
