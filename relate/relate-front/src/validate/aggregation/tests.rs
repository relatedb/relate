//! The aggregation battery's tests, offloaded by the file-size gate
//! (the ast/tree.rs shape): grouping keys, ORDER BY aggregation, the
//! projected-item predicate, and the paren batteries.

use super::*;
use crate::ast::naming::tests::{paren_item, paren_prop, paren_var, paren_wrap};
// LABS-260: an ORDER BY aggregate over the projection's input scope is
// valid when the DISTINCT projection itself aggregates.
#[test]
fn distinct_return_order_by_aggregate_uses_input_scope() {
    // Direct re-aggregation of a projected aggregate.
    let ok = validate_semantics(
        "MATCH (p:Person) RETURN DISTINCT p.community, count(p) ORDER BY count(p) DESC",
    );
    assert!(ok.is_ok(), "projected aggregate in ORDER BY: {ok:?}");
    // Ordering by the aggregate's alias keeps working.
    let ok = validate_semantics(
        "MATCH (p:Person) RETURN DISTINCT p.community, count(p) AS c ORDER BY c DESC",
    );
    assert!(ok.is_ok(), "alias ordering: {ok:?}");
    // A variable that only exists pre-projection but is referenced
    // outside any aggregation is still undefined (ReturnOrderBy2 [13]
    // shape: DISTINCT does not preserve it).
    let err = validate_semantics(
        "MATCH (p:Person) RETURN DISTINCT p.community, count(p) ORDER BY p.name",
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UndefinedVariable);
    // A variable inside the ORDER BY aggregate that was never in scope
    // is still undefined.
    let err = validate_semantics(
        "MATCH (p:Person) RETURN DISTINCT p.community, count(p) ORDER BY count(z)",
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UndefinedVariable);
}
// LABS-276 / ADR-0225: every single-input aggregate requires exactly
// one argument (Neo4j 42I13), wherever it appears in the projection
// clause — items, GROUP BY keys, ORDER BY expressions, and both the
// RETURN and WITH forms.
#[test]
fn zero_argument_single_input_aggregates_error() {
    // The canonical SINGLE_INPUT_AGGREGATES table drives the loop, so
    // a name added to the table without validation reach fails here
    // first.
    for agg in SINGLE_INPUT_AGGREGATES {
        let err = validate_semantics(&format!("CREATE (n) RETURN {agg}()")).unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::InvalidNumberOfArguments,
            "{agg}() item: {err:?}"
        );
        let err =
            validate_semantics(&format!("UNWIND [1] AS x WITH {agg}() AS s RETURN s")).unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::InvalidNumberOfArguments,
            "WITH {agg}() item: {err:?}"
        );
    }
}
#[test]
fn zero_argument_aggregate_nested_in_expression_errors() {
    let err = validate_semantics("MATCH (n) RETURN 1 + sum()").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidNumberOfArguments);
    let err = validate_semantics("MATCH (n) RETURN count(*) ORDER BY min()").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidNumberOfArguments);
    let err = validate_semantics("MATCH (n) RETURN n.id GROUP BY max()").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidNumberOfArguments);
}
// ADR-0217: the front's arity rejection and the executor's `agg_eval`
// backstop rejection are one contract — the `AggregateArity` table in
// `relate-ir`. The core-`Error` channel spells the kind inline in
// the contract's `rejection`; the front's `EngineError` carries the
// kind typed and `Display` prefixes it. This pins the byte-equivalence
// of the two renderings, so neither spelling can drift alone.
#[test]
fn front_arity_rejection_matches_the_ir_contract_text() {
    for (name, got) in [("sum", 0), ("stDev", 2), ("collect_list", 3)] {
        let front = arity_rejection(name, got).to_string();
        let contract = AggregateArity::SingleInput.rejection(name, got);
        assert_eq!(
            front, contract,
            "{name} rejection drifted from the contract"
        );
    }
}
#[test]
fn grouping_element_referencing_an_alias_must_be_bare() {
    for query in [
        "WITH 1 AS a RETURN a, count(*) GROUP BY a + 1",
        "WITH {p: 1} AS c, 2 AS b RETURN c AS a, sum(b) AS s GROUP BY a.p",
    ] {
        let err = validate_semantics(query).unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::InvalidAggregation,
            "{query}: {err:?}"
        );
    }
    // A bare alias reference resolves to the aliased item.
    assert!(validate_semantics("WITH 1 AS a RETURN a, count(*) GROUP BY a").is_ok());
}
#[test]
fn one_argument_aggregates_and_the_matching_shapes_still_validate() {
    // One-argument calls, count(*), count(x), and the zero-input-row
    // semantics (sum=0, avg/min/max/collect of nothing) are unchanged.
    assert!(validate_semantics(
        "UNWIND [] AS x RETURN sum(x), avg(x), min(x), max(x), collect(x), count(x), count(*)"
    )
    .is_ok());
    assert!(validate_semantics("MATCH (n:Person) RETURN n.age GROUP BY n.age").is_ok());
    assert!(validate_semantics("MATCH (n:Person) RETURN count(n) ORDER BY count(n) DESC").is_ok());
}
// LABS-744: aggregates under the aggregation-transparent positions are
// not this module's to reject. The finding's shapes (`RETURN
// -count(*)`, `-sum(n.v)`) already pass front validation: a Unary
// operand sits inside the aggregation-transparent reach
// ([`agg_enter_child`] here, `historical_enter_child` in `crate::agg`),
// so the item is classified aggregating, and no plan-time rejection of
// these shapes exists anywhere in the front. The error users see is the
// executor's scalar-eval backstop ("count(*) only valid in
// aggregation", eval/mod.rs; "<fn>() is an aggregating function",
// eval/function.rs), reached through `agg_eval`'s `_` wildcard arm —
// the gap LABS-744 closes with a shared aggregate-transparent child
// map in `relate-ir`'s walk.rs, consumed by `agg_eval`. Pin the
// front's acceptance so that cross-crate fix cannot regress into a
// plan-time rejection added here.
#[test]
fn aggregates_under_transparent_positions_are_not_rejected_here() {
    for query in [
        "MATCH (n) RETURN -count(*)",
        "MATCH (n) RETURN -sum(n.v)",
        "MATCH (n) RETURN n.age, -count(*) GROUP BY n.age",
    ] {
        let ok = validate_semantics(query);
        assert!(ok.is_ok(), "{query} must pass front validation: {ok:?}");
    }
}
// The percentile aggregates stay out of the arity table (ADR-0225):
// their two-argument check lives at their call site, not here.
#[test]
fn percentile_aggregates_keep_their_own_call_site_arity_check() {
    assert!(validate_semantics("MATCH (n) RETURN percentileCont(n.age, 0.5)").is_ok());
    // Zero or one argument is not this module's error — the call-site
    // check in the executor rejects it.
    assert!(validate_semantics("MATCH (n) RETURN percentileCont()").is_ok());
    assert!(validate_semantics("MATCH (n) RETURN percentileDisc(n.age)").is_ok());
}
// The front-end ambiguity gate is variable-coverage only; the leaf-shape
// discipline over the covered variables is the planner's
// (`check_item_grouping`), which sees the clause's grouping keys. Live Neo4j
// 2026.10.1 (probed 2026-11-21) accepts the mixed arithmetic over a key
// variable on both sides of the key list, and rejects a mixed expression
// whose variable no key covers.
#[test]
fn the_front_end_ambiguity_gate_covers_variables_only() {
    for query in [
        // The key list names `n.age`, the mixed item reads `n.age * 2`.
        "MATCH (n) RETURN n.age * 2 + count(*), n.age * 2 GROUP BY n.age",
        // A key expression the projection never restates.
        "MATCH (n) RETURN n.age * 2 + count(*) GROUP BY n.age * 2",
    ] {
        assert!(validate_semantics(query).is_ok(), "{query}");
    }
    // No key covers `n`: the gate rejects before the planner sees it.
    let err = validate_semantics("MATCH (n) RETURN n.x + count(*)").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::AmbiguousAggregationExpression);
}
// The projected-item predicate owns the RETURN `*`/`ALL` decision: a
// star item projects the incoming scope, not an expression, so it never
// answers the item question for any caller.
#[test]
fn projected_item_excludes_star_items() {
    let star = ReturnItem {
        expr: Expr::Star,
        alias: None,
        all: true,
        source_text: None,
    };
    let named = ReturnItem {
        expr: Expr::Variable("n".into()),
        alias: None,
        all: false,
        source_text: None,
    };
    assert!(!is_projected_item(std::slice::from_ref(&star), &Expr::Star));
    assert!(is_projected_item(
        &[star, named.clone()],
        &Expr::Variable("n".into())
    ));
    assert!(!is_projected_item(&[named], &Expr::Variable("m".into())));
}
// LABS-1615: the projected-item predicate is paren-transparent on both
// sides — `(n.p1)` restates as `n.p1` and vice versa, and no paren
// spelling invents or loses a column.
#[test]
fn projected_item_is_paren_transparent() {
    // The fixtures are the one shared spelling (collect's tests module,
    // the ast/tree.rs tests-visibility precedent), not a third copy of
    // the constructors.
    let item = |expr: Expr| paren_item(expr, None, "");
    assert!(is_projected_item(
        &[item(paren_wrap(paren_var("n")))],
        &paren_var("n")
    ));
    assert!(is_projected_item(
        &[item(paren_var("n"))],
        &paren_wrap(paren_var("n"))
    ));
    assert!(is_projected_item(
        &[item(paren_wrap(paren_prop("n", "p")))],
        &paren_prop("n", "p")
    ));
    assert!(is_projected_item(
        &[item(paren_prop("n", "p"))],
        &paren_wrap(paren_prop("n", "p"))
    ));
    assert!(!is_projected_item(
        &[item(paren_wrap(paren_var("n")))],
        &paren_wrap(paren_prop("n", "p"))
    ));
    assert!(!is_projected_item(
        &[item(paren_wrap(paren_var("n")))],
        &paren_var("m")
    ));
}
// LABS-1615: a GROUP BY key restates through redundant parens on
// either side — `(a.p)` and `a.p` are the same key — while the
// complex-key restriction is unchanged: a non-simple key stays
// whole-expression-only, parens or not.
#[test]
fn group_by_keys_restate_through_parens() {
    for query in [
        "MATCH (a) RETURN count(a) AS c GROUP BY (a.p) ORDER BY a.p",
        "MATCH (a) RETURN count(a) AS c GROUP BY a.p ORDER BY (a.p)",
        "MATCH (a) RETURN count(a) AS c GROUP BY (a) ORDER BY 1 + a",
    ] {
        let ok = validate_semantics(query);
        assert!(ok.is_ok(), "{query}: {ok:?}");
    }
    let err = validate_semantics(
        "MATCH (a) RETURN count(a) AS c GROUP BY a.p + 0 ORDER BY 1 + (a.p + 0)",
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UndefinedVariable);
}
// LABS-1615: the GBN1.5 self-shadowing exemption resolves through
// the projection item's parens — `RETURN (a.p) AS a GROUP BY a.p` is
// the same shadow as `RETURN a.p AS a GROUP BY a.p`, because the
// name resolves to the paren-transparent expression
// ([`projection_item_expr`] returns the normalized form it matched
// on) — and through the KEY's parens too: `GROUP BY (a.p)` is the
// key `a.p` (check_grouping_element_aliases normalizes both sides),
// and a parenthesized bare-alias key `GROUP BY (a)` resolves through
// the alias exactly like `GROUP BY a` instead of losing the bare-
// variable skip.
#[test]
fn gbn15_shadow_resolves_through_parens() {
    for query in [
        "MATCH (a) RETURN (a.p) AS a, count(*) AS c GROUP BY a.p",
        "MATCH (a) RETURN a.p AS a, count(*) AS c GROUP BY a.p",
        "MATCH (a) RETURN (a.p) AS a, count(*) AS c GROUP BY (a.p)",
        "MATCH (a) RETURN (a).p AS a, count(*) AS c GROUP BY a.p",
        "MATCH (a) RETURN (a).p AS a, count(*) AS c GROUP BY (a).p",
        "MATCH (a) RETURN a.p AS a, count(*) AS c GROUP BY (a.p)",
    ] {
        let ok = validate_semantics(query);
        assert!(ok.is_ok(), "{query}: {ok:?}");
    }
    // A parenthesized bare-alias key keeps the bare-variable skip.
    for query in [
        "MATCH (n) RETURN n AS a, count(*) AS c GROUP BY (a)",
        "MATCH (n) RETURN n AS a, count(*) AS c GROUP BY a",
    ] {
        let ok = validate_semantics(query);
        assert!(ok.is_ok(), "{query}: {ok:?}");
    }
}

