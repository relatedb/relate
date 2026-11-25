//! REL8-1372: the `UNWIND` source itself streams, not just the rows over
//! it. `UNWIND range()` always streamed; here a list comprehension source
//! (`[i IN range(...) WHERE ... | ...]`) is pulled per element — bind,
//! filter, project on demand — so an aggregating consumer's memory follows
//! its groups, never the list length (ADR-0555). The list literal's own
//! materialization is charged to the expression budget: its length is
//! bounded by the query-text cap, but an uncharged build allocated
//! hundreds of MB under a small expression cap. The pattern-comprehension
//! source still materializes — the eval walk is not resumable — and the
//! last test pins that boundary until it streams.

use crate::common;

use relate::Value;

/// The expression budget the bounded shapes must fit inside: generous for
/// one sum group, impossible for a materialized list at the sizes below.
const EXPRESSION_CAP: u64 = 16 << 20;

/// The comma-joined spellings of `1..=n` — the one query-sized-literal
/// fixture the charge tests share.
fn query_sized_elements(n: usize) -> String {
    (1..=n).map(|i| i.to_string()).collect::<Vec<_>>().join(",")
}

/// The sum of `f(i)` over `1..=n` — the one expected-answer spelling the
/// comprehension tests share.
fn sum_of(n: i64, f: impl Fn(i64) -> i64) -> i64 {
    (1..=n).map(f).sum()
}

/// A comprehension source streams: the projected list is never built, so
/// 2M elements aggregate under the same 16 MiB expression cap that a
/// materialized 2M-slot list refuses — and the answer is exact at both
/// an order of magnitude apart.
#[test]
fn a_comprehension_source_aggregates_whatever_the_input_count() {
    let mut c = common::expression_capped(EXPRESSION_CAP);
    for n in [20_000, 2_000_000] {
        let q = format!("UNWIND [i IN range(1, {n}) | i * 2] AS x RETURN sum(x)");
        let values = common::first_column(&common::run(&mut c, &q));
        assert_eq!(values, vec![Value::Int(sum_of(n, |i| i * 2))], "n = {n}");
    }
}

/// The comprehension's `WHERE` gates the stream per element: a predicate
/// that rejects most elements still answers exactly, and every skipped
/// element is one pull of the source, never a materialized row.
#[test]
fn a_filtered_comprehension_source_aggregates_exactly() {
    let mut c = common::expression_capped(EXPRESSION_CAP);
    let q = "UNWIND [i IN range(1, 2_000_000) WHERE i % 7 = 3 | i * 2] AS x \
             RETURN sum(x)";
    let values = common::first_column(&common::run(&mut c, q));
    assert_eq!(
        values,
        vec![Value::Int(sum_of(2_000_000, |i| if i % 7 == 3 {
            i * 2
        } else {
            0
        }))]
    );
}

/// The streamed form answers what the comprehension's value form answers:
/// the same expression evaluated as a value (materialized, `eval_list_comp`)
/// and as a `UNWIND` source (streamed, element by element) produce the same
/// per-row list — over a row that the predicate and projection both read.
/// Every row's comprehension passes at least one element, so the `UNWIND`
/// form (which yields nothing for an empty list) keeps every input row too.
#[test]
fn a_streamed_comprehension_source_answers_the_value_form() {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND range(1, 4) AS k CREATE (:N {k: k * 10})");
    let value_form = common::run(
        &mut c,
        "MATCH (n:N) \
         RETURN n.k AS k, [i IN range(1, 3) WHERE i + n.k > 11 | i + n.k] AS xs \
         ORDER BY k",
    );
    let streamed = common::run(
        &mut c,
        "MATCH (n:N) WITH n \
         UNWIND [i IN range(1, 3) WHERE i + n.k > 11 | i + n.k] AS x \
         RETURN n.k AS k, collect(x) AS xs ORDER BY k",
    );
    // One row per :N node (k = 10..40); the streamed rows group per input
    // row, so each collect is that row's comprehension list.
    let expected: Vec<Vec<Value>> = (1..=4)
        .map(|n| {
            let k = n * 10;
            vec![
                Value::Int(k),
                Value::List(
                    (1..=3)
                        .filter(|i| i + k > 11)
                        .map(|i| Value::Int(i + k))
                        .collect::<Vec<_>>()
                        .into(),
                ),
            ]
        })
        .collect();
    assert_eq!(value_form.rows, expected);
    assert_eq!(streamed.rows, expected);
}

