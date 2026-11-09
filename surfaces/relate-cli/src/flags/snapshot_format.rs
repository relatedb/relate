//! The `--snapshot-format` value (ADR-0163): the grammar and the
//! accepted-value list every help row and usage-error text renders — one
//! owner beside the parser, so the vocabulary cannot drift between them.

/// Parse one `--snapshot-format` value: a snapshot codec name. An
/// uncompiled choice parses here and fails closed at open (ADR-0105),
/// like every codec spelling. The flag's one vocabulary owns its
/// unknown-value error: the expected list is what this build's
/// [`codec_names`] renders — never the codec parser's narrower set.
pub(crate) fn parse(value: &str) -> Result<relate::SnapshotCodecKind, relate::Error> {
    value.parse::<relate::SnapshotCodecKind>().map_err(|_| {
        relate::Error::storage(format!(
            "unknown snapshot format '{value}' (expected {})",
            codec_names(" or ")
        ))
    })
}

/// The compiled-codec names, joined by `separator` — the one name
/// derivation the clone page's codec list, this flag's help rows and its
/// "requires" texts share (`help::compiled_codecs` delegates here).
pub(crate) fn codec_names(separator: &str) -> String {
    relate::SnapshotCodecKind::compiled()
        .iter()
        .map(|codec| codec.as_str())
        .collect::<Vec<_>>()
        .join(separator)
}
