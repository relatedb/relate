//! A streamed RUN's worker (ADR-0682): the statement runs on its own thread
//! through `Connection::query_streaming`, which owns the connection for the
//! statement's lifetime, and hands each bounded batch of rows to the session
//! thread over a channel. The worker's consumer then waits — for the PULL
//! that wants the next batch, a DISCARD that skips the rest, or the stop
//! that abandons the statement — so the result never exists whole in memory,
//! whatever its row count. The session thread keeps the wire.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use relate::{BatchFlow, CancelToken, Connection, Executed, StreamEvent, Value};

use crate::keepalive::NOOP;

/// What the worker tells the session thread.
pub(crate) enum Event {
    /// The result columns, once, ahead of the first batch.
    Columns(Vec<String>),
    /// One bounded batch of rows. The worker waits for a [`Demand`] before
    /// producing the next.
    Batch(Vec<Vec<Value>>),
    /// The statement ended; always the last event, carrying the connection
    /// back.
    Done(Box<Finished>),
}

/// How a streamed statement ended.
pub(crate) struct Finished {
    /// The engine connection, returned to the session.
    pub(crate) conn: Connection,
    /// How the statement ended (ADR-0319): `Err` failed before executing,
    /// `Executed::Failed` while executing, `Executed::Rows` is the summary
    /// with its rows empty.
    pub(crate) result: relate::Result<Executed>,
    /// The worker stopped the statement because no demand arrived within
    /// the idle timeout; `result` is then the cancellation.
    pub(crate) stalled: bool,
}

/// What the session thread tells a worker waiting after a batch.
enum Demand {
    /// Produce the next batch.
    Resume,
    /// Run the rest of the statement without delivering rows (DISCARD).
    Skip,
}

/// The session's end of one streamed statement. Dropping it stops the
/// statement and joins the worker.
pub(crate) struct StreamHandle {
    events: Receiver<Event>,
    demand: Option<Sender<Demand>>,
    token: CancelToken,
    worker: Option<JoinHandle<()>>,
}

impl StreamHandle {
    /// Start `cypher` on a worker that owns `conn` until the statement ends.
    /// A consumer that waits longer than `idle` for a demand stops the
    /// statement (`None` waits indefinitely).
    pub(crate) fn start(
        conn: Connection,
        cypher: String,
        params: BTreeMap<String, Value>,
        idle: Option<Duration>,
    ) -> std::io::Result<Self> {
        let (events_tx, events) = channel::<Event>();
        let (demand, demand_rx) = channel::<Demand>();
        let token = CancelToken::new();
        let worker_token = token.clone();
        let worker = std::thread::Builder::new()
            .name("relate-bolt-stream".into())
            // The statement runs on this thread: it needs the engine's
            // query-worker stack, not the platform default (the engine
            // asserts it, and deep expressions recurse on it).
            .stack_size(relate::QUERY_WORKER_STACK_BYTES)
            .spawn(move || {
                let mut conn = conn;
                let stalled = Arc::new(AtomicBool::new(false));
                let consumer_stalled = Arc::clone(&stalled);
                let mut skipping = false;
                let batches = events_tx.clone();
                let result =
                    conn.query_streaming_staged(&cypher, params, worker_token, move |event| {
                        let rows = match event {
                            StreamEvent::Columns(columns) => {
                                return Ok(flow(batches.send(Event::Columns(columns)).is_ok()));
                            }
                            StreamEvent::Batch(rows) => rows,
                        };
                        if skipping {
                            return Ok(BatchFlow::Continue);
                        }
                        if batches.send(Event::Batch(rows)).is_err() {
                            return Ok(BatchFlow::Stop);
                        }
                        let demanded = match idle {
                            Some(idle) => demand_rx.recv_timeout(idle),
                            None => demand_rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                        };
                        match demanded {
                            Ok(Demand::Resume) => Ok(BatchFlow::Continue),
                            Ok(Demand::Skip) => {
                                skipping = true;
                                Ok(BatchFlow::Continue)
                            }
                            Err(RecvTimeoutError::Timeout) => {
                                consumer_stalled.store(true, Ordering::Relaxed);
                                Ok(BatchFlow::Stop)
                            }
                            // The session dropped its end: the statement is
                            // abandoned.
                            Err(RecvTimeoutError::Disconnected) => Ok(BatchFlow::Stop),
                        }
                    });
                // A dropped receiver means the session is gone; the
                // connection goes with the message.
                let _ = events_tx.send(Event::Done(Box::new(Finished {
                    conn,
                    result,
                    stalled: stalled.load(Ordering::Relaxed),
                })));
            })?;
        Ok(Self {
            events,
            demand: Some(demand),
            token,
            worker: Some(worker),
        })
    }