/// A comprehension whose source is itself a comprehension streams through
/// both levels: neither the inner nor the outer projected list is built.
#[test]
fn a_nested_comprehension_source_aggregates_whatever_the_input_count() {
    let mut c = common::expression_capped(EXPRESSION_CAP);
    let q = "UNWIND [x IN [i IN range(1, 1_000_000) | i * 3] | x + 1] AS y RETURN sum(y)";
    let values = common::first_column(&common::run(&mut c, q));
    assert_eq!(values, vec![Value::Int(sum_of(1_000_000, |i| i * 3 + 1))]);
}

/// A query-sized list literal is refused on the expression cap when its
/// materialization no longer fits: the charge is the same retaining
/// protocol a comprehension's output pays, so a 300k-slot literal under a
/// 16 MiB cap refuses instead of allocating silently.
#[test]
fn a_query_sized_list_literal_is_refused_on_the_expression_cap() {
    let mut c = common::expression_capped(EXPRESSION_CAP);
    let elements = query_sized_elements(300_000);
    let q = format!("UNWIND [{elements}] AS x RETURN count(x)");
    common::refused_expression_cap(&mut c, &q);
}

/// A small literal is unaffected: the charge is a bound check, and every
/// ordinary `[1, 2, 3]` in every expression still evaluates.
#[test]
fn a_small_list_literal_evaluates_under_the_cap() {
    let mut c = common::expression_capped(EXPRESSION_CAP);
    let values = common::first_column(&common::run(&mut c, "UNWIND [1, 2, 3] AS x RETURN sum(x)"));
    assert_eq!(values, vec![Value::Int(6)]);
}

/// The map literal is the list literal's twin — the same query-text
/// bounded container class, charged with the map-entry figure its
/// projection form bills. A map entry's figure (24 B + key bytes) is
/// smaller than a value slot, so the pinning cap is half the file's
/// other one: 350k query-sized entries bill well past 8 MiB.
#[test]
fn a_query_sized_map_literal_is_refused_on_the_expression_cap() {
    let mut c = common::expression_capped(EXPRESSION_CAP / 2);
    let elements = (0..350_000)
        .map(|i| format!("k{i}: 1"))
        .collect::<Vec<_>>()
        .join(",");
    let q = format!("RETURN {{{elements}}} AS m");
    common::refused_expression_cap(&mut c, &q);
}

/// The aggregate evaluator's literals charge too: a non-aggregate
/// projection item beside an aggregation evaluates through
/// `exec/aggregate/eval.rs`, which rebuilds the literal and hands it to
/// scalar `eval` — whose `Expr::List`/`Expr::Map` arms charge it — so a
/// query-sized literal refuses on the expression cap in an aggregating
/// projection just as it does in a plain one.
#[test]
fn a_query_sized_literal_in_an_aggregating_projection_is_refused() {
    let mut c = common::expression_capped(EXPRESSION_CAP);
    let elements = query_sized_elements(300_000);
    let q = format!("UNWIND range(1, 3) AS i RETURN [{elements}] AS xs, count(i)");
    common::refused_expression_cap(&mut c, &q);
}

/// REL8-1372's residual, pinned at its new home: a pattern-comprehension
/// source still refuses under a tight expression cap — the comprehension
/// is its `collect { … }` subquery (ADR-0685), so the element list is the
/// nested engine's final page materialized as one list value, and `UNWIND`
/// over a list value needs the whole list. Its collect-subquery spelling
/// refuses identically: the two forms are one evaluation path, and the
/// folding win is bounded by the same admission both spellings pay.
#[test]
fn a_pattern_comprehension_source_refuses_like_its_collect_subquery() {
    let (_dir, mut c) = common::reopened_expression_capped(
        EXPRESSION_CAP,
        &["CREATE (n:N {id: 1}) WITH n \
           UNWIND range(1, 400_000) AS i CREATE (n)-[:R {v: i}]->(:M {v: i})"],
    );
    common::refused_expression_cap(
        &mut c,
        "MATCH (n:N {id: 1}) UNWIND [(n)-[:R]->(m) | m.v] AS x RETURN sum(x)",
    );
    common::refused_expression_cap(
        &mut c,
        "MATCH (n:N {id: 1}) \
         UNWIND collect { MATCH (n)-[:R]->(m) RETURN m.v } AS x \
         RETURN sum(x)",
    );
}
