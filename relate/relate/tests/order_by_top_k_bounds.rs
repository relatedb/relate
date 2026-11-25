//! LABS-1373: `ORDER BY ... LIMIT/SKIP` bounds the sort's own container.
//!
//! ADR-0555 declares the unlimited sort a breaker, but a sort with a
//! `LIMIT` (plus its `SKIP`) needs only the first `skip + limit` rows of
//! the order: the projection folds its producer into a charged top-k heap
//! of that capacity and never holds more. The memory-shape test below is
//! the ADR-0555 proof — two inputs an order of magnitude apart, one
//! bounded workspace, and the heap's charge independent of the input
//! count. The differential test pins that the bounded selection answers
//! exactly what a full sort answers.

mod common;

use relate::Value;

/// The workspace budget the bounded shape must fit inside: generous for
/// five retained rows, impossible for the full input at the sizes below.
const BUDGET: u64 = 16 << 20;

/// Assert that every spelling of a zero page answers nothing — the one
/// spelling of the check both zero-page tests read.
fn assert_limit_zero_answers_nothing(c: &mut relate::Connection, queries: &[&str]) {
    for q in queries {
        assert!(
            common::rows(c, q).is_empty(),
            "a LIMIT 0 answers nothing: {q}"
        );
    }
}

/// One `ORDER BY ... LIMIT` over `n` generated rows under the bounded
/// workspace: the answer is the last five ids, whatever `n` is.
fn top_five(conn: &mut relate::Connection, n: i64) -> Vec<Value> {
    let q = format!("UNWIND range(1, {n}) AS i WITH i AS v ORDER BY v DESC LIMIT 5 RETURN v");
    common::first_column(&common::run(conn, &q))
}

/// The generated feed never materializes: `UNWIND` streams into any
/// consumer that can fold, and the ordered window is a fold — its `land`
/// runs per row, every row is evaluated to compare it, so parking the
/// generator changes no answer. The statement therefore holds only its
/// `skip + limit` answer, and the admission pool sees nothing
/// input-proportional: 2M rows answer under the same 16 MiB pool that
/// refused 200k before the window existed.
///
/// This test is also the **guard for the streaming invariant** — "an
/// `UNWIND` feed is never materialized into a consumer that can fold" is
/// pinned here, not in a doc: any change that collects the generated feed
/// on this shape (a landing that no longer folds, a dispatcher guard that
/// stops parking the generator) puts the input back on the pool and fails
/// this test with a `concurrent-logical-workspace-limit` refusal.
#[test]
fn an_unwind_feed_streams_into_the_window_whatever_the_input_count() {
    let mut c = common::workspace_capped(BUDGET);
    assert_eq!(
        top_five(&mut c, 2_000_000),
        vec![
            Value::Int(2_000_000),
            Value::Int(1_999_999),
            Value::Int(1_999_998),
            Value::Int(1_999_997),
            Value::Int(1_999_996)
        ]
    );
}

/// A `MATCH`-fed `ORDER BY … LIMIT` folds the walk's rows into the same
/// bounded window a generated feed reaches: the projection holds
/// `skip + limit` entries, never the input. 30k one-column rows answer under
/// a 2 MiB pool the held feed could not fit.
#[test]
fn a_match_fed_window_holds_only_its_page() {
    let (_dir, mut c) = common::reopened_under(
        2 << 20,
        &["UNWIND range(1, 30_000) AS i CREATE (:N {a: i})"],
    );
    let values = common::first_column(&common::run(
        &mut c,
        "MATCH (n:N) WITH n.a AS a ORDER BY a DESC LIMIT 5 RETURN a",
    ));
    assert_eq!(
        values,
        vec![
            Value::Int(30_000),
            Value::Int(29_999),
            Value::Int(29_998),
            Value::Int(29_997),
            Value::Int(29_996)
        ]
    );
    // The MATCH's own `WHERE` still gates what reaches the window.
    let values = common::first_column(&common::run(
        &mut c,
        "MATCH (n:N) WHERE n.a <= 5000 WITH n.a AS a ORDER BY a DESC LIMIT 5 RETURN a",
    ));
    assert_eq!(
        values,
        vec![
            Value::Int(5_000),
            Value::Int(4_999),
            Value::Int(4_998),
            Value::Int(4_997),
            Value::Int(4_996)
        ]
    );
}

