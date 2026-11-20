//! ADR-0371 preview-gate predicates for the CIP graph-type family
//! (ADR-0388: CIP-283 optional properties, CIP-262 property default values, CIP-266
//! implied labels identifying node element types; CIP-275 `SHOW CURRENT
//! GRAPH TYPE` display variants): the AST-level checks
//! `parse_and_lower_statement_with_preview` runs before anything of a
//! statement lowers or plans. Each predicate answers "does this statement's
//! own text use the gated construct" from the parsed AST alone;
//! constructs that only materialize against *stored* graph-type state
//! (an `ADD` whose new identifying label is already implied by a stored
//! node element type) stay the executor's to fail closed -- the same
//! authored error, produced where the state lives.
//!
//! The predicates are one family over the same body walk: a single
//! collector runs once per admitted statement and yields the
//! `PreviewFeature`s the statement's bodies use, so admission is one
//! loop over that set -- a later graph-type gate is one more arm here,
//! not a fourth tree walk and a fourth admission branch.

use relate_core::PreviewFeature;
use relate_error::EngineError;
use relate_parser::type_normalize::{type_expr_to_canon, CanonKind};
use relate_parser::{Clause, GraphTypeBody, GraphTypeNodeTypeSpec, Statement};

use crate::ast::tree::walk_statement_tree;

/// The preview features a statement's graph-type bodies use, in registry
/// order (the first arm that finds a construct records it), deduplicated
/// -- the set admission loops over, once per statement.
pub(crate) type GraphTypeGates = Vec<PreviewFeature>;

/// Records `feature` once (a construct found again by a later body of
/// the same statement stays one entry).
fn note(gates: &mut GraphTypeGates, feature: PreviewFeature) {
    if !gates.contains(&feature) {
        gates.push(feature);
    }
}

/// Walks the statement tree once and collects the preview features its
/// graph-type bodies use. A walk error propagates: the gate never
/// answers "none" over a tree it could not walk.
pub(crate) fn statement_graph_type_gates(stmt: &Statement) -> Result<GraphTypeGates, EngineError> {
    let mut gates: GraphTypeGates = Vec::new();
    let mut visit_clause = |clause: &Clause| -> Result<(), EngineError> {
        if let Some(body) = clause_graph_type_body(clause) {
            inspect_body(body, &mut gates);
        }
        // CIP-275: the display variants are a property of the SHOW
        // clause itself, not of a body.
        if let Clause::ShowCurrentGraphType(show) = clause {
            if show.variant.is_some() {
                note(&mut gates, PreviewFeature::GraphTypeShowVariants);
            }
        }
        Ok(())
    };
    walk_statement_tree(stmt, &mut |_| Ok(()), &mut visit_clause, &mut |_| Ok(()))?;
    Ok(gates)
}

/// The graph-type body a clause carries: `CREATE GRAPH TYPE`'s own body
/// and `ALTER CURRENT GRAPH TYPE`'s inline body (the `Named` target has
/// no body of its own). Every other clause carries none.
pub(super) fn clause_graph_type_body(clause: &Clause) -> Option<&GraphTypeBody> {
    match clause {
        Clause::CreateGraphType(c) => Some(&c.body),
        Clause::AlterCurrentGraphType(c) => match &c.target {
            relate_parser::AlterCurrentGraphTypeTarget::Body(body) => Some(body),
            relate_parser::AlterCurrentGraphTypeTarget::Named(_) => None,
        },
        _ => None,
    }
}

/// Folds one body's element types into `gates`: a bare `ANY` value type
/// or a `DEFAULT ON COMMIT` clause on any element-type property, and an
/// implied label naming another in-body node element type's identifying
/// label.
fn inspect_body(body: &GraphTypeBody, gates: &mut GraphTypeGates) {
    for node in &body.node_types {
        inspect_properties(&node.properties, gates);
    }
    for edge in &body.edge_types {
        inspect_properties(&edge.properties, gates);
    }
    for node in &body.node_types {
        if node_implied_labels_identify(body, node) {
            note(gates, PreviewFeature::GraphTypeImpliedLabels);
        }
    }
}

