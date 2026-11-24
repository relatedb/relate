use super::*;

/// The registry's recorded gate sites agree with the front's actual
/// behavior, per row: a Lexer/Parser row changes `ParseOptions` when
/// enabled (others change nothing), and an AstDetector row is exactly
/// a row whose detector fires on its probe statement. The probe table
/// is an exhaustive match, so a new row is a compile error here.
#[test]
fn recorded_gate_sites_agree_with_the_front() {
    use relate_core::{GateSite, PreviewFeature, PreviewFeatures, PREVIEW_FEATURES};
    fn probe(feature: PreviewFeature) -> Option<&'static str> {
        match feature {
            PreviewFeature::HybridSearch => Some(COMBINE_QUERY),
            PreviewFeature::LocalCallables => Some("DEFINE FUNCTION f() = 1 RETURN f()"),
            PreviewFeature::Float16CoordinateTypes => Some("RETURN vector([1.0], 1, FLOAT16)"),
            PreviewFeature::PathConstruction => {
                Some("MATCH (a)-[r]->(b) RETURN PATH [a, r, b] AS p")
            }
            PreviewFeature::ChronologicalComparisons => Some("RETURN date() IS BEFORE date()"),
            PreviewFeature::GraphTypeOptProps => {
                Some("ALTER CURRENT GRAPH TYPE SET { (:L => {p :: ANY}) }")
            }
            PreviewFeature::GraphTypeDefaultValues => {
                Some("ALTER CURRENT GRAPH TYPE SET { (:L => {p :: STRING DEFAULT ON COMMIT 'x'}) }")
            }
            PreviewFeature::GraphTypeImpliedLabels => {
                Some("ALTER CURRENT GRAPH TYPE SET { (:A => {p :: STRING}), (:B => :A) }")
            }
            PreviewFeature::GraphTypeShowVariants => {
                Some("SHOW CURRENT GRAPH TYPE AS CONSOLIDATED")
            }
            PreviewFeature::Ontology
            | PreviewFeature::ObfuscatedLiterals
            | PreviewFeature::MixedLabelSyntax
            | PreviewFeature::CollectionZip
            | PreviewFeature::CollectionHas
            | PreviewFeature::CypherAnnotations
            | PreviewFeature::GqlStddevAliases
            | PreviewFeature::PathConcatenation => None,
        }
    }
    for feature in PREVIEW_FEATURES {
        let name = feature.kebab_name();
        let sites = feature.gate_sites();
        let parse_gated = sites.contains(&GateSite::Lexer) || sites.contains(&GateSite::Parser);
        let changed = parse_options(PreviewFeatures::none().with_feature(*feature))
            != relate_parser::lexer::ParseOptions::default();
        assert_eq!(parse_gated, changed, "{name}: parse-site mismatch");
        let probed = match probe(*feature) {
            Some(src) => {
                // The canonical options with the feature enabled, so a
                // probe that needs a parse-time gate still parses.
                let options = parse_options(PreviewFeatures::none().with_feature(*feature));
                let stmt = parse_sized_with_options(src, options).expect("probe parses");
                let gates = validate::statement_graph_type_gates(&stmt).expect("gate walk");
                assert!(
                    statement_uses_feature(*feature, &stmt, &gates).expect("detector runs"),
                    "{name}: detector did not fire on {src}"
                );
                true
            }
            None => false,
        };
        assert_eq!(
            sites.contains(&GateSite::AstDetector),
            probed,
            "{name}: AstDetector site disagrees with the detector"
        );
    }
}

use relate_ir::{Clause, Expr};

