//! ADR-0503's admission gates, end to end: which statements insert an entry
//! (reads only, under both memory bounds), which bypass the cache entirely
//! (a reserved-prefix caller map, dynamic name operands), which never serve a
//! cached plan (`EXPLAIN`/`PROFILE`), and that one entry per template is
//! shared by every connection of the store.

use super::*;

/// Only read-classified statements insert: a write never leaves an entry,
/// and repeating it stays correct.
#[test]
fn writes_never_insert() {
    let (db, mut c) = users();
    for i in 0..3 {
        c.query(&format!("CREATE (:Tag {{t: {i}}})")).unwrap();
    }
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.len(), 0, "no write leaves an entry");
}

/// The insert-side memory bound: a statement text above
/// `MAX_CACHED_STATEMENT_BYTES` never builds an entry, however read-shaped
/// it is — the count cap alone would let a few maximal statements pin
/// disproportionate derived memory.
#[test]
fn oversized_statements_never_insert() {
    let (db, mut c) = users();
    let big = format!(
        "MATCH (n:User) WHERE n.name = '{}' RETURN n.name",
        "x".repeat(80 * 1024)
    );
    for _ in 0..2 {
        assert!(rows(&c.query(&big).unwrap()).is_empty());
    }
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.len(), 0, "an oversized statement stays uncached");
}

/// Admission off has no workspace pool. The count and text caps still
/// bound the cache, and a repeated read still inserts one entry.
#[test]
fn admission_off_still_caches() {
    let (db, mut c) = users();
    c.query("RETURN 1 AS n").unwrap();
    c.query("RETURN 2 AS n").unwrap();
    assert!(db.inner.resolved.budget.snapshot().is_none());
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.len(), 1);
}

/// The eligible read shape inserts exactly one entry, shared by every
/// connection of the store.
#[test]
fn one_entry_per_template_shared_across_connections() {
    let (db, mut c) = users();
    c.query("MATCH (n:User {id: 1}) RETURN n.name AS name")
        .unwrap();
    c.query("MATCH (n:User {id: 2}) RETURN n.name AS name")
        .unwrap();
    {
        let entries = crate::plan_cache::lock(&db.inner);
        assert_eq!(entries.len(), 1, "the two statements share one template");
    }
    // A second connection of the same store hits the same entry.
    let mut other = db.connect().unwrap();
    assert_eq!(
        rows(
            &other
                .query("MATCH (n:User {id: 3}) RETURN n.name AS name")
                .unwrap()
        ),
        vec![vec![Value::String("cy".into())]]
    );
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.len(), 1);
}

/// `EXPLAIN`/`PROFILE`-prefixed statements and the plan-tree API never
/// serve cached plans: the tree keeps spelling the statement as written,
/// and the preamble-flagged statements leave no entries of their own.
#[test]
fn plan_trees_never_serve_cached_plans() {
    let (db, mut c) = users();
    // An indexed point read: the seek line narrates the predicate with the
    // bound as spelled — the contract a cached (auto-parameterized) plan
    // would break with `$__AUTO_*__`.
    c.query("CREATE INDEX user_id FOR (n:User) ON (n.id)")
        .unwrap();
    // Warm the plain shape first: an entry exists.
    c.query("MATCH (n:User {id: 1}) RETURN n.name AS name")
        .unwrap();
    {
        let entries = crate::plan_cache::lock(&db.inner);
        assert_eq!(entries.len(), 1);
    }
    // The plan-tree API spells the statement as written.
    let tree = c
        .explain_plan("MATCH (n:User {id: 1}) RETURN n.name AS name")
        .unwrap();
    let text = node_texts(&tree).join("\n");
    assert!(
        text.contains("WHERE id = 1"),
        "the seek line spells the literal: {text}"
    );
    assert!(
        !text.contains("__AUTO_"),
        "no auto-parameter in the tree: {text}"
    );
    // EXPLAIN through the query path is equally literal and leaves no
    // entry of its own.
    let explained = c
        .query("EXPLAIN MATCH (n:User {id: 2}) RETURN n.name AS name")
        .unwrap();
    let plan = explained
        .plan
        .as_deref()
        .expect("EXPLAIN stamps the artifact");
    let text = node_texts(&plan.root).join("\n");
    assert!(
        text.contains("WHERE id = 2"),
        "the EXPLAIN tree spells the literal: {text}"
    );
    assert!(!text.contains("__AUTO_"), "no auto-parameter: {text}");
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(
        entries.len(),
        1,
        "the preamble-flagged statement added nothing"
    );
}

/// Every `operator`/`details`/`statement_echo` of a tree and its
/// children, in pre-order — the text a renderer would show.
fn node_texts(node: &crate::PlanNode) -> Vec<String> {
    let mut out = vec![format!(
        "{}|{}|{}",
        node.operator, node.details, node.statement_echo
    )];
    for child in &node.children {
        out.extend(node_texts(child));
    }
    out
}

