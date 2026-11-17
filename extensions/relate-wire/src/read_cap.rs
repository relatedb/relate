//! The one bounded read shared by every whole-source reader, and the
//! per-record flavor a still-open stream reads through.
//!
//! `--fetch-params` sources (ADR-0267), `fs-to-graph` file content
//! (ADR-0157), `git-to-graph`'s `git log` stdout (ADR-0377), the extension
//! tarball's decompressed stream (ADR-0431 §2, the unpacker's bomb cap)
//! and the remote `--database` bundle (LABS-872) each have their own cap,
//! wording and overflow policy, but the capped read itself is two forms of
//! one refusal rule: the buffered [`read_capped`] (a `Vec`-sink adapter
//! over the chunked twin) and its chunked streaming form
//! [`read_capped_into`] (one fixed buffer under a `Read::take` of
//! `limit + 1`, so an oversized source costs one byte past the cap, never
//! a chunk) — a hostile or growing source cannot allocate past the cap
//! before the refusal, buffered or staged to a sink. The helpers report
//! the overflow; each caller maps it to its own error or `None`.
//!
//! [`read_line_capped`] is the same mechanism one line at a time — a
//! `BufRead::take` of `limit + 1` bytes around one `read_until` — for a
//! source that is still open after the line: the extension mount's host
//! reads its JSON-RPC stdout one line at a time (ADR-0442 §3), so a
//! hostile line cannot allocate past the line cap before the refusal.
//!
//! [`DeadlineReader`] is the same record read moved onto its own thread, so
//! its owner can bound each read with a deadline: a child that holds its
//! pipes and writes nothing stalls a read, never the host. The mount's
//! session loop and the `check` verb's staged session run every request
//! through it (ADR-0442's deferred read deadline). Its one-slot channel is
//! the pipe's own backpressure: the reader thread blocks until the host
//! consumes the line it already read, so a host doing store work never lets
//! the child's output pile up unbounded ahead of it.
//!
//! [`write_line`] is its writing twin: one payload, one terminator, one
//! flush — the shape every line the extension wires write shares, so the
//! envelope write, the host's responses and the child's requests cannot
//! each hand-roll it.
//!
//! [`drain_stderr`] is the bounded drain for the stderr of a child whose
//! stdout another thread reads capped: the whole stream is consumed (the
//! child never blocks on a full stderr pipe) while at most a stated number
//! of bytes is retained. `git-to-graph`'s `git log` (ADR-0377) and the
//! mount's children (ADR-0442) share it.

use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

/// The outcome of [`read_capped`].
#[derive(Debug, PartialEq, Eq)]
pub enum CappedRead {
    /// The source ended at or below the cap, and is fully buffered.
    Complete(Vec<u8>),
    /// The source held the byte that would make the buffer `limit + 1`; at
    /// most `limit` bytes are buffered and the rest is unread.
    Exceeded,
}

/// Read `reader` to end under a `limit`-byte cap — the buffered form, a
/// thin adapter over [`read_capped_into`] with a `Vec` sink, so the two
/// forms give the identical accept/refuse decision on every input.
///
/// The read stops at `limit + 1`, so an oversized source costs one byte past
/// the cap instead of the whole stream, and `limit == 0` accepts only an
/// empty source. I/O errors propagate unchanged: only the caller knows what
/// its source is called and which wording its failures use.
pub fn read_capped(reader: impl Read, limit: u64) -> std::io::Result<CappedRead> {
    let mut buffer = Vec::new();
    match read_capped_into(reader, &mut buffer, limit) {
        CappedCopy::Complete(_) => Ok(CappedRead::Complete(buffer)),
        CappedCopy::Exceeded => Ok(CappedRead::Exceeded),
        CappedCopy::Source(e) | CappedCopy::Sink(e) => Err(e),
    }
}

/// The outcome of [`read_capped_into`], the streaming twin of
/// [`CappedRead`]: the copy's byte count, the cap refusal, or which side of
/// the copy failed — the caller knows what its source and sink are called
/// and which wording each failure uses.
#[derive(Debug)]
pub enum CappedCopy {
    /// The source ended at or below the cap; the count is what landed in
    /// the sink.
    Complete(u64),
    /// The source held the byte that would make the copy `limit + 1`; at
    /// most `limit` bytes were written and the rest is unread.
    Exceeded,
    /// Reading the source failed.
    Source(std::io::Error),
    /// Writing the sink failed.
    Sink(std::io::Error),
}

