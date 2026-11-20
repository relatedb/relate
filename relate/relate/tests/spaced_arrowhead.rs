//! Whitespace between a relationship pattern's dash and its arrowheads:
//! live Neo4j 2026.10 executes `- [...] - >`, `-- >` and `< - [...] -`
//! exactly like the glued `->`, `-->` and `<- [...] -`, so the spaced
//! spellings resolve to the same directions here. The single-dash forms
//! (`- >` without a bracket body or closing dash, `->` without a leading
//! dash) stay rejections on both sides of the whitespace, and a `<` in an
//! expression context keeps its comparison meaning (`(a) < -1`).

mod common;

use common::{connection, err_text, first_column};
use relate::Value;

/// One `:L1`/`:L2` pair with an `:R` edge each way, so every direction
/// has a matching row to return.
fn pair_store() -> relate::Connection {
    let mut c = connection();
    c.query("CREATE (a:L1 {p1: 1}), (b:L2 {p1: 2}), (a)-[:R]->(b), (b)-[:R]->(a)")
        .unwrap();
    c
}

/// Each spaced spelling returns the same rows as its glued twin, and the
/// head and tail spellings mix freely.
#[test]
fn spaced_arrowhead_forms_match_the_glued_forms_row_for_row() {
    let mut c = pair_store();
    for (spaced, glued) in [
        (
            "MATCH (a:L1) - [:R] - > (b:L2) RETURN b.p1",
            "MATCH (a:L1) - [:R] -> (b:L2) RETURN b.p1",
        ),
        (
            "MATCH (a:L1) -- > (b:L2) RETURN b.p1",
            "MATCH (a:L1) --> (b:L2) RETURN b.p1",
        ),
        (
            "MATCH (a:L1) < - [:R] - (b:L2) RETURN a.p1",
            "MATCH (a:L1) <- [:R] - (b:L2) RETURN a.p1",
        ),
        (
            "MATCH (a:L1) < -- (b:L2) RETURN a.p1",
            "MATCH (a:L1) <-- (b:L2) RETURN a.p1",
        ),
        (
            "MATCH (a:L1) < - [:R] -> (b:L2) RETURN count(*) AS n",
            "MATCH (a:L1) <- [:R] -> (b:L2) RETURN count(*) AS n",
        ),
        (
            "MATCH (a:L1) <- [:R] - > (b:L2) RETURN count(*) AS n",
            "MATCH (a:L1) <- [:R] -> (b:L2) RETURN count(*) AS n",
        ),
    ] {
        let spaced_rows = first_column(&c.query(spaced).unwrap());
        assert!(
            !spaced_rows.is_empty(),
            "{spaced}: row equality with {glued} is vacuous without rows"
        );
        assert_eq!(
            spaced_rows,
            first_column(&c.query(glued).unwrap()),
            "{spaced}"
        );
    }
}

/// The variable-length bracket body rides the spaced tail: the exact
/// failing shape from the gap (`EXPLAIN`-wrapped or not) parses, plans
/// and executes.
#[test]
fn var_length_spaced_arrowhead_executes_and_plans() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 1})-[:R]->(:M {p1: 3})-[:R]->(:L2 {p1: 2})")
        .unwrap();
    for q in [
        "MATCH (a:L1) - [:R*2] - > (b:L2) RETURN b.p1",
        "MATCH (a:L1) - [:R*2] -> (b:L2) RETURN b.p1",
    ] {
        assert_eq!(
            first_column(&c.query(q).unwrap()),
            vec![Value::Int(2)],
            "{q}"
        );
    }
    // The EXPLAIN-wrapped spelling plans too, not just executes.
    c.query("EXPLAIN MATCH (a:L1) - [:R*2] - > (b:L2) RETURN b")
        .unwrap();
}

/// A lone head dash (no bracket body, no closing dash) cannot carry an
/// arrowhead, spaced or glued: both reject from the relationship pattern
/// itself, each naming what its position expected.
#[test]
fn a_single_dash_arrowhead_spaced_or_glued_stays_rejected() {
    let mut c = pair_store();
    let spaced = err_text(c.query("MATCH (a:L1) - > (b:L2) RETURN b"));
    assert!(
        spaced.contains("expected -> or -"),
        "the spaced form rejects at the tail: {spaced}"
    );
    let glued = err_text(c.query("MATCH (a:L1)->(b:L2) RETURN b"));
    assert!(
        glued.contains("expected - or <-"),
        "the glued form rejects at the head: {glued}"
    );
}

/// The tail runs before the clause WHERE, so a comparison `>` there is
/// never swallowed; inside a pattern predicate (claimed through the
/// leading dash) the spaced tail parses too; and in an expression a `<`
/// after a `)` stays a comparison, so the pattern gate never claims it.
#[test]
fn comparisons_keep_their_meaning_around_spaced_arrowheads() {
    let mut c = pair_store();
    assert_eq!(
        first_column(
            &c.query("MATCH (a:L1) - [:R] - > (b:L2) WHERE a.p1 > 0 RETURN b.p1")
                .unwrap()
        ),
        vec![Value::Int(2)]
    );
    assert_eq!(
        first_column(
            &c.query("MATCH (a:L1) WHERE (a) - [:R] - > () RETURN count(*) AS n")
                .unwrap()
        ),
        vec![Value::Int(1)]
    );
    assert_eq!(
        first_column(&c.query("MATCH (a:L1) RETURN (a) < -1 AS lt").unwrap()),
        vec![Value::Null]
    );
    assert_eq!(
        first_column(&c.query("RETURN (1) < (2) AS lt").unwrap()),
        vec![Value::Bool(true)]
    );
}

/// The `p = <pattern> | …` comprehension gate runs the real pattern reader
/// speculatively, so it recognizes the spaced left head once the reader
/// does.
#[test]
fn a_named_pattern_comprehension_recognizes_the_spaced_left_head() {
    let mut c = pair_store();
    assert_eq!(
        first_column(
            &c.query("MATCH (a:L1) RETURN [p = (a) < - [:R] - (b:L2) | b.p1] AS ps")
                .unwrap()
        ),
        vec![Value::List(vec![Value::Int(2)].into())]
    );
}
