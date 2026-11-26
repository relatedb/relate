//! The auto-commit statement route's streaming reply (ADR-0682, ADR-0386).
//!
//! The statement runs while the reply is written: the engine hands its
//! result batches to a consumer ([`relate::Connection::query_streaming`]),
//! and the consumer decides the framing the way the one-shot path does —
//! from the serialized size, but over a body that does not exist yet. It
//! buffers the rows it is given until their encoding reaches
//! [`STREAM_THRESHOLD_BYTES`], then writes the chunked head and the
//! envelope's opening (`{"data":{"fields":[...],"values":[`) and from there
//! encodes every batch straight into the frame writer, so a result of any
//! length is held one batch at a time. An answer that ends below the
//! threshold is collected into the ordinary reply and framed by the
//! one-shot path, byte for byte as before.
//!
//! The head is a promise: once the chunked 200 is out, a later failure can
//! no longer change the status. It is written as the Query API's in-band
//! `errors` member after the rows (`{"data":{...},"errors":[...]}`), and
//! the body then ends. A failure before the head — an error ahead of the
//! first byte of a below-threshold answer — is the ordinary error reply with
//! its own status.
//!
//! A failed socket write (including the write timeout, which ends a peer
//! that stopped reading) answers [`BatchFlow::Stop`], which cancels the
//! statement: a stalled client does not keep the engine producing rows.

use std::io::{self, Write};

use relate::{BatchFlow, CancelToken, Error, Namespace, QueryResult, StreamEvent, Value};

use crate::content_encoding;
use crate::reply::{
    collected, engine_error, errors_member, run_error, success_tail, PendingStatement, Reply,
    ResponseMediaType,
};
use crate::stream::{write_chunked_head, ByteCount, ChunkedBody, STREAM_THRESHOLD_BYTES};
use crate::v2_envelope::{streamed as envelope, RowView};
use crate::v2_json::{refuse_entities_without_element_ids, Form};

/// How a pending statement's reply left [`serve`].
pub(crate) enum Streamed {
    /// The reply is on the wire, from the chunked head to the last chunk.
    Done,
    /// The answer never reached the threshold, or failed before the head:
    /// the ordinary reply, for the caller to frame and write.
    Buffered(Reply),
}

/// Run `pending`, streaming its rows to `writer` under the reply's content
/// type and `extra_headers`. `accept_encoding` is the request's, read only
/// if the body streams.
pub(crate) fn serve<W: Write + Send>(
    writer: &mut W,
    pending: PendingStatement,
    extra_headers: Vec<(String, String)>,
    accept_encoding: Option<&str>,
) -> io::Result<Streamed> {
    let PendingStatement {
        mut conn,
        statement,
        params,
        include_counters,
        media_type,
        limit,
    } = pending;
    conn.set_statement_limit(limit);
    // The session namespace is the statement's: the engine streams a
    // statement only when it has no `USE` clause (`streamable_part`), so
    // nothing re-routes it, and any statement with one lands and is
    // answered from its result header by the collected path.
    let namespace = conn.namespace();
    let mut gate = Gate {
        state: Some(State::Buffering {
            writer,
            rows: Vec::new(),
            bytes: 0,
        }),
        columns: Vec::new(),
        namespace,
        form: Form {
            media_type,
            namespace: Some(namespace),
        },
        head: Head {
            content_type: media_type.content_type(),
            extra_headers: &extra_headers,
            accept_encoding,
        },
        refused: None,
    };
    let outcome = conn.query_streaming(&statement, params, CancelToken::new(), |event| {
        gate.on_event(event)
    });
    let mut buffered = gate.finish(outcome, include_counters, media_type)?;
    if let Streamed::Buffered(reply) = &mut buffered {
        reply.extra_headers = extra_headers;
    }
    Ok(buffered)
}

/// The response head fields the chunked head is built from.
struct Head<'a> {
    content_type: &'static str,
    extra_headers: &'a [(String, String)],
    accept_encoding: Option<&'a str>,
}

/// Where the consumer's output goes: held until the framing is decided, then
/// into the frame writer — or nowhere, once the socket failed.
enum State<'a, W: Write> {
    Buffering {
        writer: &'a mut W,
        rows: Vec<Vec<Value>>,
        /// The v2 encoding's size of the rows held so far.
        bytes: usize,
    },
    Streaming {
        body: ChunkedBody<&'a mut W>,
        wrote_row: bool,
    },
    /// The socket (or its write timeout) failed: nothing more is written,
    /// and the connection is dropped with this error.
    Broken(io::Error),
}

/// Why a write stopped the statement.
enum Failure {
    /// The socket (or its write timeout) failed: the connection is dropped.
    Io(io::Error),
    /// A row the wire cannot carry: answered like an engine error.
    Refused(Error),
}

/// A serializer failure is classified at the boundary: a failed write is the
/// socket's ([`Failure::Io`]); anything else is a value the encoder refused,
/// answered as an engine error ([`Failure::Refused`]).
impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        if error.is_io() {
            Self::Io(error.into())
        } else {
            Self::Refused(Error::internal(format!(
                "the reply encoder refused a value: {error}"
            )))
        }
    }
}

/// The [`Gate::state`] invariant: only a push in progress holds it out.
const STATE_HELD: &str = "the reply state is held between deliveries";

