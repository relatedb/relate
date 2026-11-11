use std::str::FromStr;

use relate_core::{Error, Result};
use serde::{Deserialize, Serialize};

/// Snapshot encoding for store files (ADR-0050). WAL is independent.
///
/// `Default` is the codec for new stores: Parquet when it is compiled in
/// (ADR-0105), JSON otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotCodecKind {
    Parquet,
    Json,
    /// The snapshot is a folder of markdown files (ADR-0406): an
    /// Obsidian-compatible vault read and written by the engine, with
    /// the WAL and the engine sidecar under `.cypherlite/`.
    Markdown,
    /// EXPERIMENTAL (default off, `rkyv-codec`): one zero-copy rkyv
    /// archive. Measurement only until an ADR promotes it.
    Rkyv,
}

impl Default for SnapshotCodecKind {
    fn default() -> Self {
        Self::compiled()[0]
    }
}

impl SnapshotCodecKind {
    /// The codec vocabulary, spelled once (the new-store default
    /// first): [`Self::compiled`] derives its build-dependent slice
    /// from it, the unknown-codec error derives its expected-name
    /// list, and [`FromStr`] derives its name→kind inverse through
    /// [`Self::as_str`]. A new codec edits this table, `as_str`, and a
    /// compile-forced arm in each layout fact ([`Self::wal_dir`],
    /// [`Self::checkpoint_image`], [`Self::engine_dir`],
    /// [`Self::feature_name`]).
    const ALL: [Self; 4] = [Self::Parquet, Self::Json, Self::Markdown, Self::Rkyv];

    /// Whether this codec is compiled into the build. JSON always is;
    /// Parquet needs the `parquet` feature on a non-wasm32 target (ADR-0105).
    ///
    /// # Example
    /// ```
    /// use relate_storage::SnapshotCodecKind;
    /// assert!(SnapshotCodecKind::Json.is_compiled());
    /// assert_eq!(
    ///     SnapshotCodecKind::Parquet.is_compiled(),
    ///     SnapshotCodecKind::compiled().contains(&SnapshotCodecKind::Parquet),
    /// );
    /// ```
    pub const fn is_compiled(self) -> bool {
        match self {
            Self::Parquet => cfg!(parquet_codec),
            Self::Rkyv => cfg!(rkyv_codec),
            Self::Json | Self::Markdown => true,
        }
    }

