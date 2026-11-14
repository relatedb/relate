//! The connection's negotiated dialect (ADR-0329, ADR-0484) and its
//! capability methods: the `patch_bolt` patches (a 4.4-only axis) and
//! the spoken Bolt version — the two axes every version-gated value
//! form reads, in both wire directions. A leaf beside `version`
//! (importing only `patches` and `version`); the field decoders
//! (`fields`, `param_structs`), the value codec (`value_codec`) and the
//! session import downward.

use crate::patches::{BoltPatch, BoltPatches};
use crate::version::BoltVersion;

/// The connection's negotiated dialect (ADR-0329, ADR-0484): the
/// `patch_bolt` patches (a 4.4-only axis) and the spoken Bolt version —
/// the two axes every version-gated value form reads, in both
/// directions. `value_codec::ValueCodec` is this bundle plus the session
/// namespace the 5.x element-id fields read; the parameter decode chain
/// (`params_to_map`, `Pack::to_mdb_at`, `struct_to_value` in
/// `param_structs`) takes the bundle alone, so no site threads the two
/// axes as a loose pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Dialect {
    /// The 4.4 `patch_bolt` patches (ADR-0329); none at 5.x, where the
    /// HELLO ignores `patch_bolt`.
    pub(crate) patches: BoltPatches,
    /// The negotiated Bolt version (ADR-0484).
    pub(crate) version: BoltVersion,
}

impl Dialect {
    /// The dialect of a connection that negotiated `patches` at
    /// `version`.
    pub(crate) fn new(patches: BoltPatches, version: BoltVersion) -> Self {
        Self { patches, version }
    }

    /// Whether the UTC DateTime structures `0x49`/`0x69` apply on this
    /// connection: negotiated through the 4.4 `utc` patch (ADR-0329) or
    /// unconditionally on a 5.x connection (ADR-0484 decision 7),
    /// parameters and results alike — the one derivation behind both
    /// directions, so they can never disagree.
    pub(crate) fn utc_datetime_structures(self) -> bool {
        match self.version {
            BoltVersion::V4_4 => self.patches.applies(BoltPatch::Utc),
            BoltVersion::V5_4 => true,
        }
    }

    /// Whether the graph structures carry element-id fields (ADR-0484
    /// decision 6): the 5.x structures do, the 4.4 ones do not.
    pub(crate) fn element_ids(self) -> bool {
        self.version.speaks_5x()
    }

    /// The plain Bolt 4.4 dialect — no patch, the 4.4 structures — the
    /// unit tests' spelling.
    #[cfg(test)]
    pub(crate) fn plain_44() -> Self {
        Self::new(BoltPatches::default(), BoltVersion::V4_4)
    }

    /// The 4.4 dialect with the `utc` patch negotiated (ADR-0329): the
    /// 4.4 spelling of the UTC DateTime pair.
    #[cfg(test)]
    pub(crate) fn utc_44() -> Self {
        Self::new(BoltPatches::utc(), BoltVersion::V4_4)
    }

    /// The 5.4 dialect (ADR-0484 decision 7): the UTC DateTime pair with
    /// no patch to negotiate.
    #[cfg(test)]
    pub(crate) fn at_5_4() -> Self {
        Self::new(BoltPatches::default(), BoltVersion::V5_4)
    }
}
