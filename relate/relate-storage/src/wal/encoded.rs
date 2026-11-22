//! The one encoding of a commit record's redo ops (ADR-0094, as amended by
//! the encode-once ADR): the `ops` array is serialized exactly once, at
//! admission and off the serial path, into a buffer that already reserves
//! the record's header slack. The group leader then writes the header
//! (`lsn`, `commit_ts`) into that slack in place, so the frame bytes are
//! the record the single-pass serde encoder produced before — without a
//! second or third serialization of the ops.

use std::io::{self, Write};

use relate_core::tokens::WorkspaceCharge;
use relate_core::{CapacityKind, Error, Result};
use relate_engine_api::{CommitRecord, RedoOp};
use web_time::Instant;

use crate::vfs::{BoundedSink, Refusal};

use super::validate_redo_ids;

/// Records whose buffer is no larger than this are trimmed to their
/// length after encoding (see [`EncodedOps::encode`]).
const SMALL_RECORD_BYTES: usize = 64 * 1024;

/// Largest header the record can carry — the slack in front of the ops
/// body, so every real header fits and the admission figure (slack + body +
/// closing brace) is a safe upper bound of the final payload. The spellings
/// and width belong to the record type, [`CommitRecord`].
const HEADER_MAX: usize = CommitRecord::HEADER_MAX;

/// A commit's redo ops, encoded once, with room for the record header in
/// front. Owned by the commit slot from admission to append, then by the
/// [`crate::WalPreflight`] proof, whose frame writes [`Self::payload`].
#[derive(Debug)]
pub struct EncodedOps {
    /// `HEADER_MAX` bytes of header slack, then the ops array, then the
    /// record's closing brace.
    buf: Vec<u8>,
    /// First payload byte: `HEADER_MAX` until a header is stamped in.
    start: usize,
    /// Whether the ops include a store-level ontology op (ADR-0120 §3),
    /// decided with the ops in hand so the append applies the §4 rotation
    /// rule without decoding the payload.
    ontology_op: bool,
    /// Time spent encoding, folded into the append's `encode_write_ns`.
    encode_ns: u64,
}

impl EncodedOps {
    /// Encode `ops` once, bounded by the per-record payload cap measured
    /// on the whole prospective record (maximum-width header included), so
    /// the refusal — [`Error::Capacity`] in [`CapacityKind::WalPayload`]
    /// (ADR-0494) — and its figures are exactly those of encoding the
    /// record with maximum-width LSN and timestamp. `cap` is the store's
    /// resolved payload cap (ADR-0650) — the commit pipeline and the write
    /// engine pass the resolved value; an ad-hoc encoding passes
    /// [`Error::WAL_PAYLOAD_CAP_BYTES`].
    ///
    /// The buffer is charged as it is built: `charge` is extended before
    /// each growth of the buffer (doubling from 1 KiB, clamped to the cap,
    /// the old and new buffer both charged during the copy), so concurrent
    /// large commits are refused by the workspace budget before they
    /// allocate past it. A refused charge aborts the encoding at once with
    /// the budget's own typed error and leaves nothing allocated beyond the
    /// charge. On success `charge` holds exactly the buffer's capacity (on
    /// top of whatever it held before) and the caller keeps it for as long
    /// as the encoding lives. A commit whose buffer stayed within 64 KiB
    /// is trimmed to its length and charged for that.
    pub fn encode(ops: &[RedoOp], charge: &mut WorkspaceCharge, cap: u64) -> Result<Self> {
        Self::encode_with(ops, Some(charge), cap)
    }

    /// [`Self::encode`] with no workspace charge at all, for a throwaway
    /// encoding no admission holds a budget for (a direct append, a test):
    /// nothing is charged, so no charge can be released while the buffer
    /// it covered is still alive. The payload, the cap and its refusal are
    /// exactly those of [`Self::encode`].
    pub fn encode_unmetered(ops: &[RedoOp], cap: u64) -> Result<Self> {
        Self::encode_with(ops, None, cap)
    }

    fn encode_with(
        ops: &[RedoOp],
        mut meter: Option<&mut WorkspaceCharge>,
        cap: u64,
    ) -> Result<Self> {
        // The frame-bound admission lives in the one core every encoding
        // funnels through, so no entry point can bypass it (ADR-0650).
        super::check_payload_cap(cap, "WAL payload cap")?;
        let t0 = Instant::now();
        // The one validation pass per op: persisted-ID range plus the
        // ADR-0120 §3 16 MiB ontology envelope (the ADR-0086 object
        // bound), before the commit is encoded or written.
        validate_redo_ids(ops)?;
        let ontology_op = ops.iter().any(RedoOp::is_ontology_op);
        let mut buf = vec![0u8; HEADER_MAX];
        // The per-record WAL payload cap (ADR-0650, the caller's resolved
        // value) and the capacity kind its crossing refusal carries
        // (ADR-0494), spelled together here — the one bounded pass over a
        // commit record.
        let refusal = Refusal::Capacity(CapacityKind::WalPayload);
        // What the charge held before this encoding, read once: the sink
        // charges the buffer on top of it and the trim settles against it.
        let held = meter.as_deref().map_or(0, WorkspaceCharge::bytes);
        let mut sink =
            BoundedSink::capture_charged(&mut buf, cap, refusal, meter.as_deref_mut(), held)?;
        let result: io::Result<()> = serde_json::to_writer(&mut sink, ops)
            .map_err(io::Error::from)
            .and_then(|()| sink.write_all(b"}"));
        sink.finish(result, wal_serialization_error, |bytes| {
            Error::storage(format!(
                "WAL encode buffer allocation of {bytes} bytes failed"
            ))
        })?;
        // A record within 64 KiB is small enough that
        // trimming its slack is a cheap copy: hand the slack back. A
        // larger buffer keeps its capacity (and its charge) — it is never
        // copied again.
        if buf.capacity() <= SMALL_RECORD_BYTES {
            buf.shrink_to_fit();
            if let Some(charge) = meter {
                charge.resize(held.saturating_add(buf.capacity() as u64))?;
            }
        }
        Ok(Self {
            buf,
            start: HEADER_MAX,
            ontology_op,
            encode_ns: t0.elapsed().as_nanos() as u64,
        })
    }

