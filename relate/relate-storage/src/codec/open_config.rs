//! The codec-specific open-time configuration family (ADR-0460,
//! ADR-0534): the one generic configuration slot a store's open carries,
//! resolved alongside the codec and spent by that codec's load and
//! checkpoint only. The enum, its refusal error, its variant facts, and
//! the target's install/projection methods live here; the crate root
//! re-exports the type.

use relate_core::{Error, Result};

use super::markdown::MarkdownBodyProperties;
use super::SnapshotCodecKind;
use crate::StoreTarget;

/// Codec-specific open-time configuration, one generic slot on
/// [`StoreTarget`] (ADR-0460): resolved alongside the codec at open and
/// spent by that codec's load and checkpoint only — `None`, the
/// default, is the recorded/sidecar state every codec falls back to.
/// One variant per codec that grows open-time knobs; the next codec
/// extends this enum, never the target's field list.
#[derive(Clone, Debug)]
pub enum CodecOpenConfig {
    /// The markdown vault's explicit body-property configuration
    /// (ADR-0460); the sidecar manifest's recorded one governs when
    /// absent.
    MarkdownBodies(MarkdownBodyProperties),
    /// The Parquet store opts into the wide-column part encoding
    /// (ADR-0534): its next checkpoint re-encodes every part with one
    /// typed column per promoted property key and publishes a wide
    /// manifest, which then keeps the store wide whatever this flag. The
    /// default, `None`, keeps the tagged-JSON property column.
    #[cfg(parquet_codec)]
    ParquetWide,
}

/// The set-but-ignored error for a codec-specific configuration on a
/// store that does not open as its codec — the one spelling every
/// refusal site in this crate raises, keyed on the configuration's own
/// identity ([`CodecOpenConfig::describe`]). The `named` evidence names
/// a codec when a refusal site actually derived one (the request at the
/// pre-lease refusal, the resolved codec at the installer); `None`
/// states the fact it can prove: the store does not open as the
/// configuration's codec.
pub(crate) fn codec_config_mismatch_error(
    config: &CodecOpenConfig,
    named: Option<SnapshotCodecKind>,
) -> Error {
    let what = config.describe();
    let required = config.required_codec();
    match named {
        Some(codec) => Error::semantic(format!(
            "{what} are set but the store's snapshot codec is {codec}, not {required}; \
             the configuration would be ignored"
        )),
        None => Error::semantic(format!(
            "{what} are set but the store does not open as a {required} vault (no snapshot \
             codec was requested and the directory carries no {required} sidecar); the \
             configuration would be ignored"
        )),
    }
}

impl CodecOpenConfig {
    /// The codec this configuration requires — the one fact every
    /// refusal site consults, owned by the enum instead of hand-spelled
    /// at each consumer.
    pub(crate) fn required_codec(&self) -> SnapshotCodecKind {
        match self {
            CodecOpenConfig::MarkdownBodies(_) => SnapshotCodecKind::Markdown,
            #[cfg(parquet_codec)]
            CodecOpenConfig::ParquetWide => SnapshotCodecKind::Parquet,
        }
    }

