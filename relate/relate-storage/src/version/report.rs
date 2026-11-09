//! The migrate-on-open report and its error-context helper (ADR-0127
//! Decisions 4 and 8), split out of the version registry.

use relate_core::Error;

use crate::map_message;

use super::SnapshotMarker;

/// The subsystems a snapshot migration can rewrite. The WAL and bundle
/// container never appear here today: `MDBWAL01` is both minimum and
/// current (ADR-0127 Decision 5) and the container format is frozen
/// (Decision 6), so only the snapshot markers migrate. `Manifest` exists
/// only with the Parquet codec (ADR-0105): its loader is the variant's
/// sole constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Subsystem {
    #[cfg(manifest_codec)]
    Manifest,
    ValueEncoding,
}

impl std::fmt::Display for Subsystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(manifest_codec)]
            Subsystem::Manifest => "manifest",
            Subsystem::ValueEncoding => "value_encoding",
        })
    }
}

/// Record of what migrate-on-open rewrote (ADR-0127 Decision 8): the one
/// migration this open performed — "subsystem X was at version N,
/// published at M", both ends recorded by the one site that knows what
/// the migration checkpoint published. A store's snapshot is one codec's
/// marker, so an open migrates at most one subsystem: the report holds
/// one entry, never a list. Internal to `relate-storage`; the crate has
/// no logging facility, so the report is observable only through
/// `pub(crate)` test hooks.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct MigrationReport {
    migrated: Option<(Subsystem, u64, u64)>,
}

impl MigrationReport {
    /// Record the one migration this open performed. A store's snapshot
    /// is one codec's marker, so a second record never comes; should one
    /// anyway, it replaces — there is no list to append to.
    pub(crate) fn record(&mut self, subsystem: Subsystem, from: u64, to: u64) {
        self.migrated = Some((subsystem, from, to));
    }

    /// The migration this open performed, if any: (subsystem, the
    /// version it was at, the version it was published at).
    pub(crate) fn migration(&self) -> Option<(Subsystem, u64, u64)> {
        self.migrated
    }
}

impl std::fmt::Display for MigrationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.migration() {
            Some((subsystem, from, to)) => write!(f, "{subsystem} {from}→{to}"),
            // Only what the report knows: an empty report is also a
            // legacy marker on a VFS migrate-on-open skips (ADR-0127
            // Decision 7), not proof the store is at the current format.
            None => f.write_str("no migration recorded"),
        }
    }
}

/// Attach the decoded version to a legacy-reader failure without changing
/// its `Error` variant or doubling its display prefix (ADR-0127 Decision
/// 4): [`map_message`] appends the context to `Storage` and `Io` messages;
/// every other variant keeps its payload verbatim.
pub(crate) fn with_version_context(error: Error, context: std::fmt::Arguments<'_>) -> Error {
    map_message(error, |message| format!("{message} ({context})"))
}

/// The one legacy-error shape (ADR-0127 Decision 4): a failure while
/// reading a legacy store names the decoded version via
/// [`with_version_context`]; a current store keeps its error verbatim. The
/// context wording is per subsystem and lives here alone, so every legacy
/// gate phrases it identically.
pub(crate) fn annotate_legacy_failure(error: Error, marker: SnapshotMarker) -> Error {
    let SnapshotMarker::Legacy(subsystem, version) = marker else {
        return error;
    };
    match subsystem {
        #[cfg(manifest_codec)]
        Subsystem::Manifest => with_version_context(
            error,
            format_args!("store snapshot is at manifest version {version}"),
        ),
        Subsystem::ValueEncoding => with_version_context(
            error,
            format_args!("store snapshot is at value_encoding {version}"),
        ),
    }
}
