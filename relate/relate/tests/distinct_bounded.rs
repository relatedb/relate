//! ADR-0555 §3 / ADR-0660's first-seen distinct pass over a `MATCH` feed as
//! well as a generated one: a named `DISTINCT` holds the distinct keys and
//! the rows it kept, not the projected input. The memory tests run under a
//! pool the input cannot fit; the differentials compare the folded answer
//! against the grouping reference and the unbounded spellings.

mod common;

use relate::Value;

/// A store of 20 000 `:N` nodes whose `s` cycles through 20 strings, built
/// unlimited and reopened under `pool` bytes of workspace.
fn twenty_thousand(pool: u64) -> (tempfile::TempDir, relate::Connection) {
    common::reopened_under(
        pool,
        &["UNWIND range(1, 20000) AS i CREATE (:N {k: i, s: 's' + toString(i % 20)})"],
    )
}

/// `RETURN DISTINCT` and `WITH DISTINCT` over a MATCH fold their rows into
/// the seen-set as they land: 20 000 rows with 20 distinct keys answer under
/// a 400 KB pool that the collected input cannot fit.
#[test]
fn a_match_fed_distinct_holds_only_its_keys() {
    let (_dir, mut c) = twenty_thousand(400 << 10);
    for q in [
        "MATCH (n:N) RETURN DISTINCT n.s AS s",
        "MATCH (n:N) WITH DISTINCT n.s AS s RETURN s",
    ] {
        let mut values: Vec<String> = common::first_column(&common::run(&mut c, q))
            .into_iter()
            .map(|v| match v {
                Value::String(s) => s.to_string(),
                other => panic!("{q}: a non-string key {other:?}"),
            })
            .collect();
        values.sort();
        assert_eq!(values.len(), 20, "{q}: every distinct key once");
        assert!(values.contains(&"s0".to_string()), "{q}: {values:?}");
        assert!(values.contains(&"s19".to_string()), "{q}: {values:?}");
    }
}

/// The generated feed keeps the same bounded shape: an `UNWIND`-fed
/// `DISTINCT` keeps only the 20 seen keys under the 400 KB pool.
#[test]
fn a_generated_feed_distinct_holds_only_its_keys() {
    let (_dir, mut c) = twenty_thousand(400 << 10);
    let mut values: Vec<i64> = common::first_column(&common::run(
        &mut c,
        "UNWIND range(1, 20000) AS x RETURN DISTINCT x % 20 AS s",
    ))
    .into_iter()
    .map(|v| match v {
        Value::Int(i) => i,
        other => panic!("a non-int key {other:?}"),
    })
    .collect();
    values.sort();
    assert_eq!(values, (0..20).collect::<Vec<_>>());
}

/// A `WHERE` written on the distinct clause itself filters each row before
/// its key is taken: only the keys whose value passes survive.
#[test]
fn a_fused_distinct_where_filters_before_the_key() {
    let (_dir, mut c) = twenty_thousand(16 << 20);
    let mut values = common::first_column_strings(&common::run(
        &mut c,
        "MATCH (n:N) WITH DISTINCT n.s AS s WHERE s STARTS WITH 's1' RETURN s",
    ));
    values.sort();
    let mut expected: Vec<String> = std::iter::once("s1".to_string())
        .chain((10..20).map(|i| format!("s{i}")))
        .collect();
    expected.sort();
    assert_eq!(values, expected);
}

/// The folded answer is the grouping reference's, over a plain pool.
#[test]
fn a_distinct_fold_answers_what_the_grouping_answers() {
    let (_dir, mut c) = twenty_thousand(16 << 20);
    let keys = |q: &str, c: &mut relate::Connection| {
        let mut keys: Vec<String> = common::first_column(&common::run(c, q))
            .into_iter()
            .map(|v| format!("{v:?}"))
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(
        keys("MATCH (n:N) RETURN DISTINCT n.s AS s", &mut c),
        keys("MATCH (n:N) WITH n.s AS s, count(*) AS c RETURN s", &mut c),
        "DISTINCT and the grouping agree on the keys"
    );
    // Post-key ordering and paging stay the tail's.
    let page = common::first_column(&common::run(
        &mut c,
        "MATCH (n:N) RETURN DISTINCT n.s AS s ORDER BY s DESC LIMIT 5",
    ));
    let got: Vec<String> = page
        .into_iter()
        .map(|v| match v {
            Value::String(s) => s.to_string(),
            other => panic!("a non-string row {other:?}"),
        })
        .collect();
    assert_eq!(got, vec!["s9", "s8", "s7", "s6", "s5"]);
}

/// A provably empty page needs no keys: `DISTINCT … LIMIT 0` answers empty
/// without projecting or keying the input, so 200k distinct keys fit the
/// 400 KB pool (a WHERE-bearing MATCH keeps the plain limit cap off, so the
/// sink is the path).
#[test]
fn a_distinct_limit_zero_answers_nothing_without_keying() {
    let (_dir, mut c) = common::reopened_under(
        400 << 10,
        &["UNWIND range(1, 200_000) AS i CREATE (:N {k: i})"],
    );
    let rows = common::rows(
        &mut c,
        "MATCH (n:N) WHERE n.k > 0 RETURN DISTINCT n.k AS k LIMIT 0",
    );
    assert!(rows.is_empty());
}
