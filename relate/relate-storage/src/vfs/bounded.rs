//! The crate's bounded serializers (ADR-0492's one-channel sink shape):
//! [`BoundedSink`] over a caller-owned buffer, [`CappedVfsWriter`] over a
//! VFS file, the test-only `CappedSink` (the capped in-memory twin), and
//! the typed [`Refusal`] they carry — extracted from `mod.rs` (the
//! no-1k-line rule, LABS-311). The counting core ([`BoundCore`]) is the
//! one admission policy and the one crossing query every bounded
//! serializer shares.

use std::io::{self, Write};

use relate_core::{CapacityKind, Error, Result};

#[cfg(any(test, parquet_codec))]
use super::VfsFile;

/// The counting core every bounded serializer shares: the running byte
/// total, the bound, the sole crossing fact, and the typed refusal. One
/// admission policy ([`BoundCore::admit`]) and one crossing query
/// ([`BoundCore::crossed_refusal`]) — the next bounded writer embeds this
/// core instead of re-deriving either.
struct BoundCore {
    /// Bytes admitted so far; advanced ([`BoundCore::commit`]) only after
    /// the destination accepted them.
    bytes: u64,
    limit: u64,
    /// The counted byte total at the moment the bound was crossed: the
    /// crossing write's running total when the byte bound refused it, the
    /// saturating maximum when the counter itself overflowed, or the
    /// bytes counted so far when the capped allocation refused. The sole
    /// crossing fact — `None` while the bound held, set by every refusal
    /// path, never re-derived at the refusal's consumer.
    crossed_at: Option<u64>,
    refusal: Refusal,
}

impl BoundCore {
    fn new(limit: u64, refusal: Refusal) -> Self {
        Self {
            bytes: 0,
            limit,
            crossed_at: None,
            refusal,
        }
    }

    /// Admit one write of `len` bytes against the bound: the running
    /// total the caller commits ([`Self::commit`]) once the destination
    /// accepts the bytes, or the typed abort of the recorded crossing
    /// when the write would cross the bound — or when the counter itself
    /// overflows, refused as the saturating maximum.
    fn admit(&mut self, len: usize) -> io::Result<u64> {
        let Some(total) = self.bytes.checked_add(len as u64) else {
            return Err(self.refuse_at(u64::MAX));
        };
        if total > self.limit {
            return Err(self.refuse_at(total));
        }
        Ok(total)
    }

    /// Record the crossing at `crossed_at` and render the typed refusal
    /// as the io abort — the one spelling of a refused write.
    /// [`Self::admit`] and the capped capture's refused growth consume
    /// it, so the crossing fact and the abort's rendering can never
    /// drift apart.
    fn refuse_at(&mut self, crossed_at: u64) -> io::Error {
        self.crossed_at = Some(crossed_at);
        bound_refusal(&self.refusal.error(crossed_at, self.limit))
    }

    /// Advance the counter to an admitted total — after the destination
    /// accepted the bytes, so a refused write leaves it where it was.
    fn commit(&mut self, total: u64) {
        self.bytes = total;
    }

    /// The typed refusal of a crossed bound — the one error a crossing
    /// surfaces as; `None` while the bound held, so the serializer's io
    /// abort never re-stringifies into it.
    fn crossed_refusal(&self) -> Option<Error> {
        Some(self.refusal.error(self.crossed_at?, self.limit))
    }
}

/// A [`Write`] adapter over a VFS file which fails before a write would
/// exceed `limit`. It keeps Parquet checkpoint candidates bounded without
/// collecting their complete contents in heap memory — the destination
/// half of the shared [`BoundCore`] policy, with the file's `write_all`
/// as its append.
#[cfg(any(test, parquet_codec))]
pub(crate) struct CappedVfsWriter<'a> {
    core: BoundCore,
    file: &'a mut dyn VfsFile,
}

#[cfg(any(test, parquet_codec))]
impl<'a> CappedVfsWriter<'a> {
    pub(crate) fn new(file: &'a mut dyn VfsFile, limit: u64, refusal: Refusal) -> Self {
        Self {
            core: BoundCore::new(limit, refusal),
            file,
        }
    }

