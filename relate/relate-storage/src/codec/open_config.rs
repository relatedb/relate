//! The codec-specific open-time configuration family (ADR-0460): the one
//! generic configuration slot a store's open carries, resolved alongside
//! the codec and spent by that codec's load and checkpoint only. The
//! enum, its refusal error, its variant facts, and the target's
//! install/projection methods live here; the crate root re-exports the
//! type.

use relate_core::{Error, Result};

use super::markdown::MarkdownBodyProperties;
use super::SnapshotCodecKind;
use crate::StoreTarget;

/// Codec-specific open-time configuration, one generic slot on
/// [`StoreTarget`] (ADR-0460): resolved alongside the codec at open and
/// spent by that codec's load and checkpoint only — `None`, the
/// default, is the recorded/sidecar state every codec falls back to.
/// One variant per codec that grows open-time knobs; the next codec
/// extends this enum, never the target's field list. ADR-0460's recorded
/// extension seam — it carried the Parquet wide-column variant until
/// ADR-0553 deleted that opt-in, and it stays the recorded shape for the
/// next codec knob rather than collapsing into its one payload.
#[derive(Clone, Debug)]
pub enum CodecOpenConfig {
    /// The markdown vault's explicit body-property configuration
    /// (ADR-0460); the sidecar manifest's recorded one governs when
    /// absent.
    MarkdownBodies(MarkdownBodyProperties),
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
        }
    }

    /// The configuration's own validity — the payload's validation,
    /// owned by the enum so every install-or-refuse boundary runs it.
    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            CodecOpenConfig::MarkdownBodies(bodies) => bodies.validate(),
        }
    }

    /// The markdown vault's body-property configuration when this config
    /// is one (ADR-0460) — the one projection of the variant, so no
    /// consumer enumerates its siblings.
    pub(crate) fn markdown_bodies(&self) -> Option<&MarkdownBodyProperties> {
        match self {
            CodecOpenConfig::MarkdownBodies(bodies) => Some(bodies),
        }
    }

    /// The configuration's own name in refusal messages — the variant
    /// owns its identity; no refusal site hardcodes it.
    pub(crate) fn describe(&self) -> &'static str {
        match self {
            CodecOpenConfig::MarkdownBodies(_) => "markdown body properties",
        }
    }

    /// The pre-lease half of the codec match, one policy for every
    /// variant: the configuration's own validation, then the codec
    /// match on `visible` — the one codec this open can see before the
    /// lease (a bundle always resolves Parquet; a markdown sidecar or
    /// request proves Markdown; `None` when unprovable, the WAL-layout
    /// hint cannot name Parquet), derived at the call site. An unseen
    /// codec refuses here, the fail-before-lease contract (ADR-0460):
    /// the configuration would be ignored, and no installer sees the
    /// open.
    pub(crate) fn ensure_pre_lease(&self, visible: Option<SnapshotCodecKind>) -> Result<()> {
        self.validate()?;
        match visible {
            Some(codec) => self.ensure_matches(codec),
            None => Err(codec_config_mismatch_error(self, None)),
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
}
