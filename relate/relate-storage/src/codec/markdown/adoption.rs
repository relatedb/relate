//! ADR-0623's adoption subsystem — the one home for the knowledge both
//! adoptions share: the engine-directory shape verdict and its
//! classification, both refusal families and their constructors, the
//! probe and rename error vocabulary, and the caller-supplied-VFS
//! adapter. What genuinely differs between the two adoptions stays with
//! its own substrate: the native adapter's host probes, writer lease,
//! and no-replace rename live in `native_ownership`; the VFS adapter
//! here probes through the [`Vfs`] trait and renames through it, with
//! no lease — a caller-supplied VFS's concurrency belongs to its
//! caller.

use relate_core::{Error, Result};

use super::layout::{
    engine_dir_probe_error, legacy_sidecar_error, LegacySidecarRepair, SidecarSpelling, ENGINE_DIR,
    LEGACY_ENGINE_DIR,
};
use super::manifest::probe_sidecar;
use crate::{OpenMode, Vfs};

/// One engine-directory entry's shape (ADR-0623) — the adoption's
/// three-way probe verdict, the one classification both adoptions
/// speak: the native (host-filesystem, writer-leased) probe in
/// `native_ownership::engine_entry` and the VFS probe in
/// [`engine_entry_vfs`] map the same verdicts to the same refusals
/// through [`EngineEntry::adoption`]. The entry is absent, a plain
/// directory, or anything else (a file, an alias).
#[derive(Clone, Copy, Debug)]
pub(crate) enum EngineEntry {
    /// No entry at the name: nothing to adopt, nothing to refuse.
    Absent,
    /// A plain directory — the shape the adoption renames.
    Dir,
    /// A present entry of any other shape (a file, an alias).
    Other,
}

/// ADR-0623's adoption refusals, one wording for the native
/// (host-filesystem, writer-leased) and the VFS adoptions alike.
pub(crate) enum AdoptionRefusal {
    /// The legacy entry exists and is not a directory.
    LegacyNotADirectory,
    /// The engine entry exists and is not a directory.
    EngineNotADirectory,
    /// Both spellings are present.
    BothDirectories,
}

/// The one adoption refusal (ADR-0623) for a refused shape: the hand
/// repair in the family's wording — same family as
/// [`legacy_sidecar_error`](super::layout::legacy_sidecar_error), one
/// constructor per refusal.
pub(crate) fn adoption_refusal(case: AdoptionRefusal) -> Error {
    let text = match case {
        AdoptionRefusal::LegacyNotADirectory => format!(
            "refusing to open: `{LEGACY_ENGINE_DIR}` exists and is not a directory; \
             remove it by hand and retry"
        ),
        AdoptionRefusal::EngineNotADirectory => format!(
            "refusing to open: `{ENGINE_DIR}` exists and is not a directory; \
             remove it by hand and retry"
        ),
        AdoptionRefusal::BothDirectories => format!(
            "refusing to open: the store carries both a `{LEGACY_ENGINE_DIR}` and an \
             `{ENGINE_DIR}` engine directory; delete the stale `{LEGACY_ENGINE_DIR}` \
             directory by hand and retry"
        ),
    };
    Error::storage(text)
}

/// The adoption's verdict over the two engine-directory shapes
/// (ADR-0623): the one classification both adapters consult, so the
/// native (writer-leased, host-rename) and VFS (lease-free,
/// [`Vfs::rename`]) adoptions can never disagree about what the shapes
/// mean.
pub(crate) enum AdoptionVerdict {
    /// No legacy directory: a fresh or already-adopted store, nothing
    /// to do.
    Nothing,
    /// The legacy directory is a directory and the engine name is
    /// free: the adapter's own rename proceeds.
    Proceed,
    /// A shape the adoption refuses, with the shared refusal.
    Refuse(AdoptionRefusal),
}

impl EngineEntry {
    /// The adoption classification: [`AdoptionVerdict::Nothing`] when
    /// no legacy directory exists, [`AdoptionVerdict::Proceed`] when it
    /// is a directory and the engine name is free, and
    /// [`AdoptionVerdict::Refuse`] for every refused shape — a present
    /// entry that is not a directory at either name, or both spellings
    /// present.
    pub(crate) fn adoption(legacy: EngineEntry, engine: EngineEntry) -> AdoptionVerdict {
        match (legacy, engine) {
            (EngineEntry::Absent, _) => AdoptionVerdict::Nothing,
            (EngineEntry::Other, _) => {
                AdoptionVerdict::Refuse(AdoptionRefusal::LegacyNotADirectory)
            }
            (EngineEntry::Dir, EngineEntry::Absent) => AdoptionVerdict::Proceed,
            (EngineEntry::Dir, EngineEntry::Other) => {
                AdoptionVerdict::Refuse(AdoptionRefusal::EngineNotADirectory)
            }
            (EngineEntry::Dir, EngineEntry::Dir) => {
                AdoptionVerdict::Refuse(AdoptionRefusal::BothDirectories)
            }
        }
    }
}

/// The one adoption-rename failure, spoken by both adapters' renames:
/// the store keeps its legacy spelling — a crash inside the window
/// re-adopts at the next writable open, and a backend whose rename
/// moves single entries fails closed, never a silent half-move.
pub(crate) fn adoption_rename_error(err: impl std::fmt::Display) -> Error {
    Error::storage(format!("adopting the legacy engine directory: {err}"))
}

