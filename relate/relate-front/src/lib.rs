//! Relate parser front-end lowered to the Relate IR (ADR-0046).

pub mod agg;
pub(crate) mod ast;
mod call_scope;
mod ident_hash;
pub(crate) mod local_callables;
mod lower;
mod output_vars;
pub mod show_columns;
pub mod validate;

use relate_core::{Error, Result};
use relate_ir::Query;

pub use lower::{lower_expr, lower_statement, lower_statement_with_limits, lower_type_expr};
pub use validate::{is_known_function, KNOWN_FUNCTIONS};

// `StatementPolicy` is reached through its home module, `validate`, so the
// type has one public path rather than a second re-export alias (LABS-311).
use validate::StatementPolicy;

/// A parsed and lowered top-level statement: the planner input plus the
/// statement's `EXPLAIN`/`PROFILE` preamble flags.
///
/// Only the top-level statement carries the flags (the preamble grammar is
/// top-level only); nested bodies (`CALL { }`, `NEXT` stages, expression
/// subqueries) always report `false`. The flags are deliberately *not* part
/// of [`Query`]: honoring them happens above planning, at the connection
/// layer, so no execution path can mistake an `EXPLAIN` for its wrapped
/// statement.
pub struct LoweredStatement {
    pub query: Query,
    pub explain: bool,
    pub profile: bool,
}

/// Maps a `relate-parser` failure to a front-end error.
///
/// ADR-0116 §2: a parse failure tagged
/// [`UNSUPPORTED_ADMINISTRATION_DETAIL`](relate_parser::parser::UNSUPPORTED_ADMINISTRATION_DETAIL)
/// is a recognized database administration command outside CE's supported
/// set; its message is the verbatim statement text (without a leading
/// `CYPHER n`), echoed in CE's `UnsupportedAdministrationCommand` (51N27)
/// *semantic* error. Every other parse failure stays a syntax error,
/// prepending the TCK error-detail tag when one is present.
fn map_parse_error(e: relate_parser::ParseError) -> Error {
    if e.detail == Some(relate_parser::parser::UNSUPPORTED_ADMINISTRATION_DETAIL) {
        return relate_core::ce_messages::unsupported_administration_command(&e.message);
    }
    if let Some(detail) = e.detail {
        Error::syntax(format!("{detail}: {e}"))
    } else {
        Error::syntax(e.to_string())
    }
}

/// Parse with `relate-parser`, resolve CIP-231 local callables away
/// (the rewrite pass that eliminates `DEFINE` blocks and their call
/// sites, ADR-0382), and lower into a planner-IR `Query`.
pub fn parse_and_lower(src: &str) -> Result<Query> {
    let mut stmt = relate_parser::parse(src).map_err(map_parse_error)?;
    // The ADR-0257 depth guard runs before the CIP-231 resolver walks the
    // tree: a too-deep body gets the typed depth error here, never a
    // stack overflow inside the resolver's own recursion.
    crate::validate::validate_nesting_depth(&stmt)
        .map_err(|error| Error::syntax(error.to_string()))?;
    local_callables::resolve(&mut stmt)?;
    lower_statement(&stmt)
}

/// The resolve-then-lower tail every top-level entry shares: the CIP-231
/// rewrite pass (ADR-0382) eliminates `DEFINE` blocks and their call
/// sites, then lowering runs on the plain pre-existing Cypher that
/// remains, keeping the statement's own `EXPLAIN`/`PROFILE` flags.
/// `params` are the statement's bound parameters, which the dynamic-operand
/// admission rule treats as compile-time constants (LABS-709).
fn resolve_and_lower(
    mut stmt: relate_parser::Statement,
    options: validate::ValidateOptions,
    policy: StatementPolicy,
    params: &std::collections::BTreeMap<String, relate_core::Value>,
) -> Result<LoweredStatement> {
    crate::validate::validate_nesting_depth(&stmt)
        .map_err(|error| Error::syntax(error.to_string()))?;
    local_callables::resolve(&mut stmt)?;
    Ok(LoweredStatement {
        explain: stmt.explain,
        profile: stmt.profile,
        query: lower::lower_statement_with_options_and_policy(&stmt, options, policy, params)?,
    })
}

