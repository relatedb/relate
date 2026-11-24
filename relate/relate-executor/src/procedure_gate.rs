//! The one built-in-procedure → preview-feature mapping (ADR-0378,
//! ADR-0610).
//!
//! The `db.ontology.*` procedure family is gated behind ADR-0371's
//! `RELATE_PREVIEW` opt-in, and `db.transactions` behind the
//! `value-provenance` row with its whole machinery (ADR-0610: retention
//! changes what a store persists, so no part of the feature can be on
//! alone). Both consumers of that fact — the facade's plan gate
//! (`relate::gate`) and the `SHOW PROCEDURES` catalog (`show_catalog`) —
//! read this mapping, and the mapping itself derives the ontology family
//! from the canonical registry predicate ([`is_ontology_procedure`]) and
//! the one gated name from the built-in registry, so the gate never
//! re-spells either.

use relate_core::PreviewFeature;

use crate::procedure_registry::{builtin_procedure_signature, is_ontology_procedure};

/// The preview feature gating procedure `name` (ADR-0378, ADR-0610):
/// `Some(PreviewFeature::Ontology)` when `name` resolves to one of the
/// built-in `db.ontology.*` registry signatures,
/// `Some(PreviewFeature::ValueProvenance)` for the built-in
/// `db.transactions` signature, `None` for every other name. The lookup
/// is by name against the built-in registry only and never consults the
/// host procedure registry, so a user-registered procedure is not gated —
/// a host `db.ontology.custom`, for example, resolves to no built-in
/// signature and stays ungated.
pub fn procedure_preview_feature(name: &str) -> Option<PreviewFeature> {
    if is_ontology_procedure(name) {
        return Some(PreviewFeature::Ontology);
    }
    builtin_procedure_signature(name)
        .filter(|signature| signature.name == "db.transactions")
        .map(|_| PreviewFeature::ValueProvenance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedure_registry::BUILTIN_PROCEDURE_SIGNATURES;

    #[test]
    fn the_gated_names_are_exactly_the_ontology_family_and_db_transactions() {
        // The gated names, spelled literally: this is the independent fact
        // the test pins, not a re-run of the predicate the mapping is
        // defined from, so a mapping change that moved a name in or out of
        // the family fails here.
        let family = [
            ("db.ontology.set", Some(PreviewFeature::Ontology)),
            ("db.ontology.get", Some(PreviewFeature::Ontology)),
            ("db.ontology.drop", Some(PreviewFeature::Ontology)),
            ("db.ontology.apply", Some(PreviewFeature::Ontology)),
            ("db.transactions", Some(PreviewFeature::ValueProvenance)),
        ];
        for (name, expected) in family {
            assert_eq!(procedure_preview_feature(name), expected, "{name}");
        }
        // A non-gated name is ungated, and so is a host-registered
        // `db.ontology.*` name: the mapping reads the built-in registry
        // only.
        assert_eq!(procedure_preview_feature("db.labels"), None);
        assert_eq!(procedure_preview_feature("db.ontology.custom"), None);
        // No other registry row is gated either: the gated set is exactly
        // the five literal names above, so a registry row added or dropped
        // without the matching mapping change fails here.
        let gated: Vec<&str> = BUILTIN_PROCEDURE_SIGNATURES
            .iter()
            .filter(|signature| procedure_preview_feature(signature.name).is_some())
            .map(|signature| signature.name)
            .collect();
        assert_eq!(
            gated,
            [
                "db.ontology.apply",
                "db.ontology.drop",
                "db.ontology.get",
                "db.ontology.set",
                "db.transactions"
            ]
        );
    }
}
