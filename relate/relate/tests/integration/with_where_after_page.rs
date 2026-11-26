//! The `WITH` grammar's trailing `WHERE` filters the page the projection
//! body produced — `WITH projection [ORDER BY] [SKIP] [LIMIT] [WHERE]`
//! (REL8-2076 finding 33). Filtering before the page takes the page from
//! the filtered rows instead and answers the wrong rows, including rows a
//! following write then acts on. The `SHOW … YIELD` spelling already pages
//! before its `WHERE`; both spellings must agree.

use crate::common;

use relate::Value;

/// The four rows of the corpus repro: with an `ORDER BY`/`SKIP`/`LIMIT`
/// page, the trailing `WHERE` keeps only the page's own rows that pass —
/// never the first `limit` rows that pass out of the whole input.
#[test]
fn a_with_where_filters_the_page_its_order_skip_limit_produced() {
    let mut c = common::connection();
    for (statement, expected) in [
        (
            "UNWIND range(1, 20) AS x WITH x ORDER BY x LIMIT 5 WHERE x > 3 RETURN collect(x) AS c",
            common::ints(&[4, 5]),
        ),
        (
            "UNWIND range(1, 20) AS x WITH x, x % 3 AS m ORDER BY m, x LIMIT 4 WHERE m = 1 \
             RETURN collect(x) AS c",
            common::ints(&[]),
        ),
        (
            "UNWIND range(1, 20) AS x WITH x ORDER BY x SKIP 2 LIMIT 5 WHERE x % 2 = 0 \
             RETURN collect(x) AS c",
            common::ints(&[4, 6]),
        ),
        (
            "UNWIND [1, 2, 3, 9] AS id WITH id ORDER BY id LIMIT 1 WHERE id > 5 \
             RETURN collect(id) AS c",
            common::ints(&[]),
        ),
    ] {
        assert_eq!(
            common::scalar_one(&mut c, statement),
            expected,
            "{statement}"
        );
    }
}

/// Without an `ORDER BY`, `LIMIT n WHERE p` keeps the first `n` rows and
/// then filters them: the page is cut from the unfiltered stream.
#[test]
fn a_with_limit_then_where_keeps_the_first_rows_of_the_page() {
    let mut c = common::connection();
    assert_eq!(
        common::scalar_one(
            &mut c,
            "UNWIND range(1, 20) AS x WITH x LIMIT 5 WHERE x > 3 RETURN collect(x) AS c"
        ),
        common::ints(&[4, 5])
    );
    assert_eq!(
        common::scalar_one(
            &mut c,
            "UNWIND range(1, 20) AS x WITH x SKIP 2 LIMIT 5 WHERE x > 100 RETURN collect(x) AS c"
        ),
        common::ints(&[])
    );
}

/// The wrong row never reaches a write: only the page's survivors are
/// visible to the `SET` that follows the `WITH`.
#[test]
fn a_with_page_then_where_feeds_a_write_only_the_surviving_rows() {
    let mut c = common::connection();
    common::run(
        &mut c,
        "CREATE (:N {id: 1}), (:N {id: 2}), (:N {id: 3}), (:N {id: 9})",
    );
    assert_eq!(
        common::scalar_one(
            &mut c,
            "MATCH (n:N) WITH n ORDER BY n.id LIMIT 1 WHERE n.id > 5 \
             RETURN collect(n.id) AS c"
        ),
        common::ints(&[])
    );
    common::run(
        &mut c,
        "MATCH (n:N) WITH n ORDER BY n.id LIMIT 1 WHERE n.id > 5 SET n.marked = true",
    );
    assert_eq!(
        common::scalar(&mut c, "MATCH (n:N) WHERE n.marked RETURN count(*) AS c"),
        Value::Int(0)
    );
}

/// The two spellings of one grammar agree: the `SHOW … YIELD` paging
/// already filters after its page, and the `WITH` projection that follows
/// it now does too.
#[test]
fn the_show_and_with_spellings_page_before_the_where() {
    let mut c = common::connection();
    let show = common::scalar_one(
        &mut c,
        "SHOW FUNCTIONS YIELD name ORDER BY name LIMIT 5 WHERE name STARTS WITH 'c' \
         RETURN collect(name) AS c",
    );
    let with = common::scalar_one(
        &mut c,
        "SHOW FUNCTIONS YIELD name WITH name ORDER BY name LIMIT 5 \
         WHERE name STARTS WITH 'c' RETURN collect(name) AS c",
    );
    assert_eq!(
        show,
        common::strings(&[]),
        "the SHOW spelling pages before its WHERE"
    );
    assert_eq!(with, show, "the WITH spelling pages before its WHERE too");
}

/// Filtering after the selection does not relax the bounded window: the
/// two-million-row generator still folds into the top-k heap, so the tiny
/// cap answers it (ADR-0555 §3).
#[test]
fn the_ordered_window_still_bounds_its_input_under_a_trailing_where() {
    let mut c = common::expression_capped(common::EXPRESSION_CAP);
    let large = common::LARGE_INPUT;
    assert_eq!(
        common::scalar_one(
            &mut c,
            &format!(
                "UNWIND range(1, {large}) AS i WITH i AS v ORDER BY v DESC LIMIT 5 \
                 WHERE v > {} RETURN collect(v) AS c",
                large - 3
            )
        ),
        common::ints(&[large, large - 1, large - 2])
    );
}

/// The pass-through page still stops its generator: the page counts rows
/// before the trailing `WHERE` sees them, so five rows are pulled and the
/// two-million-row input never lands (ADR-0660).
#[test]
fn a_with_page_still_stops_its_generator_under_a_trailing_where() {
    let mut c = common::expression_capped(common::EXPRESSION_CAP);
    assert_eq!(
        common::scalar_one(
            &mut c,
            &format!(
                "UNWIND range(1, {}) AS i WITH i AS v LIMIT 5 WHERE v > 3 RETURN collect(v) AS c",
                common::LARGE_INPUT
            )
        ),
        common::ints(&[4, 5])
    );
}

/// The fused spelling and the split spelling are two different queries: the
/// fused `WITH … ORDER BY … LIMIT … WHERE` filters the page the window
/// selected, while `WITH … WHERE` followed by a page statement filters
/// before it pages. Their answers differ whenever the predicate is not
/// order-preserving.
#[test]
fn the_fused_and_split_spellings_answer_differently() {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND range(1, 10) AS i CREATE (:N {a: i})");

    let fused = "MATCH (n:N) WITH n.a AS a ORDER BY a LIMIT 3 WHERE a >= 4 RETURN a";
    let split = "MATCH (n:N) WITH n.a AS a WHERE a >= 4 ORDER BY a LIMIT 3 RETURN a";
    assert_eq!(
        common::first_column(&common::run(&mut c, fused)),
        Vec::new(),
        "the window selects the first three rows, then the trailing WHERE filters them"
    );
    assert_eq!(
        common::first_column(&common::run(&mut c, split)),
        vec![Value::Int(4), Value::Int(5), Value::Int(6)],
        "the split spelling filters before it pages"
    );
}