/// Parse and lower a top-level statement, keeping its `EXPLAIN`/`PROFILE`
/// preamble flags alongside the lowered query.
pub fn parse_and_lower_statement(src: &str) -> Result<LoweredStatement> {
    // ADR-0257: enforce the query-size limit before parsing. The parser
    // enforces the same cap; this check keeps the error on the front-end
    // path even if a caller reaches lowering another way.
    resolve_and_lower(
        parse_sized(src)?,
        validate::ValidateOptions::default(),
        StatementPolicy::default(),
        &std::collections::BTreeMap::new(),
    )
}

/// Parse and lower a top-level statement behind the ADR-0371 preview gate:
/// a statement whose AST uses a gated preview construct fails closed with
/// the authored disabled-feature error unless `preview` enables it, before
/// anything of it lowers or plans. This is the preview-gated, default-policy
/// convenience; the engine's statement admission goes through
/// [`parse_and_lower_statement_with_policy`] with the store's resolved
/// preview set and policy. The ungated [`parse_and_lower_statement`] stays
/// the dialect-floor entry for callers without a store.
pub fn parse_and_lower_statement_with_preview(
    src: &str,
    preview: relate_core::PreviewFeatures,
) -> Result<LoweredStatement> {
    parse_and_lower_statement_with_policy(src, preview, StatementPolicy::default())
}

/// [`parse_and_lower_statement_with_preview`] with the store's resolved
/// traversal-depth admission limit (LABS-610). The safe-off convenience
/// retained for callers that have a limit but no full [`StatementPolicy`];
/// the engine's admission goes through
/// [`parse_and_lower_statement_with_policy`]. The other entries delegate
/// with [`relate_core::MAX_TRAVERSAL_DEPTH`].
pub fn parse_and_lower_statement_with_limits(
    src: &str,
    preview: relate_core::PreviewFeatures,
    max_traversal_depth: u32,
) -> Result<LoweredStatement> {
    parse_and_lower_statement_with_policy(
        src,
        preview,
        StatementPolicy::unsafened(max_traversal_depth),
    )
}

/// [`parse_and_lower_statement_with_limits`] plus the opt-in safe-mode shape
/// rules (LABS-680, ADR-0477): the engine's admission passes the store's
/// resolved `OpenOptions::safe_mode` and traversal limit in one
/// [`StatementPolicy`]. Every other entry delegates with safe mode off.
pub fn parse_and_lower_statement_with_policy(
    src: &str,
    preview: relate_core::PreviewFeatures,
    policy: StatementPolicy,
) -> Result<LoweredStatement> {
    parse_and_lower_statement_with_policy_and_params(
        src,
        preview,
        policy,
        &std::collections::BTreeMap::new(),
    )
}

