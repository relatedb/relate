//! End-to-end pins for a parenthesized alias operand in a WHERE:
//! `NOT(b)` and `(b)` resolve an alias a WITH introduced (REL8-1867).
//! The front's WHERE constraint walk used to read the clause's
//! pre-projection scope — a same-clause alias was invisible — and read
//! every lone parenthesized variable as a single-node pattern, so the
//! reference failed with `UndefinedVariable`. For a value variable the
//! parens are grouping; the single-node pattern rejection stays for
//! entity variables (nodes, relationships, paths — and their WITH
//! aliases: the walk's scope carrier projects names and kinds through
//! one derivation). The carrier also pins the walk's other kind rules:
//! the static-kind family uniform across expression positions, and
//! subquery bodies inheriting the classification of the names they see.

mod common;

use common::{connection, err_text, rows};
use relate::Value;

/// The issue's query executes with Cypher's NULL comparison semantics:
/// `u.p1 <> null` is NULL for both fixture rows, `NOT(NULL)` is NULL,
/// and the WHERE filters everything — live 2026.10 plans and returns
/// zero rows. The `EXPLAIN` spelling plans.
#[test]
fn not_parenthesized_alias_where_filters_null_comparisons() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'}), (:L1 {p2: 'b'})").unwrap();
    assert_eq!(
        rows(
            &mut c,
            "MATCH (u:L1) WITH u, u.p1 <> null AS b WHERE NOT(b) RETURN u"
        ),
        Vec::<Vec<Value>>::new()
    );
    let explained = c
        .query("EXPLAIN MATCH (u:L1) WITH u, u.p1 <> null AS b WHERE NOT(b) RETURN u")
        .unwrap();
    assert!(explained.plan_root().is_some(), "EXPLAIN stamps a plan");
}

/// The same name resolution with a visible row: `IS NOT NULL` gives `b`
/// a real boolean and `NOT(b)` keeps the node without `p1` (live 2026.10:
/// one row, `v = null`); every spelling of the operand — the spaced
/// `NOT (b)`, the bare group `(b)` (the predicate `b` itself, keeping
/// the node with `p1`) — agrees with the unparenthesized `NOT b`. An
/// alias an earlier WITH introduced resolves through a MATCH's WHERE the
/// same way, as does a scalar an `UNWIND` binds.
#[test]
fn not_parenthesized_alias_where_returns_the_negated_rows() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'}), (:L1 {p2: 'b'})").unwrap();
    for query in [
        "MATCH (u:L1) WITH u, u.p1 IS NOT NULL AS b WHERE NOT(b) RETURN u.p1 AS v",
        "MATCH (u:L1) WITH u, u.p1 IS NOT NULL AS b WHERE NOT b RETURN u.p1 AS v",
        "MATCH (u:L1) WITH u, u.p1 IS NOT NULL AS b WHERE NOT (b) RETURN u.p1 AS v",
        "MATCH (u:L1) WITH u, u.p1 IS NOT NULL AS b MATCH (u:L1) WHERE NOT(b) \
         RETURN u.p1 AS v",
    ] {
        assert_eq!(rows(&mut c, query), vec![vec![Value::Null]], "{query}");
    }
    // The bare group is the predicate `b` itself: the node carrying p1.
    assert_eq!(
        rows(
            &mut c,
            "MATCH (u:L1) WITH u, u.p1 IS NOT NULL AS b WHERE (b) RETURN u.p1 AS v"
        ),
        vec![vec![Value::String("a".into())]]
    );
    // A scalar introducer beyond WITH: an UNWIND alias under NOT(b)
    // keeps the b = false pairs (both nodes once).
    assert_eq!(
        rows(
            &mut c,
            "UNWIND [true, false] AS b MATCH (n:L1) WHERE NOT(b) RETURN count(*)"
        ),
        vec![vec![Value::Int(2)]]
    );
}