struct Gate<'a, W: Write> {
    /// The reply's state, `Some` between deliveries: a batch takes it
    /// through the push (the framing switch consumes the writer) and puts
    /// back the state the push leaves.
    state: Option<State<'a, W>>,
    columns: Vec<String>,
    namespace: Namespace,
    form: Form,
    head: Head<'a>,
    /// A row the wire refused: the statement stops, and the reply answers
    /// it in the form its state allows (the error reply before the head,
    /// the in-band `errors` member after it).
    refused: Option<Error>,
}

impl<'a, W: Write> Gate<'a, W> {
    fn stopped(&self) -> bool {
        self.refused.is_some() || matches!(self.state, Some(State::Broken(_)))
    }

    fn on_event(&mut self, event: StreamEvent) -> relate::Result<BatchFlow> {
        if self.stopped() {
            return Ok(BatchFlow::Stop);
        }
        match event {
            StreamEvent::Columns(columns) => self.columns = columns,
            StreamEvent::Batch(rows) => {
                let state = self.state.take().expect(STATE_HELD);
                self.state = Some(self.push(state, rows));
            }
        }
        Ok(if self.stopped() {
            BatchFlow::Stop
        } else {
            BatchFlow::Continue
        })
    }

    /// Take one batch into `state` and return the state it leaves.
    fn push(&mut self, state: State<'a, W>, rows: Vec<Vec<Value>>) -> State<'a, W> {
        // An entity under a namespace with no element-id segment has no wire
        // form; the collected reply refuses it before it writes, the stream
        // refuses it before the batch.
        if let Err(error) = refuse_entities_without_element_ids(self.namespace, &rows) {
            return self.failed(state, Failure::Refused(error));
        }
        let form = self.form;
        match state {
            State::Buffering {
                writer,
                rows: mut held,
                bytes,
            } => {
                let bytes = match encoded_len(&rows, form) {
                    Ok(len) => bytes + len,
                    Err(failure) => {
                        let state = State::Buffering {
                            writer,
                            rows: held,
                            bytes,
                        };
                        return self.failed(state, failure);
                    }
                };
                held.extend(rows);
                if bytes < STREAM_THRESHOLD_BYTES {
                    return State::Buffering {
                        writer,
                        rows: held,
                        bytes,
                    };
                }
                self.switch(writer, &held)
            }
            State::Streaming {
                mut body,
                mut wrote_row,
            } => {
                let written = envelope::rows(&mut body, &mut wrote_row, &rows, form);
                self.settle(State::Streaming { body, wrote_row }, written)
            }
            broken @ State::Broken(_) => broken,
        }
    }

    /// The framing decision: the buffered rows reached the threshold, so the
    /// chunked head goes out with the envelope's opening and the rows held.
    fn switch(&mut self, writer: &'a mut W, rows: &[Vec<Value>]) -> State<'a, W> {
        let mut headers = self.head.extra_headers.to_vec();
        content_encoding::push_vary(&mut headers);
        let encoding = match write_chunked_head(
            writer,
            200,
            self.head.content_type,
            headers,
            self.head.accept_encoding,
        ) {
            Ok(encoding) => encoding,
            Err(error) => return State::Broken(error),
        };
        let mut body = ChunkedBody::new(writer, encoding);
        let mut wrote_row = false;
        let written = envelope::open(&mut body, &self.columns)
            .and_then(|()| envelope::rows(&mut body, &mut wrote_row, rows, self.form));
        self.settle(State::Streaming { body, wrote_row }, written)
    }

    /// The state a write's outcome leaves.
    fn settle(&mut self, state: State<'a, W>, written: serde_json::Result<()>) -> State<'a, W> {
        match written {
            Ok(()) => state,
            Err(error) => self.failed(state, error.into()),
        }
    }

    /// Record a failure against the state it happened in: a socket failure
    /// breaks the reply; a refused value keeps the state, whose form — the
    /// error reply, or the in-band member — answers it.
    fn failed(&mut self, state: State<'a, W>, failure: Failure) -> State<'a, W> {
        match failure {
            Failure::Io(error) => State::Broken(error),
            Failure::Refused(error) => {
                self.refused = Some(error);
                state
            }
        }
    }

    /// Settle the run: an answer that never streamed is the ordinary reply;
    /// a streamed one gets its tail (or its in-band error) and its last
    /// chunk; a broken one drops the connection.
    fn finish(
        self,
        outcome: relate::Result<QueryResult>,
        include_counters: bool,
        media_type: ResponseMediaType,
    ) -> io::Result<Streamed> {
        let result = match self.refused {
            Some(error) => Err(error),
            None => outcome,
        };
        match (self.state.expect(STATE_HELD), result) {
            (State::Broken(error), _) => Err(error),
            (State::Buffering { rows, .. }, Ok(mut result)) => {
                result.rows = rows;
                Ok(Streamed::Buffered(collected(
                    result,
                    include_counters,
                    media_type,
                )))
            }
            (State::Buffering { .. }, Err(error)) => Ok(Streamed::Buffered(run_error(&error))),
            (State::Streaming { mut body, .. }, result) => {
                let tail =
                    result.and_then(|result| success_tail(&result, include_counters, media_type));
                let members = match tail {
                    Ok(members) => members,
                    Err(error) => vec![errors_member(engine_error(&error).1)],
                };
                envelope::close(&mut body, &members).map_err(io::Error::from)?;
                body.finish()?;
                Ok(Streamed::Done)
            }
        }
    }
}

/// The v2 encoding's size of `rows`, counted as they are written.
fn encoded_len(rows: &[Vec<Value>], form: Form) -> Result<usize, Failure> {
    let mut count = ByteCount::default();
    for row in rows {
        serde_json::to_writer(&mut count, &RowView::new(row, form))?;
    }
    Ok(count.bytes())
}
