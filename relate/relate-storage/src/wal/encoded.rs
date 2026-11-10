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
use relate_engine_api::RedoOp;
use web_time::Instant;

use crate::vfs::{BoundedSink, Refusal};
use crate::Wal;

/// Decimal digits of `u64::MAX`.
const U64_DECIMAL_MAX: usize = 20;
const LSN_KEY: &str = "{\"lsn\":";
const COMMIT_TS_KEY: &str = ",\"commit_ts\":";
const OPS_KEY: &str = ",\"ops\":";

/// Records whose buffer is no larger than this are trimmed to their
/// length after encoding (see [`EncodedOps::encode`]).
const SMALL_RECORD_BYTES: usize = 64 * 1024;

/// Largest header the record can carry: the three fixed key spellings and
/// two maximum-width unsigned decimals. It is the slack in front of the
/// ops body, so every real header fits and the admission figure (slack +
/// body + closing brace) is a safe upper bound of the final payload.
const HEADER_MAX: usize = LSN_KEY.len() + COMMIT_TS_KEY.len() + OPS_KEY.len() + 2 * U64_DECIMAL_MAX;

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
    /// record with maximum-width LSN and timestamp.
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
    pub fn encode(ops: &[RedoOp], charge: &mut WorkspaceCharge) -> Result<Self> {
        let t0 = Instant::now();
        // The one validation pass per op: persisted-ID range plus the
        // ADR-0120 §3 16 MiB ontology envelope (the ADR-0086 object
        // bound), before the commit is encoded or written.
        Wal::validate_redo_ids(ops)?;
        let ontology_op = ops.iter().any(RedoOp::is_ontology_op);
        let mut buf = vec![0u8; HEADER_MAX];
        // The per-record WAL payload cap (ADR-0086) and the capacity kind
        // its crossing refusal carries (ADR-0494), spelled together here —
        // the one bounded pass over a commit record.
        let held = charge.bytes();
        let mut sink = BoundedSink::capture_charged(
            &mut buf,
            Error::WAL_PAYLOAD_CAP_BYTES,
            Refusal::Capacity(CapacityKind::WalPayload),
            charge,
        );
        let result: io::Result<()> = serde_json::to_writer(&mut sink, ops)
            .map_err(io::Error::from)
            .and_then(|()| sink.write_all(b"}"));
        sink.finish(result, |e| {
            Error::storage(format!("WAL serialization failed: {e}"))
        })?;
        // A record within 64 KiB is small enough that
        // trimming its slack is a cheap copy: hand the slack back. A
        // larger buffer keeps its capacity (and its charge) — it is never
        // copied again.
        if buf.capacity() <= SMALL_RECORD_BYTES {
            buf.shrink_to_fit();
            charge.resize(held.saturating_add(buf.capacity() as u64))?;
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
        write!(out, "{LSN_KEY}{lsn}{COMMIT_TS_KEY}{commit_ts}{OPS_KEY}")
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