/// The entity reading is unchanged and complete: a lone parenthesized
/// node, path or relationship variable in a WHERE keeps the single-node
/// pattern rejection — the clause's own pattern variable, a node renamed
/// by the projection, and the node, relationship or path alias the same
/// WITH carries (path-ness projects through the same commit). One rule,
/// one diagnostic, in the front end — no entity kind falls through to a
/// later layer's boolean coercion.
#[test]
fn parenthesized_entity_variables_stay_rejected_as_predicates() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'})").unwrap();
    for query in [
        "MATCH (n:L1) WHERE (n) RETURN n",
        "MATCH (n:L1) WITH n AS x WHERE (x) RETURN x",
        "MATCH (n:L1) WITH n AS x WHERE (n) RETURN x",
        "MATCH p=(a:L1)-[r]->(b:L1) WHERE (p) RETURN p",
        "MATCH p=(a:L1)-[r]->(b:L1) WITH p AS q WHERE (q) RETURN q",
        "MATCH (a:L1)-[r]->(b:L1) WHERE (r) RETURN a",
        "MATCH (a:L1)-[r]->(b:L1) WHERE NOT(r) RETURN a",
        "MATCH (a:L1)-[r]->(b:L1) WITH r AS q WHERE (q) RETURN q",
    ] {
        let error = c.query(query);
        assert!(
            err_text(error)
                .contains("InvalidArgumentType: a single node is not a valid pattern predicate"),
            "{query}: the single-node rejection stays"
        );
    }
}

/// The WITH commit projects path-ness with the name scope: `labels()` on
/// a path alias the projection carries is the front end's compile-time
/// rejection, and a path the projection drops reports as the undefined
/// name it is — not as a stale path fact from before the projection.
#[test]
fn with_projects_path_ness_with_the_scope() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'})").unwrap();
    let error = c.query("MATCH p=(a:L1) WITH p AS q RETURN labels(q)");
    assert!(
        err_text(error).contains("labels() requires a node, got PATH"),
        "a carried path alias is statically a path"
    );
    let error = c.query("MATCH p=(a:L1) WITH 1 AS x RETURN labels(p)");
    assert!(
        err_text(error).contains("UndefinedVariable: variable 'p' not defined"),
        "a dropped path is an undefined name, not a stale path fact"
    );
}

/// A pattern predicate in a WITH's WHERE references the same projection's
/// aliases exactly as a parenthesized alias does: `WITH n AS a … WHERE
/// (a)-[:R]->(n)` plans and runs (zero rows without the relationship).
#[test]
fn a_with_where_pattern_predicate_reads_the_clauses_own_aliases() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'}), (:L2 {p1: 'b'})").unwrap();
    assert_eq!(
        rows(
            &mut c,
            "MATCH (n:L1) WITH n AS a WHERE (a)-[:R]->(n) RETURN a.p1"
        ),
        Vec::<Vec<Value>>::new()
    );
}

/// A quantified path pattern's subpath variable binds a name — a list of
/// the sub-paths its group matched, a value with no kind: the name is
/// visible to the clause's later checks and stages (the count and the
/// later WITH's grouping operand resolve it), end to end.
#[test]
fn a_quantified_subpath_variable_binds_its_name() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'}), (:L1 {p2: 'b'})").unwrap();
    assert_eq!(
        rows(
            &mut c,
            "MATCH (x)(p2 = (a:L1)-->(b:L1))*(y) RETURN count(p2) AS c"
        ),
        vec![vec![Value::Int(2)]]
    );
    // The lone-parenthesized form is the value grouping — never the
    // single-node rejection, never an undefined name: p2 binds a list,
    // and the predicate reads its truth per row (an empty list filters).
    assert_eq!(
        rows(
            &mut c,
            "MATCH (x)(p2 = (a:L1)-->(b:L1))*(y) WITH 1 AS z WHERE (p2) RETURN z"
        ),
        Vec::<Vec<Value>>::new()
    );
}