/// A run of stages ending at an ordered page folds the last stage's rows
/// into the window too: two `MATCH` clauses over 300 nodes each are a
/// 90 000-row cross product, and the five-row page answers under a 2 MiB
/// pool that holds nowhere near the product.
#[test]
fn a_staged_match_run_feeds_the_window() {
    let (_dir, mut c) = common::reopened_under(
        2 << 20,
        &[
            "UNWIND range(1, 300) AS i CREATE (:A {k: i})",
            "UNWIND range(1, 300) AS i CREATE (:B {k: i})",
        ],
    );
    let rows = common::rows(
        &mut c,
        "MATCH (a:A) WITH a MATCH (b:B) \
         RETURN a.k AS a, b.k AS b ORDER BY a DESC, b DESC LIMIT 5",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int(300), Value::Int(300)],
            vec![Value::Int(300), Value::Int(299)],
            vec![Value::Int(300), Value::Int(298)],
            vec![Value::Int(300), Value::Int(297)],
            vec![Value::Int(300), Value::Int(296)],
        ]
    );
}

/// A nondeterministic page bound stays on the held path: the sink cannot
/// size its window for a page the projection may resolve differently, so the
/// `MATCH` holds its rows (refused under this pool) instead of answering a
/// page the window guessed.
#[test]
fn a_nondeterministic_page_bound_keeps_the_held_path() {
    let (_dir, mut c) = common::reopened_under(
        2 << 20,
        &["UNWIND range(1, 30_000) AS i CREATE (:N {a: i})"],
    );
    let error = common::err_text(c.query(
        "MATCH (n:N) WITH n.a AS a ORDER BY a DESC LIMIT toInteger(rand() * 5 + 1) RETURN a",
    ));
    assert!(
        error.contains("concurrent-logical-workspace-limit"),
        "the nondeterministic bound keeps the held path: {error}"
    );
}

/// A provably empty ordered page (`LIMIT 0`) over a MATCH feed takes the
/// count-only sink: the 50k-row input is never collected, so the empty
/// answer fits a 2 MiB pool the held path could not.
#[test]
fn a_match_fed_window_limit_zero_answers_nothing_without_collecting() {
    let (_dir, mut c) = common::reopened_under(
        2 << 20,
        &["UNWIND range(1, 50_000) AS i CREATE (:N {k: i})"],
    );
    let rows = common::rows(
        &mut c,
        "MATCH (n:N) WHERE n.k > 0 WITH n.k AS k ORDER BY k DESC LIMIT 0 RETURN k",
    );
    assert!(rows.is_empty());
}

/// The window is charged even when the workspace ledger is off (the
/// default): a `LIMIT` large enough to accumulate an output past the
/// per-allocation cap is refused on the window's live total alone. The
/// producer stays tiny — fifty one-slot rows — so the projection's own
/// container is the only thing that can cross the cap.
#[test]
fn a_large_ordered_window_is_refused_without_a_workspace_ledger() {
    let mut c = common::expression_capped(common::EXPRESSION_CAP);
    // Each projected row carries one ~128 KB list — every single
    // allocation stays under the cap, so the only quantity that can cross
    // it is the window's live total over fifty entries.
    let q = "UNWIND range(1, 50) AS i \
             WITH i, range(1, 8000) AS pad \
             ORDER BY i DESC LIMIT 50 \
             RETURN i";
    common::refused_expression_cap(&mut c, q);
}

