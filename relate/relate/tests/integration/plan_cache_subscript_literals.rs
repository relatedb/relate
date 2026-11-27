//! The plan cache's bracketed-literal-subscript family (REL8-1423): a
//! statement whose literal sits in a postfix bracket — `labels(n)[0]`,
//! `[1, 2, 3][0]`, `xs[1..3]` — must participate in the plan cache like
//! any other literal-bearing statement. The normalizer's list-literal
//! collapse used to swallow the subscript's brackets into one bare list
//! parameter (`labels(n)$__AUTO_LIST_0__`), a syntax error, so the
//! rewritten compile failed and every such statement was a permanent
//! cache miss — its literals re-parsed and re-planned on every execution.
//! These tests pin both participation stages: the first execution
//! compiles and records the entry with its raw text, so the identical
//! repeat takes the raw-string fast path with identical answers, and a
//! CHANGED literal is the same template — a template hit whose extracted
//! lookup value drives the (different) answer.

use crate::common;

use relate::PlanCacheOutcome;

/// The participation protocol every literal-bearing statement must make:
/// the first execution compiles (a `Miss` — the insert also records the
/// entry's raw text beside it, so the identical repeat resolves through
/// stage 1 — deterministically, not either hit flavor), and the raw-hit
/// repeat answers exactly what the miss answered. Returns the miss's
/// result — the hit's rows are already asserted equal inside, so a caller
/// wanting rows wants these rows.
fn miss_then_raw_hit(conn: &mut relate::Connection, statement: &str) -> relate::QueryResult {
    let first = common::run(conn, statement);
    assert_eq!(
        first.plan_cache(),
        PlanCacheOutcome::Miss,
        "{statement}: the first execution compiles"
    );
    let second = common::run(conn, statement);
    assert_eq!(
        second.plan_cache(),
        PlanCacheOutcome::HitRawFastPath,
        "{statement}: the identical repeat takes the raw fast path"
    );
    assert_eq!(
        second.rows, first.rows,
        "{statement}: same answer from the hit"
    );
    first
}

/// The transition every literal-subscript statement must make:
/// miss → raw fast path (the insert records the raw text beside the
/// entry), with stable rows.
#[test]
fn a_literal_subscript_statement_participates_in_the_plan_cache() {
    for statement in [
        "MATCH (n:Thing) RETURN labels(n)[0] AS kind",
        "MATCH (n:Thing) RETURN n.tags[0] AS tag",
        "RETURN [10, 20, 30][1] AS middle",
    ] {
        let mut conn = common::connection();
        common::run(&mut conn, "CREATE (:Thing {tags: ['a', 'b']})");
        common::run(&mut conn, "CREATE (:Thing {tags: ['c']})");

        miss_then_raw_hit(&mut conn, statement);
    }
}

/// The anonymous relationship pattern's empty `[]` is a bracket the
/// list-literal collapse must not swallow (the pattern head before it is
/// never an expression): a statement carrying both an anonymous rel and
/// an ordinary literal participates in the cache like any other — before
/// the pattern-head exclusion it never cached at all, its `[]` collapsing
/// to a bare placeholder the rewritten parse rejects.
#[test]
fn an_anonymous_relationship_statement_participates_in_the_plan_cache() {
    let mut conn = common::connection();
    common::run(&mut conn, "CREATE (:A {x: 1})-[:KNOWS]->(:B {x: 2})");
    miss_then_raw_hit(&mut conn, "MATCH (a)-[]->(b) WHERE a.x = 1 RETURN b.x AS x");
}

/// The value the subscript extracted binds as its own parameter: a
/// different literal index is the same template (one plan) with different
/// values, exactly like any other auto-parameterized family.
#[test]
fn a_changed_subscript_literal_shares_the_template() {
    let mut conn = common::connection();
    common::run(&mut conn, "CREATE (:List {xs: [10, 20, 30]})");
    let first = common::run(&mut conn, "MATCH (l:List) RETURN l.xs[0] AS at");
    let second = common::run(&mut conn, "MATCH (l:List) RETURN l.xs[2] AS at");
    assert_eq!(first.plan_cache(), PlanCacheOutcome::Miss);
    assert_eq!(
        second.plan_cache(),
        PlanCacheOutcome::HitTemplate,
        "the changed literal is the same template, not a new plan"
    );
    assert_eq!(first.rows.len(), 1, "one row");
    assert_eq!(second.rows.len(), 1, "one row");
    assert_eq!(first.rows[0][0], relate::Value::Int(10));
    assert_eq!(second.rows[0][0], relate::Value::Int(30));
}

/// A slice's bounds keep their own guards (the `..`-following bound never
/// extracts), and the slice statement participates in the cache all the
/// same.
#[test]
fn a_literal_slice_statement_participates_in_the_plan_cache() {
    let mut conn = common::connection();
    common::run(&mut conn, "CREATE (:List {xs: [10, 20, 30]})");
    let first = miss_then_raw_hit(&mut conn, "MATCH (l:List) RETURN l.xs[0..2] AS head");
    assert_eq!(first.rows.len(), 1, "one row");
    assert_eq!(first.rows[0][0], common::ints(&[10, 20]));
}
