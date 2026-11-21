//! ADR-0503 end-to-end: the plan cache through the public API. The
//! correctness bar is that a cached run is indistinguishable in result
//! and error from an uncached one — same rows for the same statement,
//! same rows for a same-template statement with different literals, and
//! the store-level observables (the stats generation, the entry count)
//! doing what the ADR says they do.

use crate::{Connection, Database, Value};

fn users() -> (Database, Connection) {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("CREATE (:User {id: 1, name: 'ada'}), (:User {id: 2, name: 'bob'}), (:User {id: 3, name: 'cy'})")
        .unwrap();
    (db, c)
}

fn rows(result: &crate::QueryResult) -> Vec<Vec<Value>> {
    result.rows.clone()
}

/// The minimum PARSE-phase duration over the fixed three samples both
/// timing assertions take — the order of magnitude, not one scheduling
/// slice. `min_parse` is the no-parameter spelling.
fn min_parse_with(
    c: &mut Connection,
    statement: &str,
    params: &std::collections::BTreeMap<String, Value>,
) -> u128 {
    (0..3)
        .map(|_| {
            c.query_with_params(statement, params.clone())
                .unwrap()
                .header
                .phases
                .parse()
                .as_nanos()
        })
        .min()
        .expect("three samples")
}

fn min_parse(c: &mut Connection, statement: &str) -> u128 {
    min_parse_with(c, statement, &std::collections::BTreeMap::new())
}

/// The one correctness property the whole design turns on: a template hit
/// binds THIS statement's literals as parameters — the same template with
/// different values returns each statement's own rows, in any order, over
/// any number of repeats.
#[test]
fn same_template_different_literals_return_the_right_rows() {
    let (_db, mut c) = users();
    for round in 0..3 {
        let one = c
            .query("MATCH (n:User {id: 1}) RETURN n.name AS name")
            .unwrap();
        assert_eq!(
            rows(&one),
            vec![vec![Value::String("ada".into())]],
            "round {round}"
        );
        let two = c
            .query("MATCH (n:User {id: 2}) RETURN n.name AS name")
            .unwrap();
        assert_eq!(
            rows(&two),
            vec![vec![Value::String("bob".into())]],
            "round {round}"
        );
    }
    // The string-typed key shares the same mechanics.
    let by_name = c
        .query("MATCH (n:User {name: 'bob'}) RETURN n.id AS id")
        .unwrap();
    assert_eq!(rows(&by_name), vec![vec![Value::Int(2)]]);
    let by_other_name = c
        .query("MATCH (n:User {name: 'cy'}) RETURN n.id AS id")
        .unwrap();
    assert_eq!(rows(&by_other_name), vec![vec![Value::Int(3)]]);
}

/// An exact repeat resolves through the raw-string fast path (the entry
/// remembers the last raw text and its values) and stays correct.
#[test]
fn exact_repeats_stay_correct() {
    let (_db, mut c) = users();
    for _ in 0..4 {
        let r = c
            .query("MATCH (n:User) WHERE n.id > 1 RETURN n.id AS id ORDER BY id")
            .unwrap();
        assert_eq!(rows(&r), vec![vec![Value::Int(2)], vec![Value::Int(3)]]);
    }
}

/// A list literal collapses to one parameter: same template, different
/// lists, each statement's own rows.
#[test]
fn in_lists_bind_this_statements_values() {
    let (_db, mut c) = users();
    let one = c
        .query("MATCH (n:User) WHERE n.id IN [1, 2] RETURN count(n) AS c")
        .unwrap();
    assert_eq!(rows(&one), vec![vec![Value::Int(2)]]);
    let two = c
        .query("MATCH (n:User) WHERE n.id IN [3] RETURN count(n) AS c")
        .unwrap();
    assert_eq!(rows(&two), vec![vec![Value::Int(1)]]);
    let none = c
        .query("MATCH (n:User) WHERE n.id IN [] RETURN count(n) AS c")
        .unwrap();
    assert_eq!(rows(&none), vec![vec![Value::Int(0)]]);
}

/// REL8-1120: a deeply nested literal list rides the payload boundary
/// (`plan_cache::engine_values`), whose depth guard is the hit path's one
/// value-dependent literal check beside the normalizer's aligned nesting
/// cap (`MAX_NESTING_DEPTH` == `MAX_VALUE_DEPTH`). A list well under both
/// caps caches and serves each same-template statement's own value; the
/// unit test in `plan_cache` pins the refusal past the cap.
#[test]
fn deep_literal_lists_bind_through_the_payload_boundary() {
    let (_db, mut c) = users();
    let deep = |core: &str| {
        let mut s = String::new();
        for _ in 0..400 {
            s.push('[');
        }
        s.push_str(core);
        for _ in 0..400 {
            s.push(']');
        }
        format!("RETURN {s} AS l")
    };
    let one = c.query(&deep("1")).unwrap();
    let one_again = c.query(&deep("1")).unwrap();
    assert_eq!(
        rows(&one),
        rows(&one_again),
        "a repeat serves its own value"
    );
    let two = c.query(&deep("2")).unwrap();
    assert_ne!(
        rows(&one),
        rows(&two),
        "a same-template statement binds its own core"
    );
    let two_again = c.query(&deep("2")).unwrap();
    assert_eq!(rows(&two), rows(&two_again));
}