/// The walk's kind rules are uniform and inherited: the static-kind
/// family rejects the same statically-certain misuses at every
/// expression position (a path's `labels()` in a MATCH's WHERE and a
/// path's property access at an item and in a WITH's WHERE — positions
/// that previously left them to the executor), and a subquery body
/// inherits the enclosing statement's classification for the names it
/// sees — an outer node keeps the single-node `(n)` reading and the
/// `type(n)` rejection inside the body, an unimported name stays the
/// binder's undefined-name report, and the classification flows across a
/// CALL body's NEXT stages with the name scope.
#[test]
fn kind_rules_are_uniform_and_inherited_by_subquery_bodies() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'}), (:L1 {p2: 'b'})").unwrap();
    // The static-kind family, uniform across positions.
    let error = c.query("MATCH p=(a:L1)-[r]->(b:L1) WHERE labels(p) = [] RETURN p");
    assert!(
        err_text(error).contains("labels() requires a node, got PATH"),
        "labels() on a path is rejected in a WHERE"
    );
    for query in [
        "MATCH p=(a:L1)-[r]->(b:L1) RETURN p.foo",
        "MATCH p=(a:L1)-[r]->(b:L1) WITH p WHERE p.x = 1 RETURN p",
    ] {
        let error = c.query(query);
        assert!(
            err_text(error).contains("property access is not allowed on paths"),
            "{query}: property access on a path is rejected at this position"
        );
    }
    // The legal shape stays legal and answering.
    assert_eq!(
        rows(
            &mut c,
            "MATCH (n:L1) WHERE labels(n) = ['L1'] RETURN count(*) AS c"
        ),
        vec![vec![Value::Int(2)]]
    );
    // A subquery body inherits the visible classification.
    let error =
        c.query("MATCH (n:L1) WHERE EXISTS { MATCH (m) WHERE (n) RETURN count(m) AS c } RETURN n");
    assert!(
        err_text(error)
            .contains("InvalidArgumentType: a single node is not a valid pattern predicate"),
        "an outer node keeps the single-node reading inside a body"
    );
    let error = c.query("MATCH (n:L1) CALL (n) { RETURN type(n) AS t } RETURN 1 AS ok");
    assert!(
        err_text(error).contains("type() can only be applied to relationships, not nodes"),
        "a scope-clause import is classified inside the body"
    );
    let error = c.query("MATCH (n:L1), (y:L1) CALL (n) { RETURN type(y) AS t } RETURN 1 AS ok");
    assert!(
        err_text(error).contains("UndefinedVariable: variable 'y' not defined"),
        "an unimported name stays the binder's undefined-name report"
    );
    // The classification flows across NEXT stages with the name scope.
    let error = c.query(
        "MATCH (n:L1) CALL (n) { MATCH (a:L1) WITH a AS m RETURN m NEXT MATCH (b) WHERE (m) \
         RETURN b } RETURN 1 AS ok",
    );
    assert!(
        err_text(error)
            .contains("InvalidArgumentType: a single node is not a valid pattern predicate"),
        "a NEXT stage reads the kinds earlier stages established"
    );
}

/// Round the carrier's view semantics: the WHERE view overlays the
/// projection — a same-clause re-projection of a node name to a per-row
/// expression leaves the name a value, so its lone-parenthesized form is
/// the boolean coercion (not the stale single-node rejection) — and a
/// subquery body nested in a WITH's WHERE sees the clause's expression
/// view, same-clause aliases included: an EXISTS body referencing the
/// alias the same WITH introduced runs (it was the front end's
/// `UndefinedVariable` before the view was unified).
#[test]
fn the_where_view_overlays_and_the_bodies_see_it() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 'a'}), (:L1 {p2: 'b'})").unwrap();
    // Overlay: the re-bound name is a value; its parens are grouping.
    for query in [
        "MATCH (n:L1) WITH n AS x WITH x.p1 AS x WHERE (x) RETURN x",
        "MATCH (n:L1) WITH n AS x WITH 1 AS x WHERE (x) RETURN x",
    ] {
        let error = c.query(query);
        let error = err_text(error);
        assert!(
            error.contains("requires a boolean"),
            "{query}: the re-bound name is a value: {error}"
        );
        assert!(
            !error.contains("single node"),
            "{query}: no stale single-node rejection: {error}"
        );
    }
    // The unrebound alias keeps the entity reading.
    let error = c.query("MATCH (n:L1) WITH n AS x WHERE (x) RETURN x");
    assert!(
        err_text(error)
            .contains("InvalidArgumentType: a single node is not a valid pattern predicate"),
        "an unrebound node alias keeps the single-node rejection"
    );
    // The nested body sees what the clause's WHERE sees — and an item's
    // body sees only the input: a sibling item's alias is not defined at
    // the item's position (the front's own report), while the WHERE's
    // body resolves it.
    let error = c.query(
        "MATCH (n:L1) WITH n AS m, EXISTS { MATCH (x:L1) WHERE m.p1 = x.p1 \
         RETURN count(*) } AS e RETURN e",
    );
    assert!(
        err_text(error).contains("UndefinedVariable: variable 'm' not defined"),
        "an item's body sees the clause's input, not its sibling items' aliases"
    );
    let got = rows(
        &mut c,
        "MATCH (n:L1) WITH n AS m WHERE EXISTS { MATCH (x:L1) WHERE m.p1 = x.p1 \
         RETURN count(*) } RETURN m.p1 AS v",
    );
    assert_eq!(got.len(), 2, "both rows resolve and run: {got:?}");
    assert!(
        got.contains(&vec![Value::String("a".into())]) && got.contains(&vec![Value::Null]),
        "the body read the same-clause alias: {got:?}"
    );
}