/// [`parse_and_lower_statement_with_policy`] with the statement's bound
/// parameter values: the engine's admission path, which has them and must
/// pass them so a parameter-typed dynamic operand of the wrong type fails
/// before execution (LABS-709) instead of hiding behind a zero-row `MATCH`.
pub fn parse_and_lower_statement_with_policy_and_params(
    src: &str,
    preview: relate_core::PreviewFeatures,
    policy: StatementPolicy,
    params: &std::collections::BTreeMap<String, relate_core::Value>,
) -> Result<LoweredStatement> {
    // Plan-cache placeholders are the literals they replaced. Compile-time
    // literal checks (boolean operands, SKIP/LIMIT, variable types) read
    // them for this parse only.
    let _auto = relate_core::AutoLiteralGuard::install(params);
    // The `mixed-label-syntax` gate widens what the parser accepts (the
    // floor's CIP-40/CIP-14 mixing rejections lift), so it threads into
    // the parse itself as `ParseOptions` -- unlike the AST-detector gates
    // below, there is no floor AST to detect the construct in.
    let stmt = parse_sized_with_options(src, parse_options(preview))?;
    // The one gate loop, derived from the registry itself: every row's
    // detector is consulted here, so a detector-less registry row is a
    // compile error. The graph-type trio's AST walk runs once, before the
    // loop, and its arms report membership instead of a second loop.
    let graph_type_gates = validate::statement_graph_type_gates(&stmt)
        .map_err(|error| Error::syntax(error.to_string()))?;
    for feature in relate_core::PREVIEW_FEATURES {
        let feature = *feature;
        let uses = match feature {
            relate_core::PreviewFeature::HybridSearch => {
                validate::statement_uses_hybrid_search(&stmt)
            }
            relate_core::PreviewFeature::LocalCallables => {
                local_callables::statement_uses_local_callables(&stmt)
            }
            relate_core::PreviewFeature::Float16CoordinateTypes => {
                validate::statement_uses_float16_coordinate_types(&stmt)
            }
            relate_core::PreviewFeature::ChronologicalComparisons => {
                validate::statement_uses_chronological_comparisons(&stmt)
            }
            // `obfuscated-literals` gates at the lexer (the disabled form
            // is Neo4j's own syntax error, not the authored preview error),
            // so a parsed statement never carries a gate to detect here.
            relate_core::PreviewFeature::ObfuscatedLiterals => Ok(false),
            // `mixed-label-syntax` gates at parse (the `ParseOptions`
            // threading above) and `complex-search-patterns` at validation
            // (the `ValidateOptions` threading below), never post-parse, so
            // neither is an AST detector here. ADR-0378: `ontology` gates
            // at procedure dispatch (`procedure_gate.rs`), never at parse,
            // so it has no AST detector here either. The three ADR-0497
            // name-gate features (six gated names) gate a function *name*,
            // not a construct
            // (`validate::validate_function_admission` in the lowering
            // battery), and their disabled form is the ordinary
            // unknown-function rejection rather than the authored preview
            // error a detector would raise.
            relate_core::PreviewFeature::GraphTypeOptProps
            | relate_core::PreviewFeature::GraphTypeDefaultValues
            | relate_core::PreviewFeature::GraphTypeImpliedLabels => {
                Ok(graph_type_gates.contains(&feature))
            }
            relate_core::PreviewFeature::MixedLabelSyntax
            | relate_core::PreviewFeature::ComplexSearchPatterns
            | relate_core::PreviewFeature::Ontology
            | relate_core::PreviewFeature::CollectionZip
            | relate_core::PreviewFeature::CollectionHas
            | relate_core::PreviewFeature::GqlStddevAliases => Ok(false),
        };
        if uses.map_err(|error| Error::syntax(error.to_string()))? {
            preview.require(feature)?;
        }
    }
    // The constructs that only materialize against stored graph-type state
    // fail closed in the executor's graph-type layer with the same authored
    // error.
    resolve_and_lower(stmt, validate::ValidateOptions { preview }, policy, params)
}

/// The shared size-capped parse of [`parse_and_lower_statement`] and
/// [`parse_and_lower_statement_with_preview`]: ADR-0079's query-size limit
/// before parsing (the parser enforces the same cap; this check keeps the
/// error on the front-end path even if a caller reaches lowering another
/// way), then the parse itself. Every gate closed.
fn parse_sized(src: &str) -> Result<relate_parser::Statement> {
    parse_sized_with_options(src, relate_parser::lexer::ParseOptions::default())
}

/// The parse-affecting options derived from the store's preview set: the
/// registered features that change what the parser accepts rather than
/// something an AST walk can detect -- the token stream
/// (`obfuscated-literals`) and the parser's grammar (`mixed-label-syntax`).
fn parse_options(preview: relate_core::PreviewFeatures) -> relate_parser::lexer::ParseOptions {
    relate_parser::lexer::ParseOptions {
        obfuscated_literals: preview.enabled(relate_core::PreviewFeature::ObfuscatedLiterals),
        mixed_label_syntax: preview.enabled(relate_core::PreviewFeature::MixedLabelSyntax),
    }
}