// REL8-1869: a scalar function call over a grouping-key variable is a
// valid non-aggregation part of a mixed expression. The issue's shape —
// an aggregating WITH's alias, read through `toFloat` in a later
// aggregating RETURN — and the plain-variable twin both pin that the
// boundary is the call, not the key's provenance: `RETURN p, count(m)`
// beside the same `p` already validated, so `toFloat(p)` must too.
#[test]
fn scalar_call_over_a_grouping_key_variable_is_a_valid_mixed_part() {
    for query in [
        // The issue's shape: `c` comes from an earlier aggregating WITH
        // and is the later RETURN's grouping key.
        "MATCH (n) WHERE n.p1 IS NOT NULL WITH count(n) AS c MATCH (m) \
         RETURN toFloat(c) / count(m) AS coverage, c",
        // The plain-variable twin: no aggregation provenance at all.
        "MATCH (n) WITH n.p1 AS p MATCH (m) RETURN toFloat(p) / count(m) AS coverage, p",
        // A scalar call over a key property, nested scalar calls, and a
        // literal argument riding the literal arm.
        "MATCH (n) RETURN abs(n.p1) + count(*) AS x, n.p1",
        "MATCH (n) WITH n.p1 AS p RETURN toUpper(toString(p)) + count(*) AS x, p",
        "MATCH (n) RETURN coalesce(n.p1, 0) + count(*) AS x, n.p1",
        // A non-deterministic call over the key rides the same arm (live
        // Neo4j 2026.10.1 admits the shape; the group key stays the
        // projected items).
        "MATCH (n) WITH count(n) AS c MATCH (m) \
         RETURN coalesce(rand(), c) / count(m) AS coverage, c",
    ] {
        let ok = validate_semantics(query);
        assert!(ok.is_ok(), "{query}: {ok:?}");
    }
}

