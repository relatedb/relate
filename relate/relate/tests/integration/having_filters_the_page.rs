//! REL8-2135: an aggregating `WITH`'s trailing `WHERE` — the HAVING form,
//! whose predicate carries an aggregate call — filters the **page** the
//! projection body produced, not the groups. The `WITH` grammar is
//! `WITH projection [ORDER BY] [SKIP] [LIMIT] [WHERE]` (openCypher
//! `With = WITH ProjectionBody [Where]`), so the page is taken from the
//! grouped rows and the aggregate predicate filters what it kept. Live
//! Neo4j 2026.10.2 answers every expectation here (Query API v2 probes,
//! 2026-11-26); the aggregation parks the predicate's per-group value
//! under a hidden column and the finish tail filters the paged rows on
//! it — the same position the plain trailing `WHERE`'s filter took
//! (REL8-2076 finding 33).

use crate::common;

use common::{connection, ints, run, scalar_one};

/// The corpus probe's input over a fresh connection: three 1s, two 2s,
/// one 3 — group counts 3, 2, 1.
fn unbound() -> relate::Connection {
    let mut c = connection();
    run(
        &mut c,
        "UNWIND [1,1,1,2,2,3] AS x WITH x AS u CREATE (:U {v: u})",
    );
    c
}

/// The discriminating probe: the page (`ORDER BY g DESC LIMIT 2`) keeps
/// groups 3 and 2, and neither survives `count(*) > 2` — the answer is
/// empty, not group 1. Filtering the groups before the page (the old
/// placement) answers `[1]`.
#[test]
fn a_having_filters_the_page_not_the_groups() {
    let mut c = unbound();
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (u:U) WITH u.v AS g, count(*) AS c ORDER BY g DESC LIMIT 2 \
             WHERE count(*) > 2 RETURN collect(g) AS gs"
        ),
        ints(&[]),
        "the page's groups are 3 and 2; neither has count > 2"
    );
}

/// The surviving spelling: the same page, a predicate the paged groups
/// pass — group 2 survives, group 3 does not.
#[test]
fn a_having_keeps_the_paged_rows_that_pass() {
    let mut c = unbound();
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (u:U) WITH u.v AS g, count(*) AS c ORDER BY g DESC LIMIT 2 \
             WHERE count(*) >= 2 RETURN collect(g) AS gs"
        ),
        ints(&[2])
    );
}

/// Without a page the position is unobservable: the same groups answer,
/// with or without the ORDER BY, so the no-page form pins the
/// equivalence with the group-level semantics.
#[test]
fn a_having_without_a_page_answers_the_surviving_groups() {
    let mut c = unbound();
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (u:U) WITH u.v AS g, count(*) AS c WHERE count(*) > 2 RETURN collect(g) AS gs"
        ),
        ints(&[1])
    );
}

/// SKIP shares the page: `ORDER BY g ASC SKIP 1 LIMIT 2` pages groups 2
/// and 3, and `count(*) > 1` keeps only 2.
#[test]
fn a_having_after_skip_answers_the_paged_rows() {
    let mut c = unbound();
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (u:U) WITH u.v AS g, count(*) AS c ORDER BY g ASC SKIP 1 LIMIT 2 \
             WHERE count(*) > 1 RETURN collect(g) AS gs"
        ),
        ints(&[2])
    );
}

/// A full page over every group keeps the group order the sort produced:
/// ascending over 1, 2, 3 with `count(*) >= 2` answers 1 then 2.
#[test]
fn a_having_keeps_the_sorts_order_over_the_paged_rows() {
    let mut c = unbound();
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (u:U) WITH u.v AS g, count(*) AS c ORDER BY g ASC LIMIT 3 \
             WHERE count(*) >= 2 RETURN collect(g) AS gs"
        ),
        ints(&[1, 2])
    );
}

/// The paged rows a HAVING drops never reach the clause after it: a write
/// fed by the filtered page sees only the survivors.
#[test]
fn a_having_gates_the_write_behind_the_page() {
    let mut c = unbound();
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (u:U) WITH u.v AS g, count(*) AS c ORDER BY g DESC LIMIT 2 \
             WHERE count(*) >= 2 CREATE (:Kept {g: g}) RETURN collect(g) AS gs"
        ),
        ints(&[2]),
        "group 2 is the page's only survivor"
    );
    assert_eq!(
        scalar_one(&mut c, "MATCH (k:Kept) RETURN count(*) AS n"),
        common::int(1),
        "the write saw the filtered page, not the groups"
    );
}

/// A predicate whose aggregate is not among the projection's items is a
/// static rejection both engines share (live 2026.10: `Type mismatch:
/// expected Boolean`-class SyntaxError) — pinned as the shared refusal,
/// not silently evaluated.
#[test]
fn a_having_naming_an_unprojected_aggregate_is_refused() {
    let mut c = unbound();
    let err = common::refused_literal(
        &mut c,
        "MATCH (u:U) WITH u.v AS g WHERE count(*) > 2 RETURN collect(g) AS gs",
    );
    let text = err.to_string();
    assert!(text.contains("InvalidAggregation"), "{text}");
}

/// `LIMIT 0` pages to nothing, so the HAVING never evaluates — the
/// statement answers empty rather than raising or leaking groups.
#[test]
fn a_having_after_a_zero_page_answers_nothing() {
    let mut c = unbound();
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (u:U) WITH u.v AS g, count(*) AS c ORDER BY g DESC LIMIT 0 \
             WHERE count(*) > 2 RETURN collect(g) AS gs"
        ),
        ints(&[])
    );
}