    /// The Cargo feature that compiles this codec in — the name the
    /// not-compiled error tells the user to enable. Always-compiled
    /// codecs have none; their arm is unreachable behind
    /// [`is_compiled`](Self::is_compiled).
    pub const fn feature_name(self) -> &'static str {
        match self {
            Self::Parquet => "parquet",
            Self::Rkyv => "rkyv-codec",
            Self::Json | Self::Markdown => "",
        }
    }

    /// The compiled codecs whose engine state lives at the store root —
    /// the set the layout-asserting suites iterate: the markdown codec
    /// keeps its engine state in the `.cypherlite/` sidecar
    /// (ADR-0406 §5), so its stores are not root-layout.
    pub fn compiled_with_root_layout() -> impl Iterator<Item = SnapshotCodecKind> {
        Self::compiled()
            .iter()
            .copied()
            .filter(|codec| codec.engine_dir().is_none())
    }

    /// Every codec compiled into the build, the new-store default first.
    ///
    /// # Example
    /// ```
    /// use relate_storage::SnapshotCodecKind;
    /// let compiled = SnapshotCodecKind::compiled();
    /// assert_eq!(compiled[0], SnapshotCodecKind::default());
    /// assert!(compiled.contains(&SnapshotCodecKind::Json));
    /// assert!(compiled.iter().all(|codec| codec.is_compiled()));
    /// ```
    pub fn compiled() -> &'static [SnapshotCodecKind] {
        // Filtered, not sliced: with more than one optional codec a
        // prefix slice would misreport the build. `ALL`'s order is the
        // new-store preference order, so the filter keeps `[0]` the
        // default.
        static COMPILED: std::sync::OnceLock<Vec<SnapshotCodecKind>> = std::sync::OnceLock::new();
        COMPILED.get_or_init(|| {
            Self::ALL
                .iter()
                .copied()
                .filter(|kind| kind.is_compiled())
                .collect()
        })
    }

    /// The codec names as the unknown-codec error spells them —
    /// [`ALL`](Self::ALL) rendered as prose, so the message cannot
    /// drift from the vocabulary.
    fn expected_names() -> String {
        let names: Vec<&str> = Self::ALL.iter().map(|kind| kind.as_str()).collect();
        let (last, rest) = names.split_last().expect("the codec table is never empty");
        format!("{} or {last}", rest.join(", "))
    }

    /// Codec of a manifest without a `codec` field: always Parquet
    /// (ADR-0050 §2), independent of the build's new-store default.
    pub(crate) fn absent() -> Self {
        Self::Parquet
    }

    /// The store-relative directory holding this codec's WAL segments:
    /// `wal/` for the classic codecs, `.cypherlite/wal/` for a markdown
    /// store — the engine sidecar lives under `.cypherlite/` beside the
    /// vault's own files (ADR-0406 §3).
    pub const fn wal_dir(self) -> &'static str {
        match self {
            Self::Parquet | Self::Json | Self::Rkyv => "wal",
            Self::Markdown => super::markdown::WAL_DIR,
        }
    }

    /// The store-relative file this codec publishes its checkpoint
    /// envelope to: `manifest.json` (Parquet, ADR-0050), `graph.json`
    /// (JSON), and the `.cypherlite/manifest.json` sidecar a markdown
    /// store publishes last (ADR-0406 §3). The classic arms are spelled
    /// through the same constants their writers consult
    /// ([`crate::snapshot::MANIFEST_JSON`], [`super::json::GRAPH_JSON`]),
    /// so every arm of this one typed layout fact derives from the
    /// vocabulary its consumers pin.
    pub const fn checkpoint_image(self) -> &'static str {
        match self {
            Self::Parquet | Self::Rkyv => crate::snapshot::MANIFEST_JSON,
            Self::Json => super::json::GRAPH_JSON,
            Self::Markdown => super::markdown::MANIFEST,
        }
    }

    /// The store-relative directory holding this codec's engine state:
    /// the markdown codec's hidden `.cypherlite/`, or
    /// `None` — never a sentinel string — for the snapshot-file codecs,
    /// whose engine state lives at the store root. Beside
    /// [`wal_dir`](Self::wal_dir) and [`checkpoint_image`](Self::checkpoint_image):
    /// the one typed layout fact every engine-dir consumer consults.
    pub const fn engine_dir(self) -> Option<&'static str> {
        match self {
            Self::Markdown => Some(super::markdown::ENGINE_DIR),
            Self::Parquet | Self::Json | Self::Rkyv => None,
        }
    }

    /// Whether this codec persists the graph as a markdown vault rather
    /// than a snapshot container — the fact a measurement or tooling loop
    /// keys its container work on, never `engine_dir`, which names where
    /// a codec's engine state lives.
    pub const fn writes_vault(self) -> bool {
        matches!(self, Self::Markdown)
    }

    /// Whether this codec's manifest writes the wide-column part pair
    /// (ADR-0553) — the codec half of the version a manifest checkpoint
    /// publishes, beside the registry's decoded-version predicate
    /// (`version::manifest::writes_wide`). The experimental codecs keep
    /// the legacy part encoding (ADR-0537). Manifest-bearing builds only:
    /// without one, no manifest is written and the fact has no reader.
    #[cfg(manifest_codec)]
    pub(crate) const fn writes_wide_parts(self) -> bool {
        matches!(self, Self::Parquet)
    }

    /// Every compiled codec that writes a snapshot container (a vault
    /// writer is excluded).
    pub fn compiled_snapshot_containers() -> impl Iterator<Item = SnapshotCodecKind> {
        Self::compiled()
            .iter()
            .copied()
            .filter(|kind| !kind.writes_vault())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parquet => "parquet",
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::Rkyv => "rkyv",
        }
    }

    pub fn parse_name(s: &str) -> Result<Self> {
        s.parse()
    }
}

impl FromStr for SnapshotCodecKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        // The inverse of `as_str`, derived from the one vocabulary table:
        // `ALL` searched by `as_str`, so a fourth codec edits the table
        // and `as_str` alone and this parse follows — no second name
        // match to keep in lockstep. The error quotes the caller's
        // original spelling.
        let name = s.trim().to_ascii_lowercase();
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == name)
            .ok_or_else(|| {
                Error::storage(format!(
                    "unknown snapshot codec '{s}' (expected {})",
                    Self::expected_names(),
                ))
            })
    }
}

impl std::fmt::Display for SnapshotCodecKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
