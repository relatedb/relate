//! The `--snapshot-format` value (ADR-0163, ADR-0534): the grammar, the
//! one `parquet-wide` spelling, and the accepted-value list every help
//! row and usage-error text renders — one owner beside the parser, so
//! the vocabulary cannot drift between them.

/// The `--snapshot-format` value: a snapshot codec, or the Parquet
/// codec's wide-column part encoding — an option on Parquet, never a
/// codec of its own. An uncompiled choice parses here and fails closed
/// at open (ADR-0105), like every codec spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotFormat {
    Codec(relate::SnapshotCodecKind),
    /// `parquet-wide`: the Parquet codec opted into wide-column checkpoint
    /// parts (ADR-0534) — the next checkpoint re-encodes every part and
    /// the manifest keeps the store wide.
    ParquetWide,
}

/// The one `parquet-wide` spelling, beside the codec vocabulary it
/// extends.
pub(crate) const PARQUET_WIDE: &str = "parquet-wide";

impl std::str::FromStr for SnapshotFormat {
    type Err = relate::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim().eq_ignore_ascii_case(PARQUET_WIDE) {
            return Ok(Self::ParquetWide);
        }
        // The flag's one vocabulary owns its unknown-value error: the
        // expected list is what this build's `values` renders — the
        // codec names plus, in a Parquet build, the opt-in — never the
        // codec parser's narrower set.
        value
            .parse::<relate::SnapshotCodecKind>()
            .map(Self::Codec)
            .map_err(|_| {
                relate::Error::storage(format!(
                    "unknown snapshot format '{value}' (expected {})",
                    values(" or ")
                ))
            })
    }
}

impl SnapshotFormat {
    /// The codec the open requests.
    pub(crate) fn codec(self) -> relate::SnapshotCodecKind {
        match self {
            Self::Codec(codec) => codec,
            Self::ParquetWide => relate::SnapshotCodecKind::Parquet,
        }
    }

    /// Whether the open opts into the wide-column part encoding
    /// (ADR-0534).
    pub(crate) fn wide_columns(self) -> bool {
        matches!(self, Self::ParquetWide)
    }

    /// The open knobs the format resolves to — the codec it requests and
    /// the wide-column opt-in — the one projection both the query open
    /// and serve's shared open read.
    pub(crate) fn open_knobs(self) -> (relate::SnapshotCodecKind, bool) {
        (self.codec(), self.wide_columns())
    }
}

/// The compiled-codec names, joined by `separator` — the one name
/// derivation the clone page's codec list and this flag's value list
/// share (`help::compiled_codecs` delegates here).
pub(crate) fn codec_names(separator: &str) -> String {
    relate::SnapshotCodecKind::compiled()
        .iter()
        .map(|codec| codec.as_str())
        .collect::<Vec<_>>()
        .join(separator)
}

/// The flag's opt-in clause for a help row: what `parquet-wide` is, in
/// a build that lists it — empty in a build without the Parquet codec,
/// from the same build fact that gates the spelling in [`values`].
#[cfg(feature = "serve")]
pub(crate) fn opt_in_note() -> &'static str {
    if relate::SnapshotCodecKind::Parquet.is_compiled() {
        "; the wide-column opt-in (ADR-0534) among them, the manifest keeping the store wide"
    } else {
        ""
    }
}

/// The values this build accepts, joined by `separator`: the compiled
/// codec names plus — in a build with the Parquet codec, the one typed
/// build fact — the `parquet-wide` opt-in. The one list the flag's help
/// rows and its "requires" texts render.
pub(crate) fn values(separator: &str) -> String {
    let mut values = codec_names(separator);
    if relate::SnapshotCodecKind::Parquet.is_compiled() {
        values.push_str(separator);
        values.push_str(PARQUET_WIDE);
    }
    values
}
