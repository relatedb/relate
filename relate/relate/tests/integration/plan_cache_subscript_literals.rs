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

/// The authored-spelling substitution (ADR-0718): every cache path names
/// an auto-parameterized result column by the authored spelling — the
/// first (parameterized-compile) execution, the template hit with a
/// different literal, and the raw (stage-1) hit of the identical text.
/// No path re-parses the statement: the spellings ride the
/// normalization's own channels.
#[test]
fn every_cache_path_answers_the_authored_column_name() {
    let mut conn = common::connection();
    // First execution: the parameterized compile from the rewritten text
    // — the plan's column carries the placeholder, the substitution
    // restores the authored text.
    let first = common::run(&mut conn, "RETURN 41");
    assert_eq!(first.header.columns, vec!["41".to_string()]);
    // Template hit: a different literal shares the plan, the CURRENT
    // statement's normalization supplies the spelling.
    let second = common::run(&mut conn, "RETURN 7");
    assert_eq!(second.header.columns, vec!["7".to_string()]);
    // Raw hit: the identical text resolves through stage 1, whose entry
    // recorded the spellings once at note_raw — no normalization runs.
    let third = common::run(&mut conn, "RETURN 7");
    assert_eq!(third.header.columns, vec!["7".to_string()]);
    // An expression keeps its operators and its spacing on the FIRST
    // execution (the parameterized compile of this statement's own
    // rewritten text).
    let mut fresh = common::connection();
    let sum = common::run(&mut fresh, "RETURN 1 + 2 AS total");
    assert_eq!(sum.header.columns, vec!["total".to_string()]);
    let unaliased = common::run(&mut fresh, "RETURN 1 + 2");
    assert_eq!(unaliased.header.columns, vec!["1 + 2".to_string()]);
    // A multi-space original's spacing survives on its own first
    // execution: the splice preserves the span's trailing whitespace, so
    // the plan's column text carries it and the substitution reproduces
    // the source exactly.
    let mut spaced_conn = common::connection();
    let spaced = common::run(&mut spaced_conn, "RETURN 1  +  2");
    assert_eq!(spaced.header.columns, vec!["1  +  2".to_string()]);
    // The recorded divergence (ADR-0718): a template hit's plan was
    // compiled from the FIRST statement's rewritten text, so the
    // column's between-token spacing follows the plan, not the current
    // statement — the current statement contributes its literals (the
    // spellings), the plan its shape. `1+2` hitting the template of
    // `1 + 2` answers the plan's spacing.
    let glued = common::run(&mut fresh, "RETURN 1+2");
    assert_eq!(glued.header.columns, vec!["1 + 2".to_string()]);
}

/// The substitution's safety rules survive the full path (the unit pins
/// are the parser's; these run the engine): a non-extracted string
/// literal's placeholder-lookalike content stays verbatim in an
/// auto-parameterized sibling's column, and a carried name inside a
/// string literal is never substituted from within its quotes.
#[test]
fn lookalike_string_content_survives_the_authored_substitution() {
    let mut conn = common::connection();
    common::run(&mut conn, "CREATE (:N {x: 'a'})");
    // The string is a call argument (not extracted, the call-argument
    // guard), so its `$__AUTO_INT_9__` lookalike is not a carried name:
    // verbatim, while the extracted `1` outside the call substitutes.
    let r = common::run(
        &mut conn,
        "MATCH (n:N) RETURN coalesce(n.x, '$__AUTO_INT_9__') + 1",
    );
    assert_eq!(
        r.header.columns,
        vec!["coalesce(n.x, '$__AUTO_INT_9__') + 1".to_string()]
    );
}