/// Copy `reader` into `sink` to end under a `limit`-byte cap —
/// [`read_capped`]'s streaming form, for a caller that stages the body to a
/// file instead of buffering it: one fixed-size buffer that never reads
/// past the `limit + 1`'th byte, and the cap is checked before a chunk is
/// written, so the sink never holds more than `limit` bytes and memory
/// stays bounded by the buffer rather than by the source. Interrupted reads
/// are retried (as `read_to_end` does); every other failure is reported as
/// a fact and the caller words it.
pub fn read_capped_into(reader: impl Read, sink: &mut impl Write, limit: u64) -> CappedCopy {
    // `take` is the bound itself, the same `limit + 1` the buffered form's
    // adapter rides: an oversized source costs one byte past the cap, never
    // a whole chunk, with no per-iteration bound arithmetic to overflow.
    // The `+ 1` saturates because `u64::MAX` is in `limit`'s domain — an
    // unlimited cap is a legal input to both forms, and the kit keeps one
    // edge policy for them. `Take` only spends its limit on successful
    // reads, so the Interrupted retry below stays inside it.
    let mut reader = reader.take(limit.saturating_add(1));
    let mut buffer = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return CappedCopy::Complete(written),
            Ok(read) => read,
            // `read_to_end`'s own contract: an interrupted read is retried,
            // not surfaced — the buffered adapter inherits it for free.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return CappedCopy::Source(e),
        };
        written += read as u64;
        if written > limit {
            return CappedCopy::Exceeded;
        }
        if let Err(e) = sink.write_all(&buffer[..read]) {
            return CappedCopy::Sink(e);
        }
    }
}

/// The outcome of [`read_line_capped`].
#[derive(Debug, PartialEq, Eq)]
pub enum CappedLine {
    /// The next line, its `\n` terminator included, at or below the cap.
    /// A final line the source ended without terminating is carried here
    /// too — the terminator is optional only at the end of the stream.
    Line(Vec<u8>),
    /// The source ended before any byte of the next line: no record.
    Eof,
    /// The source held the byte that would make the line `limit + 1` bytes
    /// without a terminator; at most `limit` bytes are buffered and the
    /// rest is unread. The terminator itself is free: a line of `limit`
    /// content bytes plus its `\n` reads `limit + 1` bytes and stays a
    /// `Line`, so the cap bounds the record's own bytes.
    Exceeded,
}

/// Read the reader's next line under a `limit`-byte cap, [`read_capped`]'s
/// mechanism applied to one line of a still-open stream: a
/// `BufRead::take` of `limit + 1` around one `read_until`, so a hostile or
/// growing line cannot allocate past the cap before the refusal. The cap
/// bounds the line's bytes excluding a terminating `\n`; a line that reads
/// `limit + 1` bytes with no terminator yet is `Exceeded`. I/O errors
/// propagate unchanged: only the caller knows what its source is called
/// and which wording its failures use.
///
/// The extension mount reads a stream line by line through this
/// (ADR-0442 §3), from the `run` mount's command path down — the host's
/// `extensions::protocol` reader.
pub fn read_line_capped(reader: &mut impl BufRead, limit: u64) -> std::io::Result<CappedLine> {
    let mut buffer = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_until(b'\n', &mut buffer)?;
    Ok(match buffer.last() {
        None => CappedLine::Eof,
        Some(b'\n') => CappedLine::Line(buffer),
        _ if buffer.len() as u64 > limit => CappedLine::Exceeded,
        _ => CappedLine::Line(buffer),
    })
}

/// One delivery from a [`DeadlineReader`]: [`read_line_capped`]'s outcome,
/// plus the I/O failure the reader thread met. The thread stops on the first
/// non-[`Line`](Self::Line) arrival — the stream's end, a record over the
/// cap and an I/O failure all end a session — so such an arrival is the last
/// the channel ever carries.
#[derive(Debug, PartialEq, Eq)]
pub enum LineArrival {
    /// One line, its terminator included (see [`read_line_capped`]).
    Line(Vec<u8>),
    /// The line exceeded the cap.
    Exceeded,
    /// The stream ended before the next line.
    Eof,
    /// Reading the stream failed; the I/O refusal's text.
    Failed(String),
}

