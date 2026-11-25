//! The front's interpreter of the preview registry's typed gate sites
//! (ADR-0371, ADR-0662): which `ParseOptions` the enabled features switch on,
//! and the admission loop that fails closed on a detected gated construct.

use relate_core::{DetectorKind, GateSite, ParseOption, PreviewFeatures, Result, PREVIEW_FEATURES};
use relate_error::EngineError;
use relate_parser::lexer::ParseOptions;
use relate_parser::Statement;

use crate::{local_callables, validate};

/// Switch on the `ParseOptions` field a parse-site feature names. Total over
/// [`ParseOption`]: a new option must say which field it sets.
fn apply_parse_option(option: ParseOption, options: &mut ParseOptions) {
    match option {
        ParseOption::ObfuscatedLiterals => options.obfuscated_literals = true,
        ParseOption::MixedLabelSyntax => options.mixed_label_syntax = true,
        ParseOption::CypherAnnotations => options.cypher_annotations = true,
        ParseOption::PathConstruction => options.path_construction = true,
    }
}

/// Run the AST detector a feature's `AstDetector` site names. Total over
/// [`DetectorKind`].
pub(crate) fn detector_uses(
    kind: DetectorKind,
    stmt: &Statement,
) -> std::result::Result<bool, EngineError> {
    match kind {
        DetectorKind::HybridSearch => validate::statement_uses_hybrid_search(stmt),
        DetectorKind::LocalCallables => local_callables::statement_uses_local_callables(stmt),
        DetectorKind::Float16 => validate::statement_uses_float16_coordinate_types(stmt),
        DetectorKind::PathConstruction => validate::statement_uses_path_construction(stmt),
        DetectorKind::Chronological => validate::statement_uses_chronological_comparisons(stmt),
    }
}

/// The parse-affecting options derived from the store's preview set: each
/// enabled feature's `ParseOption` sites set their `ParseOptions` field.
pub(crate) fn parse_options(preview: PreviewFeatures) -> ParseOptions {
    let mut options = ParseOptions::default();
    for feature in PREVIEW_FEATURES {
        if !preview.enabled(*feature) {
            continue;
        }
        for site in feature.gate_sites() {
            match site {
                GateSite::ParseOption(option) => apply_parse_option(*option, &mut options),
                // Detected after parsing ([`require_detected_gates`]) or by a
                // consumer outside the front.
                GateSite::AstDetector(_)
                | GateSite::GraphTypeMembership
                | GateSite::NameAdmission
                | GateSite::ProcedureDispatch
                | GateSite::Namespace
                | GateSite::GatedElsewhere => {}
            }
        }
    }
    options
}

/// The one gate loop, derived from the registry itself: each row's sites say
/// which checks run for it, and the match over the site vocabulary is
/// exhaustive, so a new site kind must say here whether it has an
/// admission-side consumer. A gated construct whose row declares no detecting
/// site is not detected here: the registry row is where a new gate is
/// declared. The graph-type family's AST walk runs once, before the loop, and
/// its `GraphTypeMembership` arm reports membership instead of a second walk.
pub(crate) fn require_detected_gates(stmt: &Statement, preview: PreviewFeatures) -> Result<()> {
    let graph_type_gates =
        validate::statement_graph_type_gates(stmt).map_err(local_callables::walker_error)?;
    for feature in PREVIEW_FEATURES {
        for site in feature.gate_sites() {
            let uses = match site {
                GateSite::AstDetector(kind) => {
                    detector_uses(*kind, stmt).map_err(local_callables::walker_error)?
                }
                GateSite::GraphTypeMembership => graph_type_gates.contains(feature),
                // Decided before parsing ([`parse_options`]) or by a consumer
                // outside the front (function admission, procedure dispatch,
                // the namespace mapping, the evaluator).
                GateSite::ParseOption(_)
                | GateSite::NameAdmission
                | GateSite::ProcedureDispatch
                | GateSite::Namespace
                | GateSite::GatedElsewhere => false,
            };
            if uses {
                preview.require(*feature)?;
            }
        }
    }
    Ok(())
}
