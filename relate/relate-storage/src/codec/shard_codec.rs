//! The Vortex shard encode/decode registry (ADR-0665): a row-shard codec's
//! byte serialization depends on its backing crate, which the publish closure
//! must not carry. The experiment crate installs the pair at startup;
//! without it the codec fails closed through [`codec_unavailable`].
//!
//! The registry is process-wide and write-once: an installed codec is never
//! replaced, so a store's reader and writer always agree on the wire bytes.
//! [`SnapshotCodecKind::is_compiled`] reads this one cell live, and
//! [`SnapshotCodecKind::compiled`] consults it on every call (no
//! first-read snapshot), so an install can never leave the two answers
//! disagreeing. The experiment crate's reference implementation is
//! measurement-only — its decode paths panic on I/O instead of returning the
//! error, and a null row decodes to the empty string.

use std::sync::OnceLock;

use relate_core::Result;

use super::{codec_unavailable, SnapshotCodecKind};

/// Encode rows into one shard's bytes.
pub type ShardEncode = fn(&[String]) -> Result<Vec<u8>>;
/// Decode one shard's bytes back into rows.
pub type ShardDecode = fn(&[u8]) -> Result<Vec<String>>;

#[derive(Clone, Copy)]
struct ShardCodec {
    encode: ShardEncode,
    decode: ShardDecode,
}

fn slot() -> &'static OnceLock<ShardCodec> {
    static SLOT: OnceLock<ShardCodec> = OnceLock::new();
    &SLOT
}

/// Install the shard encode/decode pair. The first install wins; a later
/// call leaves the published pair untouched.
#[doc(hidden)]
pub fn install(encode: ShardEncode, decode: ShardDecode) {
    let _ = slot().set(ShardCodec { encode, decode });
}

/// Whether the shard codec is installed — the codec vocabulary's
/// [`is_compiled`](SnapshotCodecKind::is_compiled) consults this.
pub(crate) fn installed() -> bool {
    slot().get().is_some()
}

fn codec() -> Result<ShardCodec> {
    slot()
        .get()
        .copied()
        .ok_or_else(|| codec_unavailable(SnapshotCodecKind::Vortex))
}

pub(crate) fn encode(rows: &[String]) -> Result<Vec<u8>> {
    (codec()?.encode)(rows)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<String>> {
    (codec()?.decode)(bytes)
}
