//! MATCH-shape validation (V1 harness wave): the CIP-73 path-selector,
//! REPEATABLE ELEMENTS, quantified-path-pattern, LIMIT/SKIP, subquery-body,
//! CREATE self-reference and CIP-60 match-mode-position shapes Neo4j
//! 2026 rejects at compile time.
//!
//! One test per newly rejected shape, beside the other engine-level
//! validation suites. Each test also pins the allowed neighbouring form so
//! the rule cannot widen silently.

mod common;

use common::{connection, err_text, run, scalar_one};
use relate::Value;

fn assert_rejected(conn: &mut relate::Connection, query: &str, needle: &str) {
    let rendered = err_text(conn.query(query));
    assert!(
        rendered.contains(needle),
        "{query}\n  expected {needle:?}, got: {rendered}"
    );
}

// ---- 42N42: a named parenthesized group needs a selective path search ----

#[test]
fn named_parenthesized_group_requires_a_selective_path_search() {
    let mut conn = connection();
    // Non-selective (ALL, or the implicit selector): sub-path assignment is
    // not supported.
    for query in [
        "MATCH ALL (p = ()-->*()) RETURN *",
        "MATCH (p = ()-->()) RETURN *",
        "MATCH ALL (p = (a)-->*(b) WHERE a.p > b.p) RETURN *",
    ] {
        assert_rejected(&mut conn, query, "UnsupportedSubPathBinding");
    }
    // A selective search accepts the named group, and so does an unnamed
    // group under ALL.
    run(&mut conn, "MATCH ANY (p = (:A)-->+(:B)) RETURN p");
    run(&mut conn, "MATCH ALL ((:A)-->+(:B)) RETURN 1");
}

// ---- 42N34: no path variable bound inside a quantified path pattern ----

#[test]
fn path_variable_cannot_be_bound_in_a_quantified_path_pattern() {
    let mut conn = connection();
    for selector in [
        "ANY SHORTEST",
        "SHORTEST 1",
        "ALL SHORTEST",
        "SHORTEST 1 GROUP",
        "ANY 1",
    ] {
        let query = format!("MATCH {selector} (p = (:A)--(:B))+ RETURN *");
        assert_rejected(&mut conn, &query, "path cannot be bound in a quantified");
    }
    // The plain named-QPP form is the CIP-28/CIP-40 stretch extension
    // (documented as a GPM feature current Neo4j omits); the ratchet pins it.
    run(&mut conn, "MATCH (p = (:A)--(:B))+ RETURN *");
}

// ---- 42N59: path and sub-path variables may not share a name ----

#[test]
fn path_and_subpath_variable_may_not_share_a_name() {
    let mut conn = connection();
    for query in [
        "MATCH p = ANY SHORTEST (p = (:A)-->+(:B)) RETURN p",
        "MATCH p = SHORTEST 1 (p = (:A)-->+(:B)) RETURN p",
        "MATCH p = ALL SHORTEST (p = (:A)-->+(:B)) RETURN p",
        "MATCH p = SHORTEST 1 GROUP (p = (:A)-->+(:B)) RETURN p",
        "MATCH p = ANY 1 (p = (:A)-->+(:B)) RETURN p",
    ] {
        assert_rejected(&mut conn, query, "VariableAlreadyDefined");
    }
    // Distinct names bind the same path value.
    run(&mut conn, "CREATE (:A)-[:R]->()-[:R]->(:B)");
    assert_eq!(
        scalar_one(
            &mut conn,
            "MATCH p = ANY SHORTEST (q = (:A)-->+(:B)) RETURN p = q AS same"
        ),
        Value::Bool(true)
    );
}

// ---- 42I21: group WHERE may not read the enclosing path variable ----

#[test]
fn group_where_may_not_reference_the_enclosing_path_variable() {
    let mut conn = connection();
    for query in [
        "MATCH p = ((:A)-->+(:B) WHERE length(p) % 2 <> 0) RETURN p",
        "MATCH p = ALL SHORTEST ((:A)-->+(:B) WHERE length(p) % 2 <> 0) RETURN p",
    ] {
        assert_rejected(&mut conn, query, "not allowed to reference `p`");
    }
    // A predicate that does not read the path variable is legal, and so is a
    // group reading its *own* sub-path name.
    run(&mut conn, "MATCH p = ((:A)-->+(:B) WHERE 1 = 1) RETURN p");
    run(
        &mut conn,
        "MATCH ANY SHORTEST (q = (:A)-->+(:B) WHERE length(q) > 0) RETURN 1",
    );
}

// ---- 42I21: REPEATABLE ELEMENTS, sibling path-pattern reference ----