/// The selection answers what a full sort answers: mixed ASC/DESC keys,
/// ties broken by arrival order, and `SKIP` paying its share of the
/// window. The unbounded spellings (no `LIMIT`) are the oracle.
#[test]
fn order_by_skip_limit_answers_what_the_full_sort_answers() {
    let mut c = common::connection();
    // `b` from a bounded set so `(a, b)` ties exist: the oracle's stable
    // full sort keeps arrival order, so the equality assertion pins the
    // window's arrival-order tiebreak (`TopKEntry::seq`) too.
    let setup = "UNWIND range(1, 500) AS i CREATE (:N {a: i % 7, b: i % 5})";
    common::run(&mut c, setup);

    let bounded =
        "MATCH (n:N) WITH n.a AS a, n.b AS b ORDER BY a ASC, b DESC SKIP 3 LIMIT 8 RETURN a, b";
    let oracle =
        "MATCH (n:N) WITH n.a AS a, n.b AS b ORDER BY a ASC, b DESC RETURN a, b SKIP 3 LIMIT 8";
    assert_eq!(
        common::rows(&mut c, bounded),
        common::rows(&mut c, oracle),
        "the bounded window must equal the full sort's page"
    );
}

/// A zero page answers nothing, in every spelling: the clause decides it
/// before a container is opened, so a `LIMIT 0` costs nothing whatever the
/// input. The assertions observe the answer — the shape of the decision
/// itself is not visible from a query — so what this pins is that the
/// answer stays empty across the shapes, not which path produced it.
#[test]
fn order_by_limit_zero_answers_nothing() {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND range(1, 100) AS i CREATE (:N {a: i})");
    let queries = [
        "MATCH (n:N) RETURN n.a AS a ORDER BY a LIMIT 0",
        "MATCH (n:N) RETURN n.a AS a LIMIT 0",
        "MATCH (n:N) RETURN n.a AS a SKIP 5 LIMIT 0",
        "MATCH (n:N) WITH n.a AS a ORDER BY a DESC LIMIT 0 RETURN a",
    ];
    assert_limit_zero_answers_nothing(&mut c, &queries);
}

/// The zero page is decided before any container is opened, so it costs
/// nothing whatever the input. The same 16 MiB workspace that a full
/// collection cannot fit answers an empty page over two million rows —
/// the assertion that tells the decision apart from collecting and then
/// paging, which the answer alone cannot.
#[test]
fn a_limit_zero_costs_nothing_whatever_the_input() {
    let mut c = common::workspace_capped(BUDGET);
    // Every spelling that can carry a zero page, including the `SKIP` forms
    // whose capacity arithmetic (`skip + 0`) would otherwise still be
    // positive and build a window.
    let queries = [
        "UNWIND range(1, 2_000_000) AS i WITH i AS v ORDER BY v LIMIT 0 RETURN v",
        "UNWIND range(1, 2_000_000) AS i WITH i AS v ORDER BY v SKIP 5 LIMIT 0 RETURN v",
        "UNWIND range(1, 2_000_000) AS i WITH i AS v ORDER BY v DESC SKIP 0 LIMIT 0 RETURN v",
    ];
    assert_limit_zero_answers_nothing(&mut c, &queries);
}

/// A projection-level `WHERE` written *before* the page: the plan for that
/// spelling is not the window — `WITH … WHERE … ORDER BY … LIMIT` splits
/// into a `WHERE` projection and a keyed `LIMIT` one, so the filter runs
/// before the page and the page is the filtered sort's. The fused spelling
/// (`WITH … ORDER BY … LIMIT … WHERE`, which pages first) lives in
/// `with_where_after_page.rs`.
#[test]
fn an_ordered_window_filters_before_it_selects() {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND range(1, 500) AS i CREATE (:N {a: i})");

    let bounded = "MATCH (n:N) WITH n.a AS a WHERE a % 2 = 0 ORDER BY a DESC LIMIT 5 RETURN a";
    // The pin: an unfiltered window would answer 500, 499, 498, … here.
    let values: Vec<Value> = common::first_column(&common::run(&mut c, bounded));
    assert_eq!(
        values,
        vec![
            Value::Int(500),
            Value::Int(498),
            Value::Int(496),
            Value::Int(494),
            Value::Int(492)
        ],
        "the window must select from the filtered stream"
    );

    // And the page still equals the unbounded filtered sort's page.
    let oracle = "MATCH (n:N) WITH n.a AS a WHERE a % 2 = 0 ORDER BY a DESC RETURN a";
    let mut expected = common::rows(&mut c, oracle);
    expected.truncate(5);
    assert_eq!(common::rows(&mut c, bounded), expected);
}