    /// The configuration's own validity — the payload's validation,
    /// owned by the enum so every install-or-refuse boundary runs it.
    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            CodecOpenConfig::MarkdownBodies(bodies) => bodies.validate(),
            #[cfg(parquet_codec)]
            CodecOpenConfig::ParquetWide => Ok(()),
        }
    }

    /// The markdown vault's body-property configuration when this config
    /// is one (ADR-0460) — the one projection of the variant, so no
    /// consumer enumerates its siblings.
    pub(crate) fn markdown_bodies(&self) -> Option<&MarkdownBodyProperties> {
        match self {
            CodecOpenConfig::MarkdownBodies(bodies) => Some(bodies),
            #[cfg(parquet_codec)]
            CodecOpenConfig::ParquetWide => None,
        }
    }

    /// Whether this configuration opts the store into the wide-column
    /// part encoding (ADR-0534) — the one projection of the variant, the
    /// same shape [`markdown_bodies`](Self::markdown_bodies) takes.
    #[cfg(parquet_codec)]
    pub(crate) fn wide_columns(&self) -> bool {
        match self {
            CodecOpenConfig::MarkdownBodies(_) => false,
            CodecOpenConfig::ParquetWide => true,
        }
    }

    /// The wide-column opt-in (ADR-0534) — the one constructor, owning
    /// the build fact: the configuration on a build that compiles the
    /// Parquet codec, and a typed refusal on one that does not. The
    /// caller expresses "off" with the flag it holds
    /// (`then(CodecOpenConfig::parquet_wide).transpose()`).
    pub fn parquet_wide() -> Result<Self> {
        #[cfg(parquet_codec)]
        return Ok(CodecOpenConfig::ParquetWide);
        #[cfg(not(parquet_codec))]
        return Err(Error::storage(
            "the wide-columns opt-in needs the Parquet snapshot codec, \
             which this build does not compile",
        ));
    }

    /// Whether an unprovable codec refuses at the pre-lease gate instead
    /// of deferring to the installer — the markdown configuration's
    /// fail-before-lease contract (ADR-0460); the wide-column option
    /// defers (ADR-0534). The variant owns the answer, so a new one
    /// must state it.
    fn demands_proven_codec(&self) -> bool {
        match self {
            CodecOpenConfig::MarkdownBodies(_) => true,
            #[cfg(parquet_codec)]
            CodecOpenConfig::ParquetWide => false,
        }
    }

    /// The configuration's own name in refusal messages — the variant
    /// owns its identity; no refusal site hardcodes it.
    pub(crate) fn describe(&self) -> &'static str {
        match self {
            CodecOpenConfig::MarkdownBodies(_) => "markdown body properties",
            #[cfg(parquet_codec)]
            CodecOpenConfig::ParquetWide => "wide-column checkpoint parts",
        }
    }

    /// The pre-lease half of the codec match, one policy for every
    /// variant: the configuration's own validation, then the codec
    /// match on `visible` — the one codec this open can see before the
    /// lease (a bundle always resolves Parquet; a markdown sidecar or
    /// request proves Markdown; `None` when unprovable, the WAL-layout
    /// hint cannot name Parquet), derived at the call site. The markdown
    /// configuration demands a visible codec (ADR-0460's
    /// fail-before-lease contract); the wide-column option defers an
    /// unseen codec to the installer over the resolved one (ADR-0534),
    /// refusing only a visible contradicting codec here.
    pub(crate) fn ensure_pre_lease(&self, visible: Option<SnapshotCodecKind>) -> Result<()> {
        self.validate()?;
        // The one codec this open can see runs the same install-or-refuse
        // sequence; what differs per variant is only whether an unseen
        // codec refuses here or defers to the installer.
        match visible {
            Some(codec) => self.ensure_matches(codec),
            None if self.demands_proven_codec() => Err(codec_config_mismatch_error(self, None)),
            None => Ok(()),
        }
    }

    /// The one install-or-refuse sequence every boundary runs: the
    /// configuration's own validation, then the codec match against
    /// `opening` — the codec this open resolves, which the refusal
    /// cites.
    pub(crate) fn ensure_matches(&self, opening: SnapshotCodecKind) -> Result<()> {
        self.validate()?;
        if self.required_codec() != opening {
            return Err(codec_config_mismatch_error(self, Some(opening)));
        }
        Ok(())
    }
}

impl StoreTarget {
    /// Install codec-specific open-time configuration
    /// ([`CodecOpenConfig`]). The one installer every codec knob passes
    /// through: the variant's own validation runs here, and a
    /// configuration whose codec does not match the store's is refused —
    /// a set-but-ignored knob is an error, never a silent no-op.
    pub fn with_codec_config(mut self, config: CodecOpenConfig) -> Result<Self> {
        config.ensure_matches(self.codec)?;
        self.codec_config = Some(config);
        Ok(self)
    }

    /// The markdown vault's explicit body-property configuration when
    /// this open carried one (ADR-0460) — the markdown codec's load and
    /// checkpoint read it here; `None` lets the sidecar manifest's
    /// recorded configuration govern.
    pub(crate) fn markdown_bodies(&self) -> Option<&MarkdownBodyProperties> {
        self.codec_config
            .as_ref()
            .and_then(|config| config.markdown_bodies())
    }

    /// Whether this open opts the Parquet store into the wide-column part
    /// encoding (ADR-0534) — the open-time half of the decision, read
    /// through the configuration's own projection; a manifest at the
    /// wide versions keeps the store wide without it.
    #[cfg(parquet_codec)]
    pub(crate) fn wide_columns(&self) -> bool {
        self.codec_config
            .as_ref()
            .is_some_and(CodecOpenConfig::wide_columns)
    }
}