/// One element type's property list: the two per-property constructs.
fn inspect_properties(props: &[relate_parser::GraphTypePropertyType], gates: &mut GraphTypeGates) {
    for p in props {
        if p.default_value.is_some() {
            note(gates, PreviewFeature::GraphTypeDefaultValues);
        }
        let canon = type_expr_to_canon(&p.value_type);
        if matches!(
            canon.kind,
            CanonKind::Predefined(relate_parser::PredefinedKind::Any)
        ) && !canon.not_null
        {
            note(gates, PreviewFeature::GraphTypeOptProps);
        }
    }
}

/// Whether one of `node`'s implied labels names another *in-body* node
/// element type's identifying label (CIP-266 inheritance). The implied
/// label's own node element type -- declared by this same statement or by
/// a later one -- is what makes the label identify an element type in the
/// merged graph type; the in-body half is what the AST can see.
fn node_implied_labels_identify(body: &GraphTypeBody, node: &GraphTypeNodeTypeSpec) -> bool {
    node.implied_labels.iter().any(|implied| {
        body.node_types
            .iter()
            .any(|other| other.identifying_label == *implied && !std::ptr::eq(other, node))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_core::{Error, PreviewFeature, PreviewFeatures};
    use relate_parser::parse;

    fn gates_of(query: &str) -> GraphTypeGates {
        let stmt = parse(query).expect("parses");
        statement_graph_type_gates(&stmt).expect("walks")
    }

    #[test]
    fn the_collector_finds_each_gated_construct() {
        // A plain body gates nothing.
        let gates = gates_of("ALTER CURRENT GRAPH TYPE SET { (:L => {p :: STRING}) }");
        assert!(gates.is_empty());
        // CIP-283: the bare ANY element-type property.
        let gates = gates_of("ALTER CURRENT GRAPH TYPE SET { (:L => {p :: ANY}) }");
        assert_eq!(gates, vec![PreviewFeature::GraphTypeOptProps]);
        // CIP-262: the DEFAULT ON COMMIT clause.
        let gates = gates_of(
            "ALTER CURRENT GRAPH TYPE SET { (:L => {p :: STRING DEFAULT ON COMMIT 'x'}) }",
        );
        assert_eq!(gates, vec![PreviewFeature::GraphTypeDefaultValues]);
        // CIP-266: an implied label naming an in-body identifying label.
        let gates = gates_of("ALTER CURRENT GRAPH TYPE SET { (:A => {p :: STRING}), (:B => :A) }");
        assert_eq!(gates, vec![PreviewFeature::GraphTypeImpliedLabels]);
        // One statement using two gated constructs records both, once.
        let gates = gates_of(
            "ALTER CURRENT GRAPH TYPE SET { (:A => {p :: ANY}), (:B => :A {p :: STRING}) }",
        );
        assert_eq!(
            gates,
            vec![
                PreviewFeature::GraphTypeOptProps,
                PreviewFeature::GraphTypeImpliedLabels,
            ]
        );
        // `ANY NOT NULL` is not the optional property; a plain implied
        // label gates nothing.
        let gates = gates_of("ALTER CURRENT GRAPH TYPE SET { (:A => {p :: ANY NOT NULL}) }");
        assert!(gates.is_empty());
        // An independent property type constraint with ANY is not the
        // element-type construct either (it stays its own rejection).
        let gates = gates_of(
            "ALTER CURRENT GRAPH TYPE SET { \
             CONSTRAINT FOR (n:L) REQUIRE n.p IS :: STRING }",
        );
        assert!(gates.is_empty());
    }

    #[test]
    fn the_collector_finds_the_show_variants() {
        // CIP-275: either variant, with or without `AS GRAPH`, records the
        // feature once; the plain forms gate nothing.
        for query in [
            "SHOW CURRENT GRAPH TYPE AS CONSOLIDATED",
            "SHOW CURRENT GRAPH TYPE AS FLATTENED",
            "SHOW CURRENT GRAPH TYPE AS CONSOLIDATED AS GRAPH",
            "SHOW CURRENT GRAPH TYPE AS FLATTENED AS GRAPH SINGLE YIELD nodes",
        ] {
            assert_eq!(
                gates_of(query),
                vec![PreviewFeature::GraphTypeShowVariants],
                "{query}"
            );
        }
        for query in [
            "SHOW CURRENT GRAPH TYPE",
            "SHOW CURRENT GRAPH TYPE AS GRAPH",
            "SHOW CURRENT GRAPH TYPE YIELD specification",
        ] {
            assert!(gates_of(query).is_empty(), "{query}");
        }
    }

    #[test]
    fn admission_fails_closed_for_the_show_variants_ungated() {
        // The authored error, never a parse error leaking the syntax; the
        // plain forms are unaffected, and each variant runs behind its gate.
        let name = "graph-type-show-variants";
        for query in [
            "SHOW CURRENT GRAPH TYPE AS CONSOLIDATED",
            "SHOW CURRENT GRAPH TYPE AS FLATTENED",
            "SHOW CURRENT GRAPH TYPE AS CONSOLIDATED AS GRAPH",
            "SHOW CURRENT GRAPH TYPE AS FLATTENED AS GRAPH SEPARATE",
            "SHOW CURRENT GRAPH TYPE AS FLATTENED YIELD specification WHERE true",
        ] {
            let error =
                match crate::parse_and_lower_statement_with_preview(query, PreviewFeatures::none())
                {
                    Ok(_) => panic!("{query}: the variant must fail closed ungated"),
                    Err(error) => error,
                };
            let rendered = error.to_string();
            assert!(
                rendered.contains(&format!(
                    "preview feature '{name}' is not enabled: set RELATE_PREVIEW={name}"
                )),
                "{query}: {rendered}"
            );
            assert!(matches!(error, Error::Semantic(_)), "{rendered}");
            let enabled = PreviewFeatures::parse(name).expect("a registered feature name");
            crate::parse_and_lower_statement_with_preview(query, enabled)
                .unwrap_or_else(|e| panic!("{query}: {e}"));
        }
        for query in [
            "SHOW CURRENT GRAPH TYPE",
            "SHOW CURRENT GRAPH TYPE AS GRAPH",
        ] {
            crate::parse_and_lower_statement_with_preview(query, PreviewFeatures::none())
                .unwrap_or_else(|e| panic!("{query}: {e}"));
        }
    }

    #[test]
    fn admission_fails_closed_with_the_authored_error_ungated() {
        // The ADR-0371 authored error, one gate at a time: each construct
        // requires its own feature before anything of it lowers.
        let cases = [
            (
                "ALTER CURRENT GRAPH TYPE SET { (:L => {p :: ANY}) }",
                PreviewFeature::GraphTypeOptProps,
                "graph-type-opt-props",
            ),
            (
                "ALTER CURRENT GRAPH TYPE SET { \
                 (:L => {p :: STRING DEFAULT ON COMMIT 'x'}) }",
                PreviewFeature::GraphTypeDefaultValues,
                "graph-type-default-values",
            ),
            (
                "ALTER CURRENT GRAPH TYPE SET { (:A => {p :: STRING}), (:B => :A) }",
                PreviewFeature::GraphTypeImpliedLabels,
                "graph-type-implied-labels",
            ),
        ];
        for (query, feature, name) in cases {
            let error =
                match crate::parse_and_lower_statement_with_preview(query, PreviewFeatures::none())
                {
                    Ok(_) => panic!("the gated construct must fail closed ungated"),
                    Err(error) => error,
                };
            let rendered = error.to_string();
            assert!(
                rendered.contains(&format!(
                    "preview feature '{name}' is not enabled: \
                     set RELATE_PREVIEW={name}"
                )),
                "{rendered}"
            );
            assert!(matches!(error, Error::Semantic(_)), "{rendered}");
            // Enabled, the statement lowers past the gate.
            let enabled = PreviewFeatures::parse(name).expect("a registered feature name");
            crate::parse_and_lower_statement_with_preview(query, enabled)
                .expect("lowers behind its gate");
            let _ = feature;
        }
    }
}