/// A [`read_line_capped`] stream read on its own thread, so its owner can
/// bound every read with a deadline: [`Self::next`] waits at most the
/// caller's deadline and reports the stall instead of blocking on the pipe.
///
/// The one mechanism behind both the mount's session loop and the `check`
/// verb's staged session (ADR-0442's deferred read deadline). The thread
/// ends with the stream — a closed receiver (the session is over) or a
/// terminal [`LineArrival`] stops it — and is detached, so a session that
/// fails on the deadline kills its child without joining a blocked read.
///
/// The channel holds one arrival: the reader thread blocks on the second
/// send until the host consumes the first, restoring the pipe's backpressure
/// (a host busy in the store does not let the child run arbitrarily far
/// ahead). It cannot deadlock the session — the host consumes an arrival
/// before it does anything that would wait for another one.
pub struct DeadlineReader {
    arrivals: mpsc::Receiver<LineArrival>,
    /// The reader thread, held as ownership rather than as a join: a read
    /// blocked on a killed child's pipe ends on EOF, and the session never
    /// waits for it.
    _thread: std::thread::JoinHandle<()>,
}

impl DeadlineReader {
    /// Start reading `reader` under `cap`, one line per arrival.
    pub fn spawn(mut reader: impl BufRead + Send + 'static, cap: u64) -> Self {
        let (sender, arrivals) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || loop {
            let arrival = match read_line_capped(&mut reader, cap) {
                Ok(CappedLine::Line(line)) => LineArrival::Line(line),
                Ok(CappedLine::Exceeded) => LineArrival::Exceeded,
                Ok(CappedLine::Eof) => LineArrival::Eof,
                Err(error) => LineArrival::Failed(error.to_string()),
            };
            let terminal = !matches!(arrival, LineArrival::Line(_));
            // A closed receiver means the session is over; the thread goes
            // with it, as it does after a terminal arrival.
            if sender.send(arrival).is_err() || terminal {
                return;
            }
        });
        Self {
            arrivals,
            _thread: thread,
        }
    }

    /// The next arrival, waiting at most `deadline`: `None` while the source
    /// is silent, and [`LineArrival::Eof`] when the stream ended (the reader
    /// thread exits without sending).
    pub fn next(&self, deadline: Duration) -> Option<LineArrival> {
        match self.arrivals.recv_timeout(deadline) {
            Ok(arrival) => Some(arrival),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(LineArrival::Eof),
        }
    }

    /// The next arrival, waiting as long as the source takes — the mount's
    /// read once the handshake is established, where a working child may
    /// legitimately spend an unbounded time between calls. [`LineArrival::Eof`]
    /// when the stream ended.
    pub fn next_blocked(&self) -> LineArrival {
        match self.arrivals.recv() {
            Ok(arrival) => arrival,
            Err(_) => LineArrival::Eof,
        }
    }
}