    /// The typed refusal of a crossed bound — the one error a crossing
    /// surfaces as (the Parquet shard writers' `ShardError::Envelope`
    /// payload); `None` while the bound held, so the serializer's io
    /// abort never re-stringifies into it.
    pub(crate) fn crossed_refusal(&self) -> Option<Error> {
        self.core.crossed_refusal()
    }
}

#[cfg(any(test, parquet_codec))]
impl Write for CappedVfsWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let total = self.core.admit(buf.len())?;
        self.file
            .write_all(buf)
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.core.commit(total);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A `Write` sink bounded by a byte `limit`.
///
/// The one shared shape for every bounded serializer in this crate: count
/// the bytes written, and on the first write that would cross `limit`
/// record the crossing count and abort the serializer with an io error
/// carrying the typed refusal's rendering. The refusal itself — the
/// [`Refusal`] the sink was constructed with — travels on one channel,
/// [`BoundedSink::finish`], never as a separate (limit, message) pair.
/// The admitted bytes are captured into a caller-owned `Vec`. The
/// test-only capped capture — the same admission policy with the backing
/// allocation also bounded — is its own type (`CappedSink`), so the
/// production sink's shape is the same in every build.
pub(crate) struct BoundedSink<'a> {
    core: BoundCore,
    output: &'a mut Vec<u8>,
}

/// The typed refusal a crossed bound produces — the one error channel
/// ([`BoundedSink::finish`]): either the caller's fixed error (the
/// snapshot manifest bound, the Parquet shard envelope), or a capacity
/// refusal (ADR-0494) the crossing count and the bound parameterize —
/// `used` is the byte total at which the serializer was stopped, a lower
/// bound of the payload the statement tried to build, `limit` the bound
/// the check consulted, and `kind` which capacity contract refused. The
/// sink stays caller-agnostic: it knows the capacity family, never which
/// subsystem's cap refused.
pub(crate) enum Refusal {
    /// Parquet-only constructor (the native manifest writer's and the
    /// shard envelope's fixed refusals) — the file's
    /// not(parquet_codec) dead-code idiom.
    #[cfg_attr(not(parquet_codec), allow(dead_code))]
    Fixed(Error),
    Capacity(CapacityKind),
}

impl Refusal {
    /// The refusal as the error [`BoundedSink::finish`] returns and the
    /// io abort renders: the fixed error verbatim, or the capacity
    /// refusal the crossing count parameterizes — the one parametric
    /// construction of [`Error::Capacity`]; the per-contract public
    /// constructors (`Error::capacity`, `Error::capacity_expression`,
    /// `Error::capacity_wal_payload`) spell the same variant per kind.
    fn error(&self, crossed_at: u64, limit: u64) -> Error {
        match self {
            Refusal::Fixed(error) => error.clone(),
            Refusal::Capacity(kind) => Error::Capacity {
                used: crossed_at,
                limit,
                kind: *kind,
            },
        }
    }
}

impl<'a> BoundedSink<'a> {
    /// Append captured bytes into `output`, up to `limit`. Bytes `output`
    /// already holds count against the limit and in the crossing figure, so
    /// a caller that reserves a fixed prefix in front of the serialized
    /// bytes (the WAL record header slack) is bounded on the whole figure.
    pub(crate) fn capture(output: &'a mut Vec<u8>, limit: u64, refusal: Refusal) -> Self {
        let mut core = BoundCore::new(limit, refusal);
        core.commit(output.len() as u64);
        Self { core, output }
    }

    /// Complete one bounded serialization — the one refusal channel: the
    /// counted bytes when the bound held; the typed `refusal` when it was
    /// crossed (the serializer's abort is discarded, the refusal is the
    /// truth); or the serializer's own failure mapped by `serde_error`.
    pub(crate) fn finish<E>(
        self,
        result: std::result::Result<(), E>,
        serde_error: impl FnOnce(E) -> Error,
    ) -> Result<u64> {
        if let Some(error) = self.core.crossed_refusal() {
            return Err(error);
        }
        result.map_err(serde_error).map(|()| self.core.bytes)
    }
}

/// The test-only capped capture: [`BoundedSink`]'s admission policy over
/// a caller-owned buffer whose backing allocation is also bounded — the
/// in-memory twin of [`CappedVfsWriter`]. Its own type, so the production
/// sink's shape is the same in every build. Growth is geometric but clamped to the bound
/// ([`append_bounded`]).
#[cfg(test)]
pub(crate) struct CappedSink<'a> {
    core: BoundCore,
    out: &'a mut Vec<u8>,
}