    /// Let the worker produce the next batch.
    pub(crate) fn resume(&self) {
        self.send(Demand::Resume);
    }

    /// Let the worker run the statement to its end without delivering rows.
    pub(crate) fn skip(&self) {
        self.send(Demand::Skip);
    }

    fn send(&self, demand: Demand) {
        if let Some(demand_tx) = &self.demand {
            // A worker that already ended (idle timeout, failure) has no
            // receiver; its `Done` is the next event.
            let _ = demand_tx.send(demand);
        }
    }

    /// The next worker event, ticking a NOOP onto `out` each
    /// `keep_alive` the worker is still busy (ADR-0312) and cancelling the
    /// statement once one wait outlasts `query_timeout` (ADR-0110,
    /// ADR-0111; the engine then answers its typed cancellation as the
    /// `Done` result). The wait that returns `Done` also joins the worker.
    pub(crate) fn wait<W: Write>(
        &mut self,
        out: &mut W,
        keep_alive: Duration,
        query_timeout: Option<Duration>,
    ) -> std::io::Result<Event> {
        require_interval(keep_alive)?;
        let started = Instant::now();
        let mut last_noop = started;
        let mut cancelled = false;
        loop {
            let tick = match query_timeout.filter(|_| !cancelled) {
                Some(limit) => keep_alive.min(limit.saturating_sub(started.elapsed())),
                None => keep_alive,
            };
            match self.events.recv_timeout(tick.max(Duration::from_millis(1))) {
                Ok(event) => {
                    if matches!(event, Event::Done(_)) {
                        self.join();
                    }
                    return Ok(event);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if query_timeout.is_some_and(|limit| started.elapsed() >= limit) && !cancelled {
                        self.token.cancel();
                        cancelled = true;
                    }
                    if last_noop.elapsed() >= keep_alive {
                        out.write_all(&NOOP).and_then(|()| out.flush())?;
                        last_noop = Instant::now();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(std::io::Error::other(
                        "stream worker ended without a result",
                    ))
                }
            }
        }
    }

    /// Stop the statement and take the connection back: cancel it, drop the
    /// demand end (a waiting consumer stops), drain to the worker's `Done`.
    pub(crate) fn abandon(mut self) -> Connection {
        self.token.cancel();
        self.demand = None;
        loop {
            match self
                .events
                .recv()
                .expect("a stream worker always ends with its Done event")
            {
                Event::Done(finished) => {
                    self.join();
                    return finished.conn;
                }
                Event::Columns(_) | Event::Batch(_) => {}
            }
        }
    }

    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.join().expect("a stream worker cannot panic");
        }
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        // The session is going away with the statement unfinished: stop it
        // and reap the worker; its connection drops with the final event.
        self.token.cancel();
        self.demand = None;
        self.join();
    }
}

/// A zero keep-alive interval is rejected before any engine call
/// (ADR-0334), as `Heartbeat::every` rejects it on the buffered path.
pub(crate) fn require_interval(keep_alive: Duration) -> std::io::Result<()> {
    if keep_alive.is_zero() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "keep-alive interval must be non-zero",
        ));
    }
    Ok(())
}

fn flow(delivered: bool) -> BatchFlow {
    if delivered {
        BatchFlow::Continue
    } else {
        BatchFlow::Stop
    }
}

/// One streamed result's open state (ADR-0682): the batch the session is
/// serving PULLs from and the worker still producing the rest.
pub(crate) struct StreamBody {
    /// The live worker; `None` once the statement ended and its connection
    /// is back with the session.
    pub(crate) handle: Option<StreamHandle>,
    /// The batch being served, and the next row of it to send.
    pub(crate) rows: Vec<Vec<Value>>,
    pub(crate) cursor: usize,
    /// The namespace the rows encode under (ADR-0484).
    pub(crate) namespace: relate::Namespace,
    /// The statement summary, once the worker ended successfully.
    pub(crate) finished: Option<Box<relate::QueryResult>>,
}
