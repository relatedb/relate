//! The shared PULL/DISCARD step (ADR-0110, ADR-0311) over the open result
//! store — split from `session` at the thousand-line rule. A child of that
//! module, so the handlers keep touching the session's private state; the
//! streamed result's arm lives in `session_stream.rs` (ADR-0682).

use std::time::Instant;

use super::session_stream::StreamStep;
use super::*;

/// PULL streams the rows it takes; DISCARD drops them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Consume {
    Pull,
    Discard,
}

impl Session {
    /// PULL {n, qid} (0x3F): at most `n` buffered RECORDs of the addressed
    /// result, `n = -1` drains (ADR-0110, ADR-0311).
    pub(super) fn pull<W: Write>(
        &mut self,
        stream: &mut W,
        fields: &[Pack],
        config: &ConnConfig,
    ) -> std::io::Result<Flow> {
        self.consume(stream, fields, Consume::Pull, config)
    }

    /// DISCARD {n, qid} (0x2F): drop at most `n` rows of the addressed
    /// result, `n = -1` drops the rest (ADR-0311).
    pub(super) fn discard<W: Write>(
        &mut self,
        stream: &mut W,
        fields: &[Pack],
        config: &ConnConfig,
    ) -> std::io::Result<Flow> {
        self.consume(stream, fields, Consume::Discard, config)
    }

    /// The shared PULL/DISCARD step. A result that leaves rows answers
    /// `{has_more: true}` and stays open; the step that takes its last row
    /// sends the ending summary (ADR-0309) and closes it; a deferred
    /// execution failure answers FAILURE and closes it (ADR-0319). The
    /// session guarantees at least one result is open (ADR-0314).
    pub(super) fn consume<W: Write>(
        &mut self,
        stream: &mut W,
        fields: &[Pack],
        mode: Consume,
        config: &ConnConfig,
    ) -> std::io::Result<Flow> {
        // ADR-0342: a present-but-malformed `n` or `qid` is a request
        // failure, exactly like an unknown `qid` below; `ConsumeRequest`
        // owns that decode, so every malformed shape leaves through this
        // one exit.
        let request = match ConsumeRequest::parse(fields) {
            Ok(request) => request,
            Err(message) => return self.fail_with_code(stream, REQUEST_INVALID_CODE, &message),
        };
        let Some(mut open) = self.results.take(request.qid) else {
            return self.fail_with_code(
                stream,
                REQUEST_INVALID_CODE,
                &format!("no open result with qid {}", request.qid),
            );
        };
        let started = Instant::now();
        let page = match &mut open.body {
            Body::Rows { result, cursor } => {
                let remaining = result.rows.len() - *cursor;
                let take = request.take(remaining);
                // ADR-0484: the page's rows and the ending summary's plan
                // value encode under the connection's dialect beside the
                // namespace the result's own header stamped — the same
                // fact its ending `db` entry reports.
                let codec = ValueCodec::new(self.dialect, result.header.namespace);
                // PULL writes the rows it takes; DISCARD writes nothing. A
                // row past the packstream depth limit is client-reachable
                // (ADR-0110), so the write's outcome decides the page.
                let written = match mode {
                    Consume::Pull => {
                        write_page(stream, &result.rows[*cursor..*cursor + take], codec)
                    }
                    Consume::Discard => Ok(()),
                };
                // ADR-0309: t_last is the whole milliseconds spent
                // streaming or discarding, summed over the result's PULL
                // and DISCARD messages. This message's share is measured
                // after its work, so the ending summary and every later
                // page carry the real running total.
                open.streamed += started.elapsed();
                match written {
                    // A path-invariant break, an unrepresentable value or
                    // an element with no element id is an engine bug or an
                    // engine-side value problem, never client input: keep
                    // it loud as the io error.
                    Err(RecordError::Fatal(e)) => return Err(e),
                    Err(RecordError::DepthExceeded) => Page::DepthExceeded,
                    Ok(()) => {
                        *cursor += take;
                        if *cursor == result.rows.len() {
                            // ADR-0357: the ending summary carries the plan
                            // pair — Neo4j's placement for the drained
                            // result, so strict clients (cypher-shell) read
                            // it from the PULL or DISCARD that ends the
                            // result. The one plan-summary policy decides
                            // the entry, never a second spelling here. The
                            // query notifications ride beside it (ADR-0363
                            // decision 11), through their one policy too.
                            Page::Ending(summary::ending(
                                result,
                                open.streamed,
                                codec,
                                &open.notification_config,
                            )?)
                        } else {
                            Page::HasMore
                        }
                    }
                }
            }
            Body::Stream(body) => self.consume_stream(
                stream,
                body,
                StreamStep {
                    request: &request,
                    mode,
                    streamed: &mut open.streamed,
                    notifications: &open.notification_config,
                },
                config,
            )?,
            Body::Failed(error) => return self.fail_engine(stream, error),
        };
        match page {
            Page::Ending(metadata) => {
                send_success(stream, metadata)?;
                self.note_query(QueryOutcome::Success);
            }
            Page::HasMore => {
                self.results.restore(open);
                send_success(stream, summary::has_more())?;
            }
            // ADR-0319: an execution failure of a streamed statement lands
            // at the PULL that reached it, after the rows that preceded it;
            // the result closes with it.
            Page::Failed(error) => return self.fail_engine(stream, &error),
            // The statement's worker waited out the PULL idle timeout and
            // was stopped; the result closes with the FAILURE.
            Page::OverCap => {
                return self.fail_with_code(
                    stream,
                    TRANSACTION_OUT_OF_MEMORY_CODE,
                    &format!(
                        "query result exceeds max-result-bytes limit of {} bytes",
                        config.limits.max_result_bytes
                    ),
                )
            }
            Page::Stalled => {
                return self.fail_with_code(
                    stream,
                    TRANSACTION_TIMED_OUT_CODE,
                    "result stream abandoned: no PULL arrived within the idle timeout",
                )
            }
            Page::DepthExceeded => {
                // ADR-0110: the depth limit is client-reachable (a deeply
                // nested parameter value returned in a row), so it degrades
                // the session — FAILURE, failed state, IGNORED until RESET,
                // after any RECORDs already written, never a killed
                // connection mid-stream. The result keeps its cursor.
                self.results.restore(open);
                return self.fail_with_code(
                    stream,
                    REQUEST_INVALID_CODE,
                    &format!("result value {}", MdbPackError::DepthExceeded),
                );
            }
        }
        Ok(Flow::Continue)
    }
}