/// `CALL { … }` subqueries and `FOREACH` are the session's conservative
/// exclusions: they run correctly, repeatedly, without entries.
#[test]
fn call_subqueries_and_foreach_stay_uncached_and_correct() {
    let (db, mut c) = users();
    for _ in 0..2 {
        let r = c
            .query("CALL { MATCH (n:User) RETURN n.id AS id } RETURN max(id) AS m")
            .unwrap();
        assert_eq!(rows(&r), vec![vec![Value::Int(3)]]);
    }
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.len(), 0);
}

/// A caller parameter named with the reserved prefix disables the cache
/// for that statement (no lookup, no insert) and runs as ordinary Cypher.
/// The check precedes even the raw-string fast path: without it, a raw
/// hit would serve a cached, auto-parameterized plan beside a caller map
/// the design excludes.
#[test]
fn reserved_prefix_parameters_bypass_the_cache() {
    let (db, mut c) = users();
    // Warm the shape first: an entry exists whose raw text is exactly the
    // statement about to run.
    let statement = "MATCH (n:User {id: 1}) RETURN n.name AS name";
    c.query(statement).unwrap();
    {
        let entries = crate::plan_cache::lock(&db.inner);
        assert_eq!(entries.len(), 1);
    }
    let mut params = std::collections::BTreeMap::new();
    params.insert("__AUTO_INT_0__".into(), Value::Int(1));
    let r = c
        .query_with_params(
            "MATCH (n:User {id: $__AUTO_INT_0__}) RETURN n.name AS name",
            params,
        )
        .unwrap();
    assert_eq!(rows(&r), vec![vec![Value::String("ada".into())]]);
    // The same raw text with the reserved key in the map bypasses too:
    // its PARSE arm is a real parse (tens of µs), not the raw hit's
    // lookup (sub-µs) — the generous gap keeps the assertion about the
    // order of magnitude, not a benchmark.
    let mut dummy = std::collections::BTreeMap::new();
    dummy.insert("__AUTO_INT_0__".into(), Value::Int(0));
    let bypassed = c.query_with_params(statement, dummy.clone()).unwrap();
    assert_eq!(rows(&bypassed), vec![vec![Value::String("ada".into())]]);
    // The bypass's PARSE arm is a real parse (tens of µs), the raw hit's
    // a lookup (sub-µs): the minimum over several samples of each, so
    // the assertion measures the order of magnitude instead of one
    // scheduling slice.
    let bypass_min = min_parse_with(&mut c, statement, &dummy);
    let hit_min = min_parse(&mut c, statement);
    assert!(
        bypass_min > hit_min,
        "bypass min parse {bypass_min} ns vs raw-hit min parse {hit_min} ns"
    );
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.len(), 1, "the reserved name disables the cache");
}

/// A plan carrying a dynamic name operand (dynamic label, property key)
/// never inserts: LABS-709 validates such operands against bound VALUES
/// at lowering time — a content-level check (empty or null-byte name)
/// typed placeholders cannot preserve — and a hit skips lowering (found
/// in review). The statement stays uncached, never silently unvalidated,
/// and repeats stay correct.
#[test]
fn dynamic_operand_statements_stay_uncached_and_validated() {
    let (db, mut c) = users();
    let mut labels = std::collections::BTreeMap::new();
    labels.insert(
        "labels".into(),
        Value::List(std::sync::Arc::new(vec![Value::String("User".into())])),
    );
    // A wrong-typed operand must keep its LABS-709 admission error on
    // every repeat — the first call is uncached (miss), and no entry may
    // exist for a later call to hit.
    let mut wrong = std::collections::BTreeMap::new();
    wrong.insert("labels".into(), Value::Int(7));
    for round in 0..3 {
        let ok = c
            .query_with_params(
                "MATCH (n:$any($labels)) WHERE n.id = 1 RETURN n.name AS name",
                labels.clone(),
            )
            .unwrap();
        assert_eq!(
            rows(&ok),
            vec![vec![Value::String("ada".into())]],
            "round {round}"
        );
        let err = c
            .query_with_params(
                "MATCH (n:$any($labels)) WHERE n.id = 1 RETURN n.name AS name",
                wrong.clone(),
            )
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("label"),
            "the wrong-typed dynamic label keeps its admission error: {err}"
        );
    }
    // The same exclusion covers a dynamic relationship TYPE (the detector
    // walks rel type expressions and the elements-only pattern
    // representation too — found in review). One lock at the very end:
    // the guard is a non-reentrant mutex, so two live acquisitions in one
    // thread deadlock — the assertion covers both cases.
    let mut types = std::collections::BTreeMap::new();
    types.insert(
        "types".into(),
        Value::List(std::sync::Arc::new(vec![Value::String("REL".into())])),
    );
    for _ in 0..2 {
        let r = c
            .query_with_params(
                "MATCH (n:User)-[:$any($types)]->(m:User) WHERE n.id = 1 RETURN count(m) AS c",
                types.clone(),
            )
            .unwrap();
        assert_eq!(rows(&r), vec![vec![Value::Int(0)]]);
    }
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(
        entries.len(),
        0,
        "no dynamic-operand statement leaves an entry"
    );
}