// LABS-1615's paren transparency reaches the mixed-expression parts: a
// parenthesized part validates like its bare spelling — the paren arm
// the sibling walk (`check_non_agg_parts_match_keys`) has always had —
// and a property's parenthesized base resolves through the base
// variable like every other spelling (live Neo4j 2026.10.1 admits all
// of these).
#[test]
fn parenthesized_parts_validate_like_their_bare_spellings() {
    for query in [
        // A bare paren property beside the projected key.
        "MATCH (n) RETURN (n.p1) + count(*) AS x, n.p1",
        // A parenthesized base: `(n).p1` reads the key variable's property.
        "MATCH (n) RETURN (n).p1 + count(*) AS x, n.p1",
        // A paren-wrapped call argument.
        "MATCH (n) WITH count(n) AS c MATCH (m) RETURN toFloat((c)) / count(m) AS coverage, c",
        // A paren-wrapped verbatim key: the guard matches it directly.
        "MATCH (n) RETURN count(*) + (n.p1) AS y, (n.p1)",
        // A bare side beside a paren-wrapped key: the guard matches the
        // paren-transparent forms directly.
        "MATCH (n) RETURN count(*) + n.p1 AS y, (n.p1)",
    ] {
        let ok = validate_semantics(query);
        assert!(ok.is_ok(), "{query}: {ok:?}");
    }
}