/// Write `payload` as one line: its bytes, the `\n` terminator, and a
/// flush — [`read_line_capped`]'s writing twin. The caller maps the I/O
/// refusal into its own vocabulary, like every helper here.
pub fn write_line(out: &mut impl std::io::Write, payload: &str) -> std::io::Result<()> {
    out.write_all(payload.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

/// The most child stderr bytes [`drain_stderr`] retains for an error
/// message, shared by every spawned-child reader — `git-to-graph`'s
/// `git log` (ADR-0377) and the mount's children (ADR-0442) — so
/// no caller grows a bound of its own. The one cap the drain runs under:
/// [`drain_stderr`] takes no cap of its own, so the truncation marker it
/// derives from this constant always fits the cap that produced it.
pub const STDERR_DRAIN_CAP: usize = 64 * 1024;

/// The marker appended when [`drain_stderr`] discarded stderr bytes past
/// its cap: derived from the cap and the caller's noun, so the stated
/// number and the enforced one cannot drift apart. The marker is a few
/// dozen bytes against the 64 KiB cap — it fits on the whole domain the
/// drain runs under, so a truncated drain's retained message (marker
/// included) never exceeds the cap.
pub fn stderr_truncated_marker(whose: &str) -> String {
    format!("\n({whose} stderr truncated at {STDERR_DRAIN_CAP} bytes)")
}

/// Read `reader` to EOF, retaining at most [`STDERR_DRAIN_CAP`] bytes —
/// the stderr side of a child whose stdout another thread reads capped.
///
/// The whole stream is consumed, so the child never blocks on a full stderr
/// pipe while its stdout is still arriving; bytes past the cap are discarded
/// and the retained message ends with the [`stderr_truncated_marker`], which
/// keeps the total (marker included) within the cap. I/O errors propagate
/// unchanged, like every helper here: only the caller knows its wording.
pub fn drain_stderr(mut reader: impl Read, whose: &str) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    // One byte past the cap tells "exactly at the cap" from "over it"; the
    // rest of the stream is drained unread so the child never blocks on a
    // full pipe.
    (&mut reader)
        .take(STDERR_DRAIN_CAP as u64 + 1)
        .read_to_end(&mut retained)?;
    let truncated = retained.len() > STDERR_DRAIN_CAP;
    if truncated {
        std::io::copy(&mut reader, &mut std::io::sink())?;
        let marker = stderr_truncated_marker(whose);
        retained.truncate(STDERR_DRAIN_CAP.saturating_sub(marker.len()));
        retained.extend(marker.as_bytes());
    }
    Ok(retained)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    use super::*;

    #[test]
    fn reads_a_source_at_the_cap_whole() {
        assert_eq!(
            read_capped(Cursor::new(vec![b'x'; 8]), 8).unwrap(),
            CappedRead::Complete(vec![b'x'; 8])
        );
    }

    #[test]
    fn reports_the_byte_past_the_cap() {
        assert_eq!(
            read_capped(Cursor::new(vec![b'x'; 9]), 8).unwrap(),
            CappedRead::Exceeded
        );
    }

    /// The streaming copy keeps the refusal-cost guarantee the buffered
    /// form set: an oversized source costs one byte past the cap, never a
    /// whole chunk — the counting reader (`CountingReader` below) makes the
    /// cost observable.
    #[test]
    fn the_streaming_refusal_costs_one_byte_past_the_cap() {
        // The source is deliberately far larger than one 64 KiB chunk AND
        // larger than the cap: a take-bounded read stops at the cap + 1
        // byte, an unbounded one would pull a whole chunk past the cap.
        let consumed = Arc::new(AtomicUsize::new(0));
        let source = CountingReader {
            inner: Cursor::new(vec![b'x'; 70_000]),
            consumed: Arc::clone(&consumed),
        };
        let mut sink = Vec::new();
        assert!(matches!(
            read_capped_into(source, &mut sink, 8),
            CappedCopy::Exceeded
        ));
        assert_eq!(
            consumed.load(Ordering::Relaxed),
            9,
            "the cap plus the one refusal byte"
        );
        // The chunk that crosses the cap never lands: with this fixture the
        // whole 9-byte read was the crossing chunk, so nothing is written.
        assert!(sink.is_empty(), "the crossing chunk never lands: {sink:?}");

        // ... and a source at the cap reads whole.
        let mut sink = Vec::new();
        assert!(matches!(
            read_capped_into(Cursor::new(vec![b'x'; 8]), &mut sink, 8),
            CappedCopy::Complete(8)
        ));
        assert_eq!(sink, vec![b'x'; 8]);
    }

    /// A reader that answers `Interrupted` once before its bytes, pinning
    /// the retry the buffered `read_to_end` form always had.
    struct InterruptingReader {
        source: Cursor<Vec<u8>>,
        interrupted: bool,
    }

    impl Read for InterruptingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "a signal interrupted the read",
                ));
            }
            self.source.read(buf)
        }
    }

    /// A `u64::MAX` cap is in `limit`'s domain and must not overflow the
    /// take bound's `+ 1` (a debug build panics on the plain arithmetic) —
    /// this test pins the kit's one edge policy, shared by both forms.
    #[test]
    fn a_max_shaped_cap_does_not_overflow_the_bound() {
        let mut sink = Vec::new();
        assert!(matches!(
            read_capped_into(Cursor::new(vec![b'x'; 200_000]), &mut sink, u64::MAX),
            CappedCopy::Complete(200_000)
        ));
        assert_eq!(sink.len(), 200_000);
    }

    #[test]
    fn an_interrupted_read_is_retried_not_surfaced() {
        let mut sink = Vec::new();
        assert!(matches!(
            read_capped_into(
                InterruptingReader {
                    source: Cursor::new(vec![b'x'; 4]),
                    interrupted: false,
                },
                &mut sink,
                8
            ),
            CappedCopy::Complete(4)
        ));
        assert_eq!(sink, vec![b'x'; 4]);
    }

    #[test]
    fn a_zero_cap_accepts_only_an_empty_source() {
        assert_eq!(
            read_capped(Cursor::new(Vec::new()), 0).unwrap(),
            CappedRead::Complete(Vec::new())
        );
        assert_eq!(
            read_capped(Cursor::new(b"x".to_vec()), 0).unwrap(),
            CappedRead::Exceeded
        );
    }

    /// A reader that yields one byte and then fails, to prove I/O errors are
    /// the caller's to word.
    struct Failing;

    impl Read for Failing {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk went away"))
        }
    }

    #[test]
    fn propagates_io_errors_unchanged() {
        let err = read_capped(Failing, 8).unwrap_err();
        assert_eq!(err.to_string(), "disk went away");
    }

    #[test]
    fn reads_a_line_at_the_cap_whole() {
        // `limit` content bytes plus the terminator read `limit + 1` bytes
        // and stay a Line: the terminator is free.
        let mut at_cap = vec![b'x'; 8];
        at_cap.push(b'\n');
        let mut source = std::io::Cursor::new(at_cap.clone());
        assert_eq!(
            read_line_capped(&mut source, 8).unwrap(),
            CappedLine::Line(at_cap)
        );
        // The stream behind the terminator is unread.
        assert_eq!(source.position(), 9);
    }

    #[test]
    fn a_line_reports_the_byte_past_the_cap() {
        let mut source = std::io::Cursor::new(vec![b'x'; 9]);
        assert_eq!(
            read_line_capped(&mut source, 8).unwrap(),
            CappedLine::Exceeded
        );
    }

    #[test]
    fn a_source_that_ended_before_the_next_line_is_eof() {
        let mut source = std::io::Cursor::new(Vec::new());
        assert_eq!(read_line_capped(&mut source, 8).unwrap(), CappedLine::Eof);
        let mut exhausted = std::io::Cursor::new(b"line\n".to_vec());
        assert_eq!(
            read_line_capped(&mut exhausted, 8).unwrap(),
            CappedLine::Line(b"line\n".to_vec())
        );
        assert_eq!(
            read_line_capped(&mut exhausted, 8).unwrap(),
            CappedLine::Eof
        );
    }

    #[test]
    fn a_final_line_the_source_never_terminated_is_a_line() {
        let mut source = std::io::Cursor::new(b"tail".to_vec());
        assert_eq!(
            read_line_capped(&mut source, 8).unwrap(),
            CappedLine::Line(b"tail".to_vec())
        );
        // ... but a terminator-less run past the cap refuses.
        let mut source = std::io::Cursor::new(vec![b'x'; 9]);
        assert_eq!(
            read_line_capped(&mut source, 8).unwrap(),
            CappedLine::Exceeded
        );
    }

    #[test]
    fn streams_one_line_per_call() {
        let mut source = std::io::Cursor::new(b"{\"a\":1}\n{\"a\":2}\n".to_vec());
        assert_eq!(
            read_line_capped(&mut source, 8).unwrap(),
            CappedLine::Line(b"{\"a\":1}\n".to_vec())
        );
        assert_eq!(
            read_line_capped(&mut source, 8).unwrap(),
            CappedLine::Line(b"{\"a\":2}\n".to_vec())
        );
        assert_eq!(read_line_capped(&mut source, 8).unwrap(), CappedLine::Eof);
    }

    #[test]
    fn the_deadline_reader_streams_lines_then_ends() {
        let lines = DeadlineReader::spawn(std::io::Cursor::new(b"one\ntwo\n".to_vec()), 8);
        assert_eq!(
            lines.next(Duration::from_secs(5)),
            Some(LineArrival::Line(b"one\n".to_vec()))
        );
        assert_eq!(
            lines.next(Duration::from_secs(5)),
            Some(LineArrival::Line(b"two\n".to_vec()))
        );
        // The stream's end is the last arrival; the thread is gone.
        assert_eq!(lines.next(Duration::from_secs(5)), Some(LineArrival::Eof));
        assert_eq!(lines.next(Duration::from_secs(5)), Some(LineArrival::Eof));
    }

    #[test]
    fn the_deadline_reader_reports_a_line_over_the_cap() {
        let lines = DeadlineReader::spawn(std::io::Cursor::new(vec![b'x'; 9]), 8);
        assert_eq!(
            lines.next(Duration::from_secs(5)),
            Some(LineArrival::Exceeded)
        );
    }

    #[test]
    fn a_silent_source_is_the_deadline_not_a_hang() {
        // An open pipe nobody writes to: the read blocks, the owner's
        // deadline reports the stall. Holding the write end keeps the
        // source open past the deadline.
        let (reader, _writer) = std::io::pipe().expect("a test pipe opens");
        let lines = DeadlineReader::spawn(std::io::BufReader::new(reader), 8);
        assert_eq!(lines.next(Duration::from_millis(50)), None);
    }

    /// A reader that counts every byte handed to its caller, so a test can
    /// observe exactly what a bounded read consumed — the drain's
    /// read-to-EOF proof and the streaming refusal's one-byte-past-the-cap
    /// cost.
    struct CountingReader<R> {
        inner: R,
        consumed: Arc<AtomicUsize>,
    }

    impl<R: Read> Read for CountingReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.consumed.fetch_add(read, Ordering::Relaxed);
            Ok(read)
        }
    }

    /// Drain `reader` with a deadline, returning the retained bytes and the
    /// byte count the reader handed out.
    fn drain_with_deadline(reader: impl Read + Send + 'static) -> (Vec<u8>, usize) {
        let consumed = Arc::new(AtomicUsize::new(0));
        let counted = CountingReader {
            inner: reader,
            consumed: Arc::clone(&consumed),
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let retained = drain_stderr(counted, "test").expect("a test reader never fails");
            sender.send(retained).expect("the test receives the drain");
        });
        let retained = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the stderr drain must reach EOF instead of hanging");
        (retained, consumed.load(Ordering::Relaxed))
    }

    #[test]
    fn stderr_drain_bounds_memory_but_reads_to_eof() {
        let marker = stderr_truncated_marker("test");
        // An endless source: stopping at the cap would leave it unread.
        let endless = std::io::repeat(b'x').take(8 * STDERR_DRAIN_CAP as u64);
        let (retained, consumed) = drain_with_deadline(endless);
        assert!(
            retained.len() <= STDERR_DRAIN_CAP,
            "{} bytes retained",
            retained.len()
        );
        assert!(
            retained.ends_with(marker.as_bytes()),
            "a truncated drain marks the cut: {:?}",
            String::from_utf8_lossy(&retained[retained.len().saturating_sub(64)..])
        );
        assert_eq!(consumed, 8 * STDERR_DRAIN_CAP, "the drain must read to EOF");

        // Exactly the byte that crosses the cap.
        let (retained, consumed) =
            drain_with_deadline(std::io::Cursor::new(vec![b'y'; STDERR_DRAIN_CAP + 1]));
        assert!(retained.len() <= STDERR_DRAIN_CAP);
        assert!(retained.ends_with(marker.as_bytes()));
        assert_eq!(consumed, STDERR_DRAIN_CAP + 1);

        // Below the cap: retained verbatim, no marker.
        let (retained, consumed) = drain_with_deadline(std::io::Cursor::new(
            b"fatal: not a git repository".to_vec(),
        ));
        assert_eq!(retained, b"fatal: not a git repository");
        assert_eq!(consumed, retained.len());
    }

    #[test]
    fn the_truncation_marker_states_the_cap_it_enforces() {
        // The marker is derived from the cap and the caller's noun: the
        // number it states is the number the drain enforces, not a second
        // hand-synced literal.
        assert_eq!(
            stderr_truncated_marker("git"),
            format!("\n(git stderr truncated at {STDERR_DRAIN_CAP} bytes)")
        );
        assert_eq!(
            stderr_truncated_marker("extension"),
            "\n(extension stderr truncated at 65536 bytes)"
        );
        // The marker fits its cap on the whole domain the drain runs under:
        // the one cap is the constant's, so a truncated drain's retained
        // message — the marker included — never exceeds the cap.
        assert!(stderr_truncated_marker("extension").len() < STDERR_DRAIN_CAP);
    }

    #[test]
    fn a_drain_io_error_propagates_unchanged() {
        let err = drain_stderr(Failing, "test").unwrap_err();
        assert_eq!(err.to_string(), "disk went away");
    }

    /// The mount's cap facts both sides state through this crate (ADR-0442
    /// §3, ADR-0447): the pinned numbers of the contract, asserted here so a
    /// bump in either direction is a deliberate protocol change, never drift.
    #[test]
    fn the_mount_caps_are_the_pinned_contract() {
        assert_eq!(crate::MAX_STATEMENT_BYTES, 8 * 1024 * 1024);
        assert_eq!(crate::MAX_ROWS_PER_STATEMENT, 1000);
        assert_eq!(crate::MAX_STATEMENTS, 4096);
        assert_eq!(crate::MAX_STREAM_BYTES, 128 * 1024 * 1024);
    }
}