/// ADR-0623's pre-read step for a caller-supplied [`Vfs`] — the
/// mode-aware twin of the native open's pre-lease legs
/// (`native_ownership::refuse_unadopted_legacy`, run first for every
/// read-only open, and `native_ownership::adopt_legacy_engine_dir`,
/// run last before the lease after every refusing check), kept whole
/// here because a VFS construction has no lease to order around — run
/// inside
/// [`StoreTarget::from_vfs_with_codec_opening`](crate::StoreTarget::from_vfs_with_codec_opening):
/// a writable construction adopts a legacy `.cypherlite/` engine
/// directory through [`Vfs::rename`] before the sidecar is first read,
/// a read-only one refuses an un-adopted legacy markdown sidecar
/// closed, naming the one writable pass that adopts it — resolving the
/// store as a fresh classic one would probe its WAL at the wrong
/// spelling — and a create-exclusive construction is left untouched,
/// mirroring the native entry (its exclusivity rules refuse a
/// populated store; adoption must not mutate one first).
pub(crate) fn prepare_legacy_engine_dir_vfs(vfs: &dyn Vfs, mode: OpenMode) -> Result<()> {
    match mode {
        OpenMode::OpenOrCreate => adopt_legacy_engine_dir_vfs(vfs),
        OpenMode::ReadOnly => refuse_unadopted_legacy_markdown_vfs(vfs),
        OpenMode::CreateExclusive => Ok(()),
    }
}

/// One engine-directory entry's shape through a caller-supplied [`Vfs`]
/// — the VFS twin of the native `engine_entry` probe, returning the
/// same shared [`EngineEntry`] verdict. One `metadata` call classifies
/// the entry; its failure branch re-asks presence through the backend's
/// own `exists` — the benign-absence question for the VFS substrate,
/// which reports no io kind — so an entry that vanished between the
/// calls classifies `Absent` (the verdict a removed directory earns),
/// and only a genuinely present-but-unstatable entry fails closed
/// through [`engine_dir_probe_error`], never folded into absence.
fn engine_entry_vfs(vfs: &dyn Vfs, name: &str) -> Result<EngineEntry> {
    match vfs.metadata(name) {
        Ok(meta) => Ok(if meta.is_dir() {
            EngineEntry::Dir
        } else {
            EngineEntry::Other
        }),
        Err(err) => match vfs.exists(name) {
            Ok(false) => Ok(EngineEntry::Absent),
            // Present but unstatable, or the existence oracle itself
            // failed: the probe fails closed through the one sentence,
            // naming the metadata failure that opened this branch.
            _ => Err(engine_dir_probe_error(name, &err)),
        },
    }
}

/// The writable leg: adopt a legacy engine directory on a
/// caller-supplied [`Vfs`] — the same shape classification
/// ([`EngineEntry::adoption`]) and the same shared refusals as the
/// native adoption, one [`Vfs::rename`] of the whole tree. No lease is
/// taken (the VFS's concurrency belongs to its caller), so the
/// function is exactly the three-arm match the verdict models — the
/// native twin's Absent pre-check exists only to decide whether the
/// writer lease is taken at all, a constraint this lease-free adapter
/// does not have. The sidecar is first read only after this returns,
/// at the adopted spelling.
fn adopt_legacy_engine_dir_vfs(vfs: &dyn Vfs) -> Result<()> {
    match EngineEntry::adoption(
        engine_entry_vfs(vfs, LEGACY_ENGINE_DIR)?,
        engine_entry_vfs(vfs, ENGINE_DIR)?,
    ) {
        AdoptionVerdict::Nothing => Ok(()),
        // A backend that moves single entries fails closed through the
        // one rename error — the store keeps its legacy spelling, and
        // the cold read's refusal still names the caller's repair.
        AdoptionVerdict::Proceed => vfs
            .rename(LEGACY_ENGINE_DIR, ENGINE_DIR)
            .map_err(adoption_rename_error),
        AdoptionVerdict::Refuse(refusal) => Err(adoption_refusal(refusal)),
    }
}

/// The read-only leg: a store whose legacy sidecar is markdown
/// (ADR-0623) on a caller-supplied [`Vfs`] — the twin of the native
/// `refuse_unadopted_legacy_markdown`: read-only cannot adopt, and an
/// un-adopted sidecar must not resolve as a fresh classic store, so it
/// refuses closed with the same adopt-once verdict, naming the one
/// writable pass that adopts it. The one typed legacy presence
/// classifier decides: any present sidecar — markdown, unparseable, or
/// naming another codec — refuses (the adoption renames on the
/// directory's shape, never on content); a legacy `.cypherlite/`
/// without the manifest (a classic store's schema mirror alone) is
/// inert.
fn refuse_unadopted_legacy_markdown_vfs(vfs: &dyn Vfs) -> Result<()> {
    use super::manifest::ManifestProbe;
    match probe_sidecar(vfs, SidecarSpelling::Legacy)? {
        ManifestProbe::Absent => Ok(()),
        ManifestProbe::Markdown(_) | ManifestProbe::Unparseable | ManifestProbe::Other(_) => Err(
            legacy_sidecar_error(LegacySidecarRepair::AdoptAtNextWritableOpen),
        ),
    }
}
