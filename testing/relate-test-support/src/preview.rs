//! The process-wide preview-opt-in test fixture (ADR-0378, ADR-0593).
//!
//! `Database::open` reads `RELATE_PREVIEW` once per open, and a test
//! binary runs its tests on threads sharing one environment — so the
//! opt-in is set exactly once per process, under a `Once`, and no test
//! ever toggles the variable back. The `relate` crate's tests/common
//! `preview_ontology` is the pattern's origin; this module is the one
//! shared home every crate's test fixture calls, so no surface grows a
//! bespoke copy.

/// Set `RELATE_PREVIEW` to `features` for this test process, at most
/// once — the first call wins and later calls are no-ops, so a binary
/// must not need two different sets. Call before the fixture's first
/// `Database::open`; the variable is read once, at open.
pub fn set_preview_once(features: &str) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var("RELATE_PREVIEW", features));
}

/// Opt this test process into the `ontology` preview feature
/// (ADR-0378/ADR-0593): the fixed-argument spelling of
/// [`set_preview_once`], for the feature row gating the `db.ontology.*`
/// procedures and the `system_ontologies` namespace visibility.
pub fn preview_ontology() {
    set_preview_once("ontology");
}