#[cfg(test)]
impl<'a> CappedSink<'a> {
    /// Append into `output` while keeping both the bytes counted and the
    /// backing capacity at or below `bound`, with `try_reserve_exact`
    /// growth.
    pub(crate) fn new(output: &'a mut Vec<u8>, bound: u64, refusal: Refusal) -> Self {
        // One counting base: the core counts from zero and the buffer
        // arrives empty, so the core's running total is the buffer's
        // length at every write — the base `append_bounded`'s cap checks
        // consult. A non-empty buffer would put the two bases out of
        // step, so it is refused here rather than silently miscounted.
        assert!(
            output.is_empty(),
            "the capped capture appends into an empty buffer"
        );
        Self {
            core: BoundCore::new(bound, refusal),
            out: output,
        }
    }

    /// Backing capacity of the capture buffer.
    pub(crate) fn capacity(&self) -> usize {
        self.out.capacity()
    }

    /// Bytes captured so far.
    pub(crate) fn captured_len(&self) -> usize {
        self.out.len()
    }

    /// The typed refusal of a crossed bound — the one error a crossing
    /// surfaces as for consumers that classify the serializer's own
    /// errors themselves (the Parquet shard writer's `ShardError` split)
    /// rather than reading the serializer's io abort; `None` while the
    /// bound held, so the abort never re-stringifies into it.
    /// Parquet-only consumer — the file's not(parquet_codec) dead-code
    /// idiom.
    #[cfg_attr(not(parquet_codec), allow(dead_code))]
    pub(crate) fn crossed_refusal(&self) -> Option<Error> {
        self.core.crossed_refusal()
    }
}

#[cfg(test)]
impl Write for CappedSink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let total = self.core.admit(buf.len())?;
        // A refused growth is a refused write at the bytes counted so
        // far — the core's own running total, equal to the buffer's
        // length under the constructor's empty-buffer invariant —
        // recorded and rendered by the one refusal path.
        if append_bounded(self.out, buf, self.core.limit).is_err() {
            return Err(self.core.refuse_at(self.core.bytes));
        }
        self.core.commit(total);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for BoundedSink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let total = self.core.admit(buf.len())?;
        self.output.extend_from_slice(buf);
        self.core.commit(total);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The io abort a crossing write hands the in-flight serializer: the typed
/// refusal's own rendering, so the abort can never disagree with the refusal
/// [`BoundedSink::finish`] returns — it only stops the serializer.
fn bound_refusal(refusal: &Error) -> io::Error {
    io::Error::other(refusal.to_string())
}

/// Grow `out` by `buf` inside the allocation envelope `limit` — the
/// capacity cap alone; the byte bound is [`BoundCore::admit`]'s, checked
/// once against the same figure before this runs, so it is never
/// re-derived here. `try_reserve_exact` reserves relative to `len`, so
/// growth is geometric but clamped to the cap. A refusal — an entry
/// capacity already past the cap, a length past usize space, or a growth
/// the cap or the allocator declined — surfaces as the bare fact
/// (`Err(())`); the crossing count it crossed at is the core's own
/// `bytes`, which the caller records through the one record-and-render
/// path ([`BoundCore::refuse_at`]) — never re-derived from the buffer
/// here.
#[cfg(test)]
fn append_bounded(out: &mut Vec<u8>, buf: &[u8], limit: u64) -> std::result::Result<(), ()> {
    // A bound beyond usize space caps no allocation a Vec can hold.
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if out.capacity() > limit {
        return Err(());
    }
    let end = out.len().checked_add(buf.len()).ok_or(())?;
    if end > out.capacity() {
        let target = out.capacity().max(1).saturating_mul(2).max(end).min(limit);
        // A growth the allocator refused is a refused envelope: the fact
        // surfaces, and the abort is the typed refusal's own rendering —
        // never the allocator's text.
        out.try_reserve_exact(target - out.len()).map_err(|_| ())?;
        // An allocator may provide more capacity than requested. Do not rely
        // on shrinking it to enforce the envelope.
        if out.capacity() > limit {
            return Err(());
        }
    }
    out.extend_from_slice(buf);
    Ok(())
}