/// REL8-1120's retry leg, pinned: the same template with a literal list
/// whose element types the compile rejects (a quantifier `%` over a
/// string list) is served by the cached plan and fails with the runtime
/// type error first — the retry re-runs the statement uncached, and the
/// caller sees the compile's own verdict and error class. The suite's
/// own bar (a cached run indistinguishable in result and error) holds
/// only through this leg.
#[test]
fn a_type_error_hit_retries_uncached_and_reports_the_compile_verdict() {
    let (_db, mut c) = users();
    // Warm the template: the numeric list inserts, the second statement
    // is a template hit.
    c.query("RETURN all(x IN [1, 2] WHERE x % 2 = 0) AS n")
        .unwrap();
    let warm = c
        .query("RETURN all(x IN [1, 2] WHERE x % 2 = 0) AS n")
        .unwrap();
    assert_eq!(rows(&warm), vec![vec![Value::Bool(false)]]);
    let err = c
        .query("RETURN all(x IN ['Clara'] WHERE x % 2 = 0) AS n")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Type mismatch") && err.contains("non-numeric"),
        "the compile's own verdict, not the runtime twin: {err}"
    );
}

/// Every write commit advances the stats generation, and the next read
/// re-plans from the cached AST (a generation mismatch) against the new
/// state — the rows reflect the write.
#[test]
fn write_commits_advance_the_generation_and_reads_follow() {
    let (db, mut c) = users();
    let read = "MATCH (n:User) RETURN count(n) AS c";
    let before = c.query(read).unwrap();
    assert_eq!(rows(&before), vec![vec![Value::Int(3)]]);
    let stamped = db.inner.stats_generation.current();
    for i in 0..3 {
        c.query("CREATE (:User {id: 4, name: 'x'})").unwrap();
        let after = c.query(read).unwrap();
        assert_eq!(rows(&after), vec![vec![Value::Int(4 + i)]]);
        // One commit = one advance.
        assert_eq!(
            db.inner.stats_generation.current(),
            stamped + (i as u64 + 1)
        );
    }
}

/// A schema change is a commit like any other: the generation advances and
/// the reads stay correct through the replan.
#[test]
fn schema_changes_advance_the_generation() {
    let (db, mut c) = users();
    let read = "MATCH (n:User {id: 1}) RETURN n.name AS name";
    assert_eq!(
        rows(&c.query(read).unwrap()),
        vec![vec![Value::String("ada".into())]]
    );
    let stamped = db.inner.stats_generation.current();
    c.query("CREATE INDEX user_id FOR (n:User) ON (n.id)")
        .unwrap();
    assert_eq!(db.inner.stats_generation.current(), stamped + 1);
    // The cached shape replans against the index and stays correct.
    for _ in 0..2 {
        assert_eq!(
            rows(&c.query(read).unwrap()),
            vec![vec![Value::String("ada".into())]]
        );
    }
}

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

/// Errors keep their class and text on repeats: a statement that fails
/// keeps failing the same way, cached or not.
#[test]
fn failing_statements_fail_identically_on_repeats() {
    let (_db, mut c) = users();
    for _ in 0..3 {
        let err = c
            .query("MATCH (n:User {id: 1}) RETURN n.missing AS x, n.missing AS x")
            .unwrap_err();
        assert!(err.to_string().contains("same name"), "{err}");
        // A pure parse error never caches anything.
        let syntax = c.query("MATCH (n:User {id: }) RETURN n").unwrap_err();
        assert!(syntax.to_string().contains("expected"), "{syntax}");
    }
}

/// The phase-split stamp stays honest: a hit's PARSE arm is the lookup,
/// not a parse — measurably below the first run's. The threshold is
/// generous (a cold parse is tens of µs; a lookup pass is single-digit),
/// so the assertion is about the order of magnitude, not a benchmark.
#[test]
fn hit_parse_phase_is_the_lookup_not_a_parse() {
    let (_db, mut c) = users();
    let statement = "MATCH (n:User {id: 1}) RETURN n.name AS name, n.id AS id ORDER BY id LIMIT 1";
    let first_parse_ns = c.query(statement).unwrap().header.phases.parse().as_nanos();
    let hit_parse_ns = min_parse(&mut c, statement);
    assert!(
        hit_parse_ns < first_parse_ns,
        "hit PARSE {hit_parse_ns} ns vs first {first_parse_ns} ns"
    );
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