#[test]
fn oversized_query_is_rejected() {
    let oversized = "a".repeat(relate_parser::parser::MAX_QUERY_BYTES + 1);
    let error = match parse_and_lower_statement(&oversized) {
        Ok(_) => panic!("oversized query unexpectedly parsed"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("maximum query size"), "{error}");
    assert!(
        error.contains(&relate_parser::parser::MAX_QUERY_BYTES.to_string()),
        "{error}"
    );
}

/// The CIP-278 showcase query, shared by the two preview-gate tests.
const COMBINE_QUERY: &str = "MATCH (node) \
    SEARCH node IN ( \
      COMBINE ( \
        VECTOR INDEX v FOR [1.0] LIMIT 10 WEIGHT 0.7, \
        FULLTEXT INDEX f FOR 'text' LIMIT 10 WEIGHT 0.3 \
      ) USING WRRF WITH CONSTANT 60 \
    ) SCORE AS score \
    RETURN node, score";

#[test]
fn hybrid_search_gate_fails_closed_without_the_opt_in() {
    // ADR-0371: a COMBINE query behind a preview set that does not
    // include `hybrid-search` fails with the authored error, before
    // anything of it lowers.
    let error = match parse_and_lower_statement_with_preview(
        COMBINE_QUERY,
        relate_core::PreviewFeatures::none(),
    ) {
        Ok(_) => panic!("a gated query unexpectedly lowered without the opt-in"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains(
            "preview feature 'hybrid-search' is not enabled: \
             set RELATE_PREVIEW=hybrid-search"
        ),
        "{error}"
    );
    // An ungated query lowers unchanged through the same entry.
    parse_and_lower_statement_with_preview("RETURN 1", relate_core::PreviewFeatures::none())
        .unwrap();
}

#[test]
fn hybrid_search_gate_admits_the_opt_in() {
    let enabled = relate_core::PreviewFeatures::parse("hybrid-search").unwrap();
    let lowered = parse_and_lower_statement_with_preview(COMBINE_QUERY, enabled)
        .expect("the gated entry lowers a COMBINE under the opt-in");
    assert_eq!(lowered.preamble, relate_parser::PreambleMode::Execute);
    let relate_ir::Clause::Match {
        search: Some(_), ..
    } = &lowered.query.parts[0].clauses[0]
    else {
        panic!("expected a MATCH with a SEARCH clause");
    };
}

/// The CIP-231 showcase query, shared by the two preview-gate tests.
const LOCAL_CALLABLES_QUERY: &str = "DEFINE FUNCTION isEven(num) = \
    CASE WHEN num % 2 = 0 THEN true ELSE false END \
    UNWIND range(1,4) AS x RETURN x, isEven(x) AS isEven";

#[test]
fn local_callables_gate_fails_closed_without_the_opt_in() {
    // ADR-0371: a DEFINE query behind a preview set that does not
    // include `local-callables` fails with the authored error, before
    // anything of it rewrites or lowers.
    let error = match parse_and_lower_statement_with_preview(
        LOCAL_CALLABLES_QUERY,
        relate_core::PreviewFeatures::none(),
    ) {
        Ok(_) => panic!("a gated query unexpectedly lowered without the opt-in"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains(
            "preview feature 'local-callables' is not enabled: \
             set RELATE_PREVIEW=local-callables"
        ),
        "{error}"
    );
}

#[test]
fn local_callables_gate_admits_the_opt_in() {
    let enabled = relate_core::PreviewFeatures::parse("local-callables").unwrap();
    let lowered = parse_and_lower_statement_with_preview(LOCAL_CALLABLES_QUERY, enabled)
        .expect("the gated entry lowers a local callable under the opt-in");
    assert_eq!(lowered.preamble, relate_parser::PreambleMode::Execute);
    // The rewrite eliminated the definition: the lowered query is
    // plain pre-existing Cypher (a COLLECT subquery under `head`).
    assert!(lowered.query.parts[0]
        .clauses
        .iter()
        .any(|clause| matches!(clause, relate_ir::Clause::Unwind { .. })));
}

/// Invariant (LABS-1120): `lex_parse` has ONE meaning — the parse
/// call's own window — so the two stamping entries (the plain entry
/// and the engine's gated entry) must measure the same work under the
/// name. Pinned by comparing both entries' `lex_parse` on the same
/// statement: the gated entry additionally runs the preview-detector
/// walk after its parse, so if either entry ever again closes the
/// window late (swallowing that walk, which the local-callables
/// statement makes real work), the ratio diverges past the bound.
#[test]
fn both_entries_lex_parse_measure_the_same_span() {
    for src in [
        "MATCH (n:Person) WHERE n.name = 'a' RETURN n.name",
        LOCAL_CALLABLES_QUERY,
    ] {
        let enabled = relate_core::PreviewFeatures::parse("local-callables").unwrap();
        // Min of three per entry: the first call through a cold path
        // pays warmup the second entry does not, and min-of-same-work
        // is the stable estimator of the span itself.
        let min_lex_parse = |lower: &dyn Fn(&str) -> Result<LoweredStatement>| {
            (0..3)
                .map(|_| {
                    lower(src)
                        .expect("the sample statement lowers")
                        .stages
                        .lex_parse
                        .as_nanos()
                })
                .min()
                .expect("three samples")
        };
        let (plain_ns, gated_ns) = (
            min_lex_parse(&parse_and_lower_statement),
            min_lex_parse(&|s| parse_and_lower_statement_with_preview(s, enabled)),
        );
        let ratio = plain_ns.max(gated_ns) as f64 / plain_ns.min(gated_ns).max(1) as f64;
        assert!(
            ratio <= 3.0,
            "{src}: the entries' lex_parse windows diverged ({plain_ns} ns vs                  {gated_ns} ns, ratio {ratio}) — they no longer measure the same span"
        );
    }
}

/// Invariant (LABS-1120): the PARSE stage's fine sub-split partitions
/// the entry's own measured window — `lex_parse + rewrite + lower`
/// never exceeds the whole entry call's duration, and each sub-window
/// is non-negative. Pinned over a plain statement and a local-callables
/// statement so the rewrite window is exercised with real work in it.
#[test]
fn parse_stages_partition_the_entry_window() {
    let samples = [
        "MATCH (n:Person) WHERE n.name = 'a' RETURN n.name",
        LOCAL_CALLABLES_QUERY,
    ];
    for src in samples {
        let enabled = relate_core::PreviewFeatures::parse("local-callables").unwrap();
        let started = Instant::now();
        let lowered = parse_and_lower_statement_with_preview(src, enabled)
            .expect("the sample statement lowers");
        let whole = started.elapsed();
        let sum = lowered.stages.lex_parse + lowered.stages.rewrite + lowered.stages.lower;
        assert!(
            sum <= whole,
            "{src}: stages {sum:?} exceed the entry window {whole:?}"
        );
    }
}

/// The `FOR` clause showcase query: the clause is default
/// dialect, so it lowers with no preview opt-in.
const GQL_FOR_QUERY: &str = "FOR x IN [1, 2, 3] RETURN x";

#[test]
fn gql_for_lowers_onto_unwind_without_a_gate() {
    let lowered =
        parse_and_lower_statement_with_preview(GQL_FOR_QUERY, relate_core::PreviewFeatures::none())
            .expect("the graduated FOR clause lowers without any opt-in");
    assert_eq!(lowered.preamble, relate_parser::PreambleMode::Execute);
    // CIP-250: the FOR clause lowers onto the existing UNWIND
    // operator -- no new IR clause shape.
    assert!(matches!(
        &lowered.query.parts[0].clauses[0],
        relate_ir::Clause::Unwind { .. }
    ));
}

/// The four Cypher 25 chronological operators, in one showcase query.
const CHRONOLOGICAL_QUERY: &str = "RETURN date('2020-01-01') IS BEFORE date('2020-01-02') \
    AS result";

#[test]
fn chronological_comparisons_gate_fails_closed_without_the_opt_in() {
    // ADR-0371: a chronological operator behind a preview set that does
    // not include `chronological-comparisons` fails with the authored
    // error before anything lowers.
    let error = match parse_and_lower_statement_with_preview(
        CHRONOLOGICAL_QUERY,
        relate_core::PreviewFeatures::none(),
    ) {
        Ok(_) => panic!("a gated query unexpectedly lowered without the opt-in"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains(
            "preview feature 'chronological-comparisons' is not enabled: \
             set RELATE_PREVIEW=chronological-comparisons"
        ),
        "{error}"
    );
}

#[test]
fn chronological_comparisons_gate_admits_the_opt_in() {
    let enabled = relate_core::PreviewFeatures::parse("chronological-comparisons").unwrap();
    let lowered = parse_and_lower_statement_with_preview(CHRONOLOGICAL_QUERY, enabled)
        .expect("the gated entry lowers a chronological comparison under the opt-in");
    let Clause::Return { items, .. } = &lowered.query.parts[0].clauses[0] else {
        panic!("expected a RETURN clause");
    };
    assert!(
        matches!(items[0].expr, Expr::ChronoCompare { .. }),
        "expected the chronological comparison, got {:?}",
        items[0].expr
    );
}

const PATH_CONSTRUCT_QUERY: &str = "MATCH (a)-[r]->(b) RETURN PATH [a, r, b] AS p";

#[test]
fn path_construction_gate_fails_closed_without_the_opt_in() {
    let error = match parse_and_lower_statement_with_preview(
        PATH_CONSTRUCT_QUERY,
        relate_core::PreviewFeatures::none(),
    ) {
        Ok(_) => panic!("a gated query unexpectedly lowered without the opt-in"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains(
            "preview feature 'path-construction' is not enabled: \
             set RELATE_PREVIEW=path-construction"
        ),
        "{error}"
    );
}

#[test]
fn path_construction_gate_admits_the_opt_in() {
    let enabled = relate_core::PreviewFeatures::parse("path-construction").unwrap();
    let lowered = parse_and_lower_statement_with_preview(PATH_CONSTRUCT_QUERY, enabled)
        .expect("the gated entry lowers a path constructor under the opt-in");
    let Clause::Return { items, .. } = &lowered.query.parts[0].clauses[1] else {
        panic!("expected a RETURN clause");
    };
    assert!(
        matches!(&items[0].expr, Expr::PathConstruct { elements } if elements.len() == 3),
        "expected the path constructor, got {:?}",
        items[0].expr
    );
}

/// The complex-pattern showcase: two components and a two-hop pattern, the
/// shape `FulltextSearchWithComplexPatternAcceptance` admits under
/// Neo4j's complex-pattern mode.
const COMPLEX_SEARCH_QUERY: &str = "MATCH (seed:Person {name: 'Alice'}), (p:Person) \
    SEARCH p IN (FULLTEXT INDEX personBios FOR 'brave' LIMIT 5) \
    RETURN p.name AS person";

/// ADR-0678: the complex-pattern mode graduated — every pattern shape the
/// general MATCH machinery accepts lowers under the default dialect, and
/// the `complex-search-patterns` opt-in is gone with the gate it widened.
#[test]
fn complex_search_patterns_lower_under_the_default_dialect() {
    let lowered = parse_and_lower(COMPLEX_SEARCH_QUERY)
        .expect("a complex SEARCH pattern lowers without any opt-in");
    let Clause::Match {
        search: Some(_), ..
    } = &lowered.parts[0].clauses[0]
    else {
        panic!("expected a MATCH with a SEARCH clause");
    };
    let graduated = relate_core::PreviewFeatures::parse("complex-search-patterns")
        .expect_err("the graduated preview name is gone from the registry");
    assert!(
        graduated.to_string().contains("unknown preview feature"),
        "{graduated}"
    );
}

#[test]
fn return_one() {
    let q = parse_and_lower("RETURN 1 AS n").unwrap();
    assert!(matches!(q.parts[0].clauses[0], Clause::Return { .. }));
    let Clause::Return { items, .. } = &q.parts[0].clauses[0] else {
        panic!("expected return");
    };
    assert!(matches!(items[0].expr, Expr::Int(1)));
    assert!(items[0].alias.as_ref().is_some_and(|a| a == "n"));
}

#[test]
fn create_match_roundtrip_shape() {
    let q = parse_and_lower("CREATE (a:Person {name: 'Ada'}) RETURN a.name").unwrap();
    assert!(matches!(q.parts[0].clauses[0], Clause::Create { .. }));
    assert!(matches!(q.parts[0].clauses[1], Clause::Return { .. }));
}

#[test]
fn rejects_parameter_maps_in_patterns() {
    for cypher in [
        "MATCH (n $param) RETURN n",
        "MATCH ()-[r:FOO $param]->() RETURN r",
        "MERGE (n $param)",
    ] {
        let error = parse_and_lower(cypher).unwrap_err().to_string();
        assert!(
            error.contains("InvalidParameterUse"),
            "wrong error for {cypher}: {error}"
        );
    }
}

#[test]
fn yield_star_is_marked_on_call() {
    let q = parse_and_lower("CALL test.my.proc() YIELD *").unwrap();
    let Clause::Call { yield_items, .. } = &q.parts[0].clauses[0] else {
        panic!("expected call");
    };
    assert!(matches!(yield_items, relate_ir::YieldItems::Star));
}

#[test]
fn malformed_var_length_is_invalid_relationship_pattern() {
    let error = parse_and_lower("MATCH (a)-[:LIKES..]->(c) RETURN a")
        .unwrap_err()
        .to_string();
    assert!(error.contains("InvalidRelationshipPattern"), "{error}");
}

#[test]
fn with_order_by_complex_agg_is_ambiguous() {
    // The non-aggregate side is NOT a projection item, so mixing it with
    // the aggregation stays the ambiguous shape.
    let error = parse_and_lower(
        "MATCH (me: Person)--(you: Person)
         WITH me.age + you.age AS agg, count(*) AS cnt
         ORDER BY me.age + you.age * 2 + count(*)
         RETURN *",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("AmbiguousAggregationExpression"), "{error}");
}

#[test]
fn with_order_by_may_decompose_a_projected_expression() {
    // The non-aggregate side is verbatim a projection item, which is the
    // grouping key (ReturnOrderBy6 [5]'s WITH spelling).
    parse_and_lower(
        "MATCH (me: Person)--(you: Person)
         WITH me.age + you.age AS agg, count(*) AS cnt
         ORDER BY me.age + you.age + count(*)
         RETURN *",
    )
    .unwrap();
}
