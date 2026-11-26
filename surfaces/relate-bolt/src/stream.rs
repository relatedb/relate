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
use std::sync::{Arc, Mutex};
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

/// How often a statement parked on its consumer checks its deadline.
const PARK_POLL: Duration = Duration::from_millis(100);

impl StreamHandle {
    /// Start `cypher` on a worker that owns `conn` until the statement ends.
    /// A consumer that waits longer than `idle` for a demand stops the
    /// statement (`None` waits indefinitely).
    ///
    /// The connection moves out of `lent` for the statement's lifetime; a
    /// failed worker spawn puts it back, so the session never loses it to
    /// a thread it could not start.
    pub(crate) fn start(
        lent: &mut Option<Connection>,
        cypher: String,
        params: BTreeMap<String, Value>,
        idle: Option<Duration>,
    ) -> std::io::Result<Self> {
        let conn = lent
            .take()
            .expect("the session owns its connection when a statement starts");
        // The slot keeps the connection reachable from here until the
        // worker thread runs: a closure that never starts drops with it.
        let slot = Arc::new(Mutex::new(Some(conn)));
        let worker_slot = Arc::clone(&slot);
        let (events_tx, events) = channel::<Event>();
        let (demand, demand_rx) = channel::<Demand>();
        let token = CancelToken::new();
        let worker_token = token.clone();
        let parked_token = token.clone();
        let worker = std::thread::Builder::new()
            .name("relate-bolt-stream".into())
            // The statement runs on this thread: it needs the engine's
            // query-worker stack, not the platform default (the engine
            // asserts it, and deep expressions recurse on it).
            .stack_size(relate::QUERY_WORKER_STACK_BYTES)
            .spawn(move || {
                let mut conn = worker_slot
                    .lock()
                    .expect("the connection slot is never poisoned: no holder panics")
                    .take()
                    .expect("the spawner leaves the connection in the slot");
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
                        // Parked on the consumer: the statement's deadline
                        // (ADR-0692) keeps running, so the wait polls the
                        // token and a spent budget ends the statement here
                        // instead of at the next PULL.
                        let parked = Instant::now();
                        let demanded = loop {
                            if parked_token.is_cancelled() {
                                return Err(relate::Error::Cancelled);
                            }
                            let slice = match idle {
                                Some(idle) => match idle.checked_sub(parked.elapsed()) {
                                    Some(left) => left.min(PARK_POLL),
                                    None => break Err(RecvTimeoutError::Timeout),
                                },
                                None => PARK_POLL,
                            };
                            match demand_rx.recv_timeout(slice) {
                                Err(RecvTimeoutError::Timeout) => {}
                                other => break other,
                            }
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
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(e) => {
                *lent = slot
                    .lock()
                    .expect("the connection slot is never poisoned: no holder panics")
                    .take();
                return Err(e);
            }
        };
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
    /// `keep_alive` the worker is still busy (ADR-0312). The statement's
    /// deadline is the engine's (ADR-0692): it answers its typed timeout as
    /// the `Done` result. The wait that returns `Done` also joins the
    /// worker.
    pub(crate) fn wait<W: Write>(
        &mut self,
        out: &mut W,
        keep_alive: Duration,
    ) -> std::io::Result<Event> {
        require_interval(keep_alive)?;
        let mut last_noop = Instant::now();
        loop {
            match self.events.recv_timeout(keep_alive) {
                Ok(event) => {
                    if matches!(event, Event::Done(_)) {
                        self.join();
                    }
                    return Ok(event);
                }
                Err(RecvTimeoutError::Timeout) => {
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
    /// `None` when the worker died without one (an engine panic): the
    /// connection died with it.
    pub(crate) fn abandon(mut self) -> Option<Connection> {
        self.token.cancel();
        self.demand = None;
        loop {
            match self.events.recv() {
                Ok(Event::Done(finished)) => {
                    self.join();
                    return Some(finished.conn);
                }
                Ok(Event::Columns(_) | Event::Batch(_)) => {}
                Err(_) => {
                    self.join();
                    return None;
                }
            }
        }
    }

    /// Reap the worker. A worker that panicked is already reported by the
    /// closed event channel (`wait` answers an io error, `abandon` no
    /// connection), so the panic payload is dropped here: `join` runs in
    /// `Drop`, where a second panic would abort the process.
    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
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
