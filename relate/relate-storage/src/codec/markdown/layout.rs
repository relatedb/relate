//! The sidecar family's layout vocabulary (ADR-0406, ADR-0623) — the
//! module below every sibling that names the engine directory: the
//! directory spellings and the paths derived from them, the
//! probe-context sentence, and the legacy-repair constructors. `manifest`
//! (the sidecar facts and readers) and `adoption` (the shape verdict and
//! the adapters) import downward from here for this vocabulary, never
//! from each other.

use relate_core::Error;

/// The engine directory's one spelling as a literal, so `concat!` can
/// build the paths under it (the schema mirror's directory and the
/// journal's segment derive from it).
macro_rules! engine_dir {
    () => {
        ".relate"
    };
}
pub(crate) use engine_dir;

/// The pre-rename spelling, a macro for the same reason as
/// `engine_dir!` — [`LEGACY_MANIFEST`] builds from it.
macro_rules! legacy_engine_dir {
    () => {
        ".cypherlite"
    };
}

/// The hidden engine directory inside the vault (the `.obsidian/`
/// convention): WAL, journal, and the sidecar manifest.
pub(crate) const ENGINE_DIR: &str = engine_dir!();
/// The pre-rename spelling of [`ENGINE_DIR`], kept only as the name the
/// adoption renames away (ADR-0623, superseding ADR-0517 §4's on-disk
/// keep): an un-adopted store still carries its whole engine state
/// under it, and every presence question about that state consults this
/// spelling beside the adopted one.
pub(crate) const LEGACY_ENGINE_DIR: &str = legacy_engine_dir!();
/// The sidecar manifest, published last (the manifest-last rule).
pub(crate) const MANIFEST: &str = concat!(engine_dir!(), "/manifest.json");
/// The pre-rename spelling's sidecar manifest: the file the adoption
/// renames to [`MANIFEST`], and the presence the reset guard and the
/// cold read consult so an un-adopted store is never mistaken for a
/// fresh one (ADR-0623).
pub(crate) const LEGACY_MANIFEST: &str = concat!(legacy_engine_dir!(), "/manifest.json");
/// The default WAL directory for a markdown store (ADR-0406 §3).
pub(crate) const WAL_DIR: &str = concat!(engine_dir!(), "/wal");

/// The sidecar's spelling (ADR-0623) — the one typed value that pairs
/// the path probed with the engine directory the probe sentence names,
/// so the pair cannot drift: the adopted spelling (`.relate/`), or the
/// legacy one an un-adopted store still carries (`.cypherlite/`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SidecarSpelling {
    /// The adopted spelling: [`ENGINE_DIR`].
    Adopted,
    /// The pre-rename spelling an un-adopted store still carries:
    /// [`LEGACY_ENGINE_DIR`].
    Legacy,
}

impl SidecarSpelling {
    /// The engine directory this spelling names — also the directory
    /// the probe-context sentence names when probing this spelling.
    pub(crate) fn engine_dir(self) -> &'static str {
        match self {
            SidecarSpelling::Adopted => ENGINE_DIR,
            SidecarSpelling::Legacy => LEGACY_ENGINE_DIR,
        }
    }

    /// The sidecar manifest path inside this spelling's directory.
    pub(crate) fn manifest(self) -> &'static str {
        match self {
            SidecarSpelling::Adopted => MANIFEST,
            SidecarSpelling::Legacy => LEGACY_MANIFEST,
        }
    }
}

/// The one probe-context sentence for an engine-directory probe
/// failure — a stat the native probe could not make, or a
/// `metadata`/`exists` call on a caller-supplied VFS that errored —
/// shared by both adapters' probes and the markdown sidecar's readers.
/// Built on the crate's one error-context primitive
/// ([`crate::storage_context`]), so the adoption's probe errors share
/// the crate's context policy instead of a second one. `err` is
/// whatever the probing substrate reports — a host `io::Error`, a VFS
/// [`Error`](relate_core::Error) — so both substrates speak the one
/// sentence.
pub(crate) fn engine_dir_probe_error(name: &str, err: impl std::fmt::Display) -> Error {
    crate::storage_context(
        Error::storage(format!("{err}")),
        format_args!("inspecting the {name} engine directory"),
    )
}

/// ADR-0623's two legacy-sidecar repairs, one carrier: a native root
/// adopts at its next writable open; a caller-supplied VFS is never
/// renamed by the reader that found the legacy spelling — its owner
/// renames the directory inside the VFS, or constructs the store
/// writable once (`from_vfs_with_codec`), whose adoption runs the
/// backend's own `Vfs::rename`.
pub(crate) enum LegacySidecarRepair {
    /// The store has a native root: its next writable open adopts.
    AdoptAtNextWritableOpen,
    /// The store lives on a caller-supplied VFS: the owner renames, or
    /// constructs writable once — the reader that found this never
    /// renames.
    CallerRenames,
}

/// The one legacy-sidecar refusal (ADR-0623): the repair the store can
/// actually reach, in the carrier's wording — same family, one
/// constructor per repair.
pub(crate) fn legacy_sidecar_error(repair: LegacySidecarRepair) -> Error {
    let text = match repair {
        LegacySidecarRepair::AdoptAtNextWritableOpen => format!(
            "the store still carries the legacy `{LEGACY_ENGINE_DIR}` engine directory; \
             open it writable once to adopt the `{ENGINE_DIR}` spelling"
        ),
        LegacySidecarRepair::CallerRenames => format!(
            "the store's engine state is under the legacy `{LEGACY_ENGINE_DIR}` directory and \
             this read never renames it: rename it to `{ENGINE_DIR}` in the VFS, or construct \
             the store writable once (`StoreTarget::from_vfs_with_codec`), which adopts it \
             through the VFS's own rename where the backend moves trees"
        ),
    };
    Error::storage(text)
}
