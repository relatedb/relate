//! The preview-feature gate fixture (ADR-0371): the store-level set that
//! `RELATE_PREVIEW` resolves at open, wired through the same
//! fail-closed gate the registry pins in `relate-core`. The first
//! gated feature is the `db.ontology.*` procedure family (ADR-0378,
//! `RELATE_PREVIEW=ontology`); the fixture calls the resolved set
//! directly so every registry row stays pinned.

use relate_core::{GateSite, Namespace, PreviewFeature, PreviewFeatures, PREVIEW_FEATURES};
use relate_executor::{procedure_preview_feature, BUILTIN_PROCEDURE_SIGNATURES};
use relate_front::validate::{function_passes_name_gate, gated_function_names};

use crate::Database;

/// An open store with no `RELATE_PREVIEW` opt-in gates every preview
/// feature closed with the authored disabled-feature error.
#[test]
fn gate_fails_closed_without_the_opt_in() {
    // The precondition the whole fixture rests on: a dev shell exporting
    // RELATE_PREVIEW changes what `Database::open` resolves — fail loudly
    // instead through the one shared predicate this fixture and the
    // `ontology_preview_gate` integration crate both call.
    crate::test_opts::assert_preview_unset();
    let db = Database::open(":memory:").unwrap();
    // ADR-0378: the resolved set is stored once, on the graph's
    // `StoreServices` bundle that the `SHOW PROCEDURES` catalog and the
    // `CALL` gate read; open installs it before the bundle is shared, so
    // there is no second copy to compare against.
    let read = db.inner.read_guard().unwrap();
    let preview = db.inner.graph.preview();
    drop(read);
    assert_eq!(preview, PreviewFeatures::none());
    for feature in PREVIEW_FEATURES {
        let error = preview.require(*feature).unwrap_err();
        let display = error.to_string();
        let name = feature.kebab_name();
        assert!(
            display.contains(&format!(
                "preview feature '{name}' is not enabled: \
                 set RELATE_PREVIEW={name}"
            )),
            "{name}: {display}"
        );
    }
}

/// The features whose gate a site's consumer actually reads, taken from the
/// consumer's own table or mapping. The `ParseOption` and `AstDetector` consumers
/// live in `relate-front`: its total matches pin them at compile time and its
/// `declared_gate_sites_behave_as_named` test checks their behavior.
fn consumer_features(site: GateSite) -> Option<Vec<PreviewFeature>> {
    let admitted_by_opt_in =
        |feature: PreviewFeature| PreviewFeatures::none().with_feature(feature);
    fn features_where(reads: impl Fn(PreviewFeature) -> bool) -> Vec<PreviewFeature> {
        PREVIEW_FEATURES
            .iter()
            .copied()
            .filter(|feature| reads(*feature))
            .collect()
    }
    match site {
        GateSite::ParseOption(_) | GateSite::AstDetector(_) => None,
        GateSite::NameAdmission => Some(features_where(|feature| {
            gated_function_names().any(|function| {
                function_passes_name_gate(function, admitted_by_opt_in(feature))
                    && !function_passes_name_gate(function, PreviewFeatures::none())
            })
        })),
        GateSite::ProcedureDispatch => Some(features_where(|feature| {
            BUILTIN_PROCEDURE_SIGNATURES
                .iter()
                .any(|signature| procedure_preview_feature(signature.name) == Some(feature))
        })),
        GateSite::Namespace => Some(features_where(|feature| {
            Namespace::ALL.iter().any(|namespace| {
                namespace.visible_under(admitted_by_opt_in(feature))
                    && !namespace.visible_under(PreviewFeatures::none())
            })
        })),
    }
}

/// For every site with a consumer outside the front, the features its
/// consumer reads equal the features whose rows declare it: a row naming a
/// site its consumer ignores, a consumer reading a feature whose row omits the
/// site, or a row dropping the last declaration of a site, fails here. The
/// sites are listed here, not read from the declarations, so a site no row
/// declares any more is still checked; `consumer_features` matches the
/// vocabulary exhaustively, so a new site kind is a compile error there.
#[test]
fn every_gate_site_matches_its_consumer() {
    for site in [
        GateSite::NameAdmission,
        GateSite::ProcedureDispatch,
        GateSite::Namespace,
    ] {
        let consumer = consumer_features(site).expect("these sites have a consumer here");
        let declared: Vec<PreviewFeature> = PREVIEW_FEATURES
            .iter()
            .copied()
            .filter(|feature| feature.gate_sites().contains(&site))
            .collect();
        assert_eq!(declared, consumer, "{site:?}");
    }
}