#[test]
fn repeatable_elements_group_where_may_not_reference_a_sibling_path() {
    let mut conn = connection();
    assert_rejected(
        &mut conn,
        "MATCH REPEATABLE ELEMENTS \
         p1 = (x)-->{,10}(y)-->{,10}(z), \
         p2 = ALL SHORTEST ((a)-->{,10}(b)-->{,10}(c) WHERE b.p = x.p) \
         RETURN count(*) AS result",
        "not allowed to reference `x`",
    );
    // The non-selective sibling form is not a selective path pattern, so the
    // rule does not fire.
    run(
        &mut conn,
        "MATCH REPEATABLE ELEMENTS \
         p1 = (x)-->{,10}(y)-->{,10}(z), \
         p2 = ((a)-->{,10}(b)-->{,10}(c) WHERE b.p = x.p) \
         RETURN count(*) AS result",
    );
}

// ---- 42N59: REPEATABLE ELEMENTS, strict interior variable reuse ----

#[test]
fn repeatable_elements_rejects_redeclared_interior_variable() {
    let mut conn = connection();
    // The interior node `b` of the selective path may not be redeclared by
    // another path pattern of the same graph pattern.
    for query in [
        "MATCH REPEATABLE ELEMENTS \
         ALL SHORTEST (:A)-[:R]-{2,3}(b)-[:R]-{1,2}(c:C), (x)--(b)--(:C) \
         RETURN count(*) AS result",
        "MATCH REPEATABLE ELEMENTS \
         ANY SHORTEST (:A)-[:R]-{2,3}(b)-[:R]-{1,2}(c:C), \
         ANY SHORTEST (b)-[:R]-{1,2}(y)-[:R]-{1,2}(z) \
         RETURN count(*) AS result",
    ] {
        assert_rejected(&mut conn, query, "VariableAlreadyDefined");
    }
    // An endpoint (exterior) variable may be reused.
    run(
        &mut conn,
        "MATCH REPEATABLE ELEMENTS \
         ALL SHORTEST (:A)-[:R]-{2,3}(b)-[:R]-{1,2}(c:C), (c)--(:X) \
         RETURN 1",
    );
}

// ---- 42N28: no graph patterns in LIMIT / SKIP ----

#[test]
fn limit_and_skip_reject_graph_patterns() {
    let mut conn = connection();
    assert_rejected(
        &mut conn,
        "MATCH (p:Person) RETURN p.name AS name LIMIT size([(a)-->(b) | b.age])",
        "statically inferrable",
    );
    assert_rejected(
        &mut conn,
        "MATCH (p:Person) RETURN p.name AS name \
         SKIP reduce(sum=0, x IN [(a)-->(b) | b.age] | sum + x)",
        "statically inferrable",
    );
    run(&mut conn, "MATCH (p:Person) RETURN p.name AS name LIMIT 2");
}

// ---- 42N71: an only-USE statement body is incomplete ----

#[test]
fn use_only_statement_body_is_incomplete() {
    let mut conn = connection();
    for query in [
        "USE neo4j",
        "RETURN EXISTS { USE neo4j }",
        "RETURN EXISTS { USE neo4j UNION USE neo4j }",
        "RETURN COUNT { USE neo4j }",
        "CALL { USE neo4j } RETURN 1",
    ] {
        assert_rejected(&mut conn, query, "incomplete query");
    }
    // A leading USE is legal once a concluding clause follows.
    run(&mut conn, "RETURN EXISTS { USE neo4j RETURN 1 }");
}

// ---- 42I58: CREATE self-reference ----

#[test]
fn create_self_reference_is_rejected() {
    let mut conn = connection();
    for query in [
        "CREATE (a)-[r:R]->(b {prop: EXISTS { (a)-[r2]->(c) }})",
        "CREATE (a), (b {prop: EXISTS { (a)-[r2]->(c) }})",
        "CREATE (a)-[r:R]->(b {prop: CASE WHEN true THEN EXISTS { (a)-[r2]->(c) } END})",
        "CREATE (a)-[r:R]->(b {prop: EXISTS { (c) WHERE EXISTS { (c)<-[r2]-(a) }}})",
        // A CALL body's UNION arm reads the created entity too.
        "CREATE (a), (b {prop: EXISTS { CALL { MATCH (z) RETURN z UNION MATCH (a) RETURN a AS z } RETURN z }})",
        // A plain property read is the same 42I58 reference (Cypher 25).
        "CREATE (a {num: 0}), ({num: a.num})",
        "CREATE (a {prop: 7})-[r:R {prop: a.prop}]->(b)",
        "CREATE (a {prop: 7})-[r:R]->(b {prop: labels(a)})",
    ] {
        assert_rejected(&mut conn, query, "cannot be created and referenced");
    }
    // A comprehension binder shadows the created name.
    run(&mut conn, "CREATE (n {prop: true IN [n IN [false] | n]})");
    // A name created by an earlier clause is an existing entity.
    run(&mut conn, "CREATE (a {prop: 7}) CREATE (b {prop: a.prop})");
}