/// Normalize a statement for the plan cache (ADR-0503): the engine
/// admission path's one entry into the parser's token-stream normalizer,
/// under the same preview-derived [`ParseOptions`] the real parse would
/// use (the gates change what the lexer produces, so the template must
/// come from the identical token stream). A pure mapping — cache
/// participation is decided where it must be, in the cache's own lookup
/// before any stage runs; a statement *referencing* an `__AUTO_*` name
/// still bails inside the normalizer, for the same reserved-name reason.
pub fn normalize_statement(
    src: &str,
    preview: relate_core::PreviewFeatures,
) -> relate_parser::NormalizeOutcome {
    relate_parser::normalize_statement(src, parse_options(preview))
}

/// [`parse_sized`] with explicit parse-affecting options.
fn parse_sized_with_options(
    src: &str,
    options: relate_parser::lexer::ParseOptions,
) -> Result<relate_parser::Statement> {
    if src.len() > relate_parser::parser::MAX_QUERY_BYTES {
        return Err(Error::syntax(format!(
            "query text of {} bytes exceeds the maximum query size of {} bytes",
            src.len(),
            relate_parser::parser::MAX_QUERY_BYTES
        )));
    }
    relate_parser::parse_with_options(src, options).map_err(map_parse_error)
}

/// Parse Cypher for `Database::query`.
///
/// Relate is the product front-end: `relate-parser` lowered into
/// planner IR (ADR-0046, ADR-0277).
///
/// Callers that honor the `EXPLAIN`/`PROFILE` prefixes (the connection
/// layer) use [`parse_and_lower_statement`]; this wrapper drops the flags.
pub fn parse_cypher(src: &str) -> Result<Query> {
    Ok(parse_and_lower_statement(src)?.query)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(!lowered.explain && !lowered.profile);
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
        assert!(!lowered.explain && !lowered.profile);
        // The rewrite eliminated the definition: the lowered query is
        // plain pre-existing Cypher (a COLLECT subquery under `head`).
        assert!(lowered.query.parts[0]
            .clauses
            .iter()
            .any(|clause| matches!(clause, relate_ir::Clause::Unwind { .. })));
    }

    /// The CIP-250 showcase query: the graduated `FOR` clause is default
    /// dialect, so it lowers with no preview opt-in.
    const GQL_FOR_QUERY: &str = "FOR x IN [1, 2, 3] RETURN x";

    #[test]
    fn gql_for_lowers_onto_unwind_without_a_gate() {
        let lowered = parse_and_lower_statement_with_preview(
            GQL_FOR_QUERY,
            relate_core::PreviewFeatures::none(),
        )
        .expect("the graduated FOR clause lowers without any opt-in");
        assert!(!lowered.explain && !lowered.profile);
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

    /// The CIP-225 Rule 0 showcase: two components and a two-hop pattern, the
    /// shape `FulltextSearchWithComplexPatternAcceptance` admits under
    /// Neo4j's complex-pattern mode.
    const COMPLEX_SEARCH_QUERY: &str = "MATCH (seed:Person {name: 'Alice'}), (p:Person) \
        SEARCH p IN (FULLTEXT INDEX personBios FOR 'brave' LIMIT 5) \
        RETURN p.name AS person";

    #[test]
    fn complex_search_patterns_gate_keeps_rule_0_without_the_opt_in() {
        // The gate widens validation instead of detecting a construct: with
        // the row off, CIP-225 Rule 0's own rejection is what a complex
        // SEARCH pattern gets.
        let error = match parse_and_lower_statement_with_preview(
            COMPLEX_SEARCH_QUERY,
            relate_core::PreviewFeatures::none(),
        ) {
            Ok(_) => panic!("a complex SEARCH pattern unexpectedly lowered without the opt-in"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("SearchClauseTooComplexPattern"), "{error}");
    }

    #[test]
    fn complex_search_patterns_gate_admits_the_opt_in() {
        let enabled = relate_core::PreviewFeatures::parse("complex-search-patterns").unwrap();
        let lowered = parse_and_lower_statement_with_preview(COMPLEX_SEARCH_QUERY, enabled)
            .expect("the gated entry lowers a complex SEARCH pattern under the opt-in");
        let Clause::Match {
            search: Some(_), ..
        } = &lowered.query.parts[0].clauses[0]
        else {
            panic!("expected a MATCH with a SEARCH clause");
        };
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
}
