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
    /// the WAL and the engine sidecar under `.relate/`.
    Markdown,
    /// EXPERIMENTAL (default off, `rkyv-codec`): one zero-copy rkyv
    /// archive. Measurement only until an ADR promotes it.
    Rkyv,
    /// EXPERIMENTAL: node/relationship rows in row-range Vortex shards
    /// (ADR-0665). Installed at runtime by the experiment crate (ADR-0677);
    /// measurement only until the engine-side A/B promotes it.
    Vortex,
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
    /// [`Self::feature_name`], [`Self::unavailable_hint`]).
    pub(crate) const ALL: [Self; 5] = [
        Self::Parquet,
        Self::Json,
        Self::Markdown,
        Self::Rkyv,
        Self::Vortex,
    ];

    /// Whether this codec is compiled into the build. JSON always is;
    /// Parquet needs the `parquet` feature on a non-wasm32 target (ADR-0105);
    /// Vortex needs the experiment crate to have installed its shard codec
    /// through the registry (ADR-0665).
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
    pub fn is_compiled(self) -> bool {
        match self {
            Self::Parquet => cfg!(parquet_codec),
            Self::Rkyv => cfg!(rkyv_codec),
            Self::Vortex => super::shard_codec::installed(),
            Self::Json | Self::Markdown => true,
        }
    }

    /// The Cargo feature that compiles this codec in — the name the
    /// not-compiled error tells the user to enable. Always-compiled
    /// codecs have none, and neither has Vortex: its shard codec arrives
    /// from the experiment crate through the registry, so its install
    /// story rides [`unavailable_hint`](Self::unavailable_hint) instead.
    pub const fn feature_name(self) -> Option<&'static str> {
        match self {
            Self::Parquet => Some("parquet"),
            Self::Rkyv => Some("rkyv-codec"),
            Self::Vortex | Self::Json | Self::Markdown => None,
        }
    }

    /// Where a codec without a Cargo feature comes from — its install
    /// story, the second half of [`unavailable_hint`](Self::unavailable_hint).
    /// A codec's provenance sits beside its [`feature_name`](Self::feature_name)
    /// so one vocabulary row carries both ways a build gets the codec.
    const fn provenance(self) -> Option<&'static str> {
        match self {
            Self::Vortex => Some("link the relate-vortex-lab experiment crate to install it"),
            Self::Parquet | Self::Rkyv | Self::Json | Self::Markdown => None,
        }
    }

    /// How a build that lacks this codec gets it — the parenthetical of
    /// [`codec_unavailable`](super::codec_unavailable), rendered from the
    /// codec's own two-part story (a Cargo feature, or a provenance) so no
    /// caller special-cases a codec. Always-compiled codecs are
    /// unreachable behind [`is_compiled`](Self::is_compiled); their row
    /// still answers instead of panicking.
    pub(crate) fn unavailable_hint(self) -> String {
        match (self.feature_name(), self.provenance()) {
            (Some(feature), _) => format!("enable the \"{feature}\" feature"),
            (None, Some(provenance)) => provenance.to_owned(),
            (None, None) => format!("{} is always compiled", self.as_str()),
        }
    }

    /// The compiled codecs whose engine state lives at the store root —
    /// the set the layout-asserting suites iterate: the markdown codec
    /// keeps its engine state in the `.relate/` sidecar
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
        // Read live, never frozen at first call: Vortex's shard codec is
        // installed at runtime, so a snapshot taken before the install
        // would contradict `is_compiled`. The set changes only when a
        // runtime codec installs (at most once per build), so the leaked
        // slice is rebuilt only on that change and otherwise reused.
        static LIVE: std::sync::Mutex<&'static [SnapshotCodecKind]> = std::sync::Mutex::new(&[]);
        let mut published = LIVE.lock().expect("compiled codec set");
        // Filtered, not sliced: with more than one optional codec a
        // prefix slice would misreport the build. `ALL`'s order is the
        // new-store preference order, so the filter keeps `[0]` the
        // default.
        let fresh: Vec<SnapshotCodecKind> = Self::ALL
            .iter()
            .copied()
            .filter(|kind| kind.is_compiled())
            .collect();
        if *published != fresh.as_slice() {
            let leaked: &'static [SnapshotCodecKind] = Vec::leak(fresh);
            *published = leaked;
        }
        *published
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
    /// `wal/` for the classic codecs, `.relate/wal/` for a markdown
    /// store — the engine sidecar lives under `.relate/` beside the
    /// vault's own files (ADR-0406 §3).
    pub const fn wal_dir(self) -> &'static str {
        match self {
            Self::Parquet | Self::Json | Self::Rkyv | Self::Vortex => "wal",
            Self::Markdown => super::markdown::WAL_DIR,
        }
    }

    /// The store-relative file this codec publishes its checkpoint
    /// envelope to: `manifest.json` (Parquet, ADR-0050), `graph.json`
    /// (JSON), and the `.relate/manifest.json` sidecar a markdown
    /// store publishes last (ADR-0406 §3). The classic arms are spelled
    /// through the same constants their writers consult
    /// ([`crate::snapshot::MANIFEST_JSON`], [`super::json::GRAPH_JSON`]),
    /// so every arm of this one typed layout fact derives from the
    /// vocabulary its consumers pin.
    pub const fn checkpoint_image(self) -> &'static str {
        match self {
            Self::Parquet | Self::Rkyv | Self::Vortex => crate::snapshot::MANIFEST_JSON,
            Self::Json => super::json::GRAPH_JSON,
            Self::Markdown => super::markdown::MANIFEST,
        }
    }

    /// The store-relative objects a checkpoint of this codec republishes
    /// at the root, [`checkpoint_image`](Self::checkpoint_image) among
    /// them: the JSON and rkyv codecs keep the Parquet-compatible
    /// `manifest.json` envelope beside their payload object.
    pub const fn checkpoint_objects(self) -> &'static [&'static str] {
        match self {
            Self::Parquet => &[crate::snapshot::MANIFEST_JSON],
            Self::Json => &[crate::snapshot::MANIFEST_JSON, super::json::GRAPH_JSON],
            Self::Rkyv => &[crate::snapshot::MANIFEST_JSON, super::GRAPH_RKYV],
            // The payload manifest names the row shards, whose set is
            // dynamic; the static root objects are these two.
            Self::Vortex => &[crate::snapshot::MANIFEST_JSON, super::GRAPH_VORTEX_META],
            Self::Markdown => &[super::markdown::MANIFEST],
        }
    }

    /// The store-relative directory holding this codec's engine state:
    /// the markdown codec's hidden `.relate/`, or
    /// `None` — never a sentinel string — for the snapshot-file codecs,
    /// whose engine state lives at the store root. Beside
    /// [`wal_dir`](Self::wal_dir) and [`checkpoint_image`](Self::checkpoint_image):
    /// the one typed layout fact every engine-dir consumer consults.
    pub const fn engine_dir(self) -> Option<&'static str> {
        match self {
            Self::Markdown => Some(super::markdown::ENGINE_DIR),
            Self::Parquet | Self::Json | Self::Rkyv | Self::Vortex => None,
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
            Self::Vortex => "vortex",
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