// ---- 22N27 / 22N01: a pre-bound non-entity in a pattern position ----

#[test]
fn prebound_non_entity_in_a_pattern_position_is_a_type_error() {
    let mut conn = connection();
    run(&mut conn, "CREATE (:A {prop: 1, m: 1})");
    // A scalar in a node position (the message's type token is the runtime
    // type; the error class is what matters).
    assert_rejected(
        &mut conn,
        "MATCH (a:A) WITH a.prop AS prop MATCH (b:B)<--(prop) RETURN b",
        "Expected to be NODE",
    );
    // A map in a node / relationship position.
    assert_rejected(
        &mut conn,
        "UNWIND [{name: 'Pelle'}] AS c MATCH (c:Cat) RETURN c.name",
        "of type NODE",
    );
    assert_rejected(
        &mut conn,
        "UNWIND [{rating: 5}] AS l MATCH (:Cat)-[l:LIKES]->() RETURN l.rating",
        "of type RELATIONSHIP",
    );
    // A null pre-binding (a failed OPTIONAL MATCH) still matches nothing.
    assert_eq!(
        scalar_one(
            &mut conn,
            "OPTIONAL MATCH (x:Missing) WITH x MATCH (b:B)<--(x) RETURN count(*)"
        ),
        Value::Int(0)
    );
}

// ---- 42N07: subquery projection may not shadow an outer name ----

#[test]
fn subquery_projection_may_not_shadow_an_outer_name() {
    let mut conn = connection();
    assert_rejected(
        &mut conn,
        "WITH 1 AS petName RETURN EXISTS { MATCH (n) RETURN n.name AS petName } AS hasPet",
        "shadowing a variable",
    );
    // The exact CALL-alias shape the harness pins (the CALL's export is an
    // outer name for the sibling EXISTS).
    assert_rejected(
        &mut conn,
        "MATCH (person:Person) \
         CALL (person) { MATCH (person)-[:HAS_DOG]->(dog:Dog) RETURN dog.name AS petName } \
         RETURN count(*) AS outerCnt, \
         EXISTS { MATCH (person)-[:HAS_DOG]->(dog:Dog) RETURN dog.name AS petName } AS hasPet",
        "shadowing a variable",
    );
    // A bare pass-through (and its redundant self-alias) is a reference, not
    // a re-declaration.
    run(
        &mut conn,
        "WITH 1 AS x RETURN EXISTS { MATCH (n) RETURN x } AS y",
    );
    run(
        &mut conn,
        "WITH 1 AS x RETURN EXISTS { MATCH (n) RETURN x AS x } AS y",
    );
}

// ---- CIP-60: the match-mode phrase belongs directly after MATCH ----

#[test]
fn match_mode_phrase_after_a_path_variable_is_rejected() {
    // Neo4j accepts `REPEATABLE ELEMENTS`/`DIFFERENT RELATIONSHIPS` only
    // between MATCH and the graph pattern; every later position — after a
    // path variable declaration, starting a comma-separated later path,
    // in an OPTIONAL MATCH — is the authored position error, in both
    // spellings of both modes.
    let mut conn = connection();
    for query in [
        "MATCH p = REPEATABLE ELEMENTS (x:SL)-[:R*2..2]->(y) RETURN y.p1",
        "MATCH p = REPEATABLE ELEMENT (x:SL)-[:R*2..2]->(y) RETURN y.p1",
        "MATCH p = DIFFERENT RELATIONSHIPS (x:SL)-[:R*2..2]->(y) RETURN y.p1",
        "MATCH p = DIFFERENT RELATIONSHIP (x:SL)-[:R*2..2]->(y) RETURN y.p1",
        "MATCH (a), p = REPEATABLE ELEMENTS (b)-[:R*2..2]->(c) RETURN p",
        "MATCH (a), REPEATABLE ELEMENTS (b)-[:R*2..2]->(c) RETURN a",
        "OPTIONAL MATCH p = REPEATABLE ELEMENTS (x:SL)-[:R*2..2]->(y) RETURN y.p1",
    ] {
        assert_rejected(&mut conn, query, "InvalidPathModePosition");
    }
    // The CIP-51 *path* modes keep their own variable-first order, and so
    // does the shortestPath() sugar — neither is a match mode.
    run(&mut conn, "CREATE (s:SL {p1: 1})-[:R]->(s)");
    run(&mut conn, "MATCH p = TRAIL (x:SL)-[:R]->(y) RETURN y.p1");
    run(
        &mut conn,
        "MATCH p = shortestPath((x:SL)-[:R]->(y)) RETURN y.p1",
    );
}