// The gate's coverage boundary is the *variable* set: a call over a
// non-key variable stays ambiguous, while a call over a covered one is
// admitted here and left to the planner's leaf discipline. The list below
// pins this front-end gate only — the end-to-end verdict of a shape can be
// stricter, and for the star-carried property leaf it is:
// `relate/relate/tests/aggregate_grouping_key_through_scalar_call.rs::
// scalar_call_over_a_non_grouped_variable_stays_ambiguous` pins the same
// statement as an end-to-end `AmbiguousAggregationExpression` rejection
// (the planner's variables-only `*`, ADR-0198 ruling 1), while live Neo4j
// 2026.10.1 runs it and returns a row per group (probed 2026-11-21 against
// a scratch database: `MATCH (m) RETURN *, toFloat(m.p1) + count(*) AS x`
// over `(:N {p1: 1}), (:N {p1: 2}), (:M)` gives x = 2.0, 3.0, null).
#[test]
fn scalar_call_mixed_parts_keep_the_grouping_coverage_boundary() {
    // `q` is in scope but not a key of the RETURN.
    let err = validate_semantics(
        "MATCH (n) WITH n.p1 AS p, n.p2 AS q MATCH (m) RETURN toFloat(q) / count(m) AS x, p",
    )
    .unwrap_err();
    assert_eq!(
        err.kind(),
        ErrorKind::AmbiguousAggregationExpression,
        "{err:?}"
    );
    for query in [
        // A property leaf of a star-carried variable, through the call: the
        // gate admits it because the star covers the variable; the planner
        // still refuses it (the sibling integration test above pins that,
        // and live disagrees with the planner — REL8-1939).
        "MATCH (m) RETURN *, toFloat(m.p1) + count(*) AS x",
        // An aggregate hidden in a list literal (invisible to
        // `contains_aggregation`) is that aggregation's own input.
        "MATCH (n) RETURN n, n.p1, count(*) + coalesce(n, [collect(n.p1)]) AS y",
    ] {
        assert!(validate_semantics(query).is_ok(), "{query}");
    }
}