    /// The bytes this encoding holds resident — the buffer's capacity,
    /// which is what the charge passed to [`Self::encode`] was extended
    /// to: maximum-width header slack, body and closing brace, plus any
    /// growth slack of a large record.
    pub fn reservation_bytes(&self) -> u64 {
        self.buf.capacity() as u64
    }

    pub(super) fn ontology_op(&self) -> bool {
        self.ontology_op
    }

    pub(super) fn encode_ns(&self) -> u64 {
        self.encode_ns
    }

    /// Write the record header in front of the body, in place.
    pub(super) fn stamp(&mut self, lsn: u64, commit_ts: u64) {
        let mut header = [0u8; HEADER_MAX];
        let mut out = &mut header[..];
        write!(
            out,
            "{}{lsn}{}{commit_ts}{}",
            CommitRecord::LSN_KEY,
            CommitRecord::COMMIT_TS_KEY,
            CommitRecord::OPS_KEY
        )
        .expect("a header of maximum-width fields fits its slack");
        // The header is right-aligned against the body: the unwritten tail
        // of `header` is the slack left in front of it.
        self.start = out.len();
        self.buf[self.start..HEADER_MAX].copy_from_slice(&header[..HEADER_MAX - self.start]);
    }

    /// The record's JSON payload; the bare ops array plus brace before a
    /// header is stamped, so callers only read it after [`Self::stamp`].
    pub(super) fn payload(&self) -> &[u8] {
        &self.buf[self.start..]
    }
}

/// Counts the bytes a serializer writes, so a length can be taken without
/// keeping the bytes.
struct ByteCounter(u64);

impl Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The one spelling of a redo-op serialization failure — the encoder's
/// `serde_error` and [`op_element_len`] both word it here. Generic over the
/// error's `Display`: the encoder's sink folds the serde error into an
/// `io::Error`, while the per-op length pass keeps the `serde_json::Error`.
fn wal_serialization_error(e: impl std::fmt::Display) -> Error {
    Error::storage(format!("WAL serialization failed: {e}"))
}

/// The encoded length of one op as an element of a record's `ops` array —
/// the unit [`PayloadTally`] accumulates.
fn op_element_len(op: &RedoOp) -> Result<u64> {
    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, op).map_err(wal_serialization_error)?;
    Ok(counter.0)
}

/// A running tally of a commit record's prospective frame payload
/// (ADR-0650) over the ops a transaction has accumulated, one op encoded
/// once as it is added instead of the whole array once at commit: the
/// maximum-width header slack, the array's brackets, one separator between
/// elements, the closing brace, and each op's encoded length — the exact
/// figure [`EncodedOps::encode`] bounds against the store's resolved cap
/// (ADR-0650), so a caller can refuse a transaction the moment its redo
/// crosses the cap rather than at the commit that would have failed anyway.
#[derive(Debug, Default)]
pub struct PayloadTally {
    /// The summed encoded length of the elements added so far.
    elements: u64,
    /// How many elements those are — the separator count derives from it,
    /// and the resume position the caller feeds the next `add` derives from
    /// it too: the tally owns the one count of what it has measured.
    ops: u64,
}

impl PayloadTally {
    /// Add `ops`, the batch appended since the last call. All-or-nothing
    /// per batch: a serialization failure part-way through leaves the
    /// tally exactly as it was, so the element figure and the op count
    /// cannot drift apart on the error path.
    pub fn add(&mut self, ops: &[RedoOp]) -> Result<()> {
        let mut elements = 0u64;
        for op in ops {
            elements += op_element_len(op)?;
        }
        self.elements += elements;
        self.ops += ops.len() as u64;
        Ok(())
    }

    /// The number of ops measured so far — the redo-log position the next
    /// [`Self::add`] resumes from, derived from the count the tally already
    /// keeps instead of a second cursor field beside it.
    pub fn ops_count(&self) -> usize {
        usize::try_from(self.ops).expect("op count exceeds usize")
    }

    /// The prospective frame payload of everything added so far, in the
    /// shape [`EncodedOps::encode`] measures it: header slack, `[`, the
    /// elements with a `,` between them, `]`, and the record's `}`.
    pub fn bytes(&self) -> u64 {
        HEADER_MAX as u64 + 2 + self.elements + self.ops.saturating_sub(1) + 1
    }
}
