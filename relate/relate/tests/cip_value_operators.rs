//! The 2026.09/2026.10 Cypher additions in one suite: the value
//! operators (map addition, map comprehension, string interpolation),
//! explicit GROUP BY keys, UUID values and their WAL roundtrips,
//! dynamic property access, SET left-hand-side precedence, toString
//! static typing, and the fulltext, vector, and hybrid SEARCH clause
//! filters. Every case below is authored locally for this suite; the
//! expectations are recomputed on this engine, and the value-operator
//! semantics were cross-checked against live Neo4j images where one
//! exists.

mod common;

use relate::{Database, Value};
use std::collections::BTreeMap;

use common::{connection, map};

// Map addition: MAP + MAP merges with last-write-wins; null on either
// side is null; MAP + LIST / LIST + MAP build a list; anything else is a
// type error.
#[test]
fn map_addition() {
    let mut c = connection();

    let r = c.query("RETURN {x: 2, y: 3} + {y: 7, z: 5} AS r").unwrap();
    assert_eq!(
        r.rows,
        vec![vec![map(&[
            ("x", Value::Int(2)),
            ("y", Value::Int(7)),
            ("z", Value::Int(5)),
        ])]]
    );

    // Null values overwrite on collision (last write wins, even null).
    let r = c.query("RETURN {x: 2, y: 3} + {y: null} AS r").unwrap();
    assert_eq!(
        r.rows,
        vec![vec![map(&[("x", Value::Int(2)), ("y", Value::Null)])]]
    );

    // Chained merges: rightmost write wins per key.
    let r = c
        .query("RETURN {x: 2, y: 3} + {y: 7, z: 5} + {x: 40} + {y: 9} AS r")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![map(&[
            ("x", Value::Int(40)),
            ("y", Value::Int(9)),
            ("z", Value::Int(5)),
        ])]]
    );

    // Null propagation.
    let r = c
        .query("RETURN {x: 2} + null AS rn, null + {y: 3} AS ln")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Null, Value::Null]]);

    // MAP + LIST / LIST + MAP: the map becomes a list element.
    let r = c.query("RETURN {x: 2} + [] AS r").unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::List(vec![map(&[("x", Value::Int(2))])].into())]]
    );
    let r = c.query("RETURN [] + {x: 2} + {y: 3} AS r").unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::List(
            vec![map(&[("x", Value::Int(2))]), map(&[("y", Value::Int(3))]),].into()
        )]]
    );

    // Non-map, non-list operands stay a type error.
    assert!(c.query("RETURN {x: 2} + 1 AS r").is_err());
    assert!(c.query("RETURN 1 + {x: 2} AS r").is_err());
}

// Map keys render sorted (BTreeMap): the merged map keeps canonical order.
#[test]
fn merged_map_key_order() {
    let mut c = connection();
    let r = c.query("RETURN {v: 9} + {b: 1} AS r").unwrap();
    assert_eq!(
        r.rows[0][0],
        Value::Map(
            BTreeMap::from([("b".into(), Value::Int(1)), ("v".into(), Value::Int(9)),]).into()
        )
    );
}

// Interpolated string literals.
#[test]
fn string_interpolation() {
    let mut c = connection();

    let r = c.query("RETURN s'x{2+3}y' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("x5y".into())]]);

    let r = c.query("RETURN S'{7}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("7".into())]]);

    let r = c.query("RETURN s' \\{w\\} {2}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String(" {w} 2".into())]]);

    // Round-2 review: escapes inside `{...}` reach the embed's re-parse
    // verbatim, so a quoted string with an escaped quote is not corrupted.
    let r = c.query(r"RETURN s'{coalesce('p\'q','r')}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("p'q".into())]]);
    // The direct call evaluates to the same value.
    let direct = c.query(r"RETURN coalesce('p\'q','r') AS r").unwrap();
    assert_eq!(direct.rows, r.rows);

    // `\{` / `\}` inside an embed still parse (unknown escape stays literal).
    let r = c.query(r"RETURN s'{coalesce('u\{v','w')}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String(r"u\{v".into())]]);

    let r = c.query("WITH 6 AS n RETURN s'{n}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("6".into())]]);

    let r = c.query("RETURN s'p{null}q' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::Null]]);

    assert!(c.query("RETURN s'{[3, 4]}' AS r").is_err());
    assert!(c.query("RETURN s'{ {k: 1} }' AS r").is_err());

    // Round-3 review: an escaped backslash immediately before an embed is a
    // backslash followed by the embed, not an escaped literal brace. `\{`
    // is a literal brace; a doubled backslash decodes once.
    let r = c.query(r"RETURN s'\\{3}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("\\3".into())]]);
    let r = c.query(r"RETURN s'\{2}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("{2}".into())]]);
    let r = c.query(r"RETURN s'm\\n{4}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("m\\n4".into())]]);

    // Round-3 review: a backtick-quoted identifier inside an embed is inert,
    // so a `}` or `'` inside the name does not end the embed.
    let r = c.query("WITH 5 AS `k}m` RETURN s'{`k}m`}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("5".into())]]);
    let r = c.query("WITH 'z' AS `v'w` RETURN s'{`v'w`}' AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("z".into())]]);

    // Walker regression guard: an embed is a full expression, so the
    // compile-time aggregation passes must see inside it. Before
    // interpolation walker support this escaped the NestedAggregation
    // check and only failed at runtime with a coercion error.
    let err = c
        .query("CREATE (n) WITH collect(n.name) AS c RETURN count(s'{collect(n)}') AS r")
        .unwrap_err();
    assert!(
        err.to_string().contains("NestedAggregation"),
        "expected a compile-time NestedAggregation, got: {err}"
    );

    // Same for the non-constant-argument check: rand() inside an embed of an
    // aggregation argument is still a compile-time NonConstantExpression.
    let err = c
        .query("UNWIND [1] AS x RETURN count(s'{rand()}') AS r")
        .unwrap_err();
    assert!(
        err.to_string().contains("NonConstantExpression"),
        "expected a compile-time NonConstantExpression, got: {err}"
    );
}

// Map comprehensions.
#[test]
fn map_comprehension() {
    let mut c = connection();

    let r = c
        .query("WITH {p: 3, q: 5} AS m RETURN {k : v IN m | k : v * 10} AS r")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![map(&[("p", Value::Int(30)), ("q", Value::Int(50))])]]
    );

    let r = c
        .query("WITH [4, 6] AS l RETURN {i IN l | 'n' + toString(i): i} AS r")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![map(&[("n4", Value::Int(4)), ("n6", Value::Int(6))])]]
    );

    let r = c
        .query("WITH [4, 5, 6] AS l RETURN {i IN l WHERE i > 4 | 'y': i} AS r")
        .unwrap();
    assert_eq!(r.rows, vec![vec![map(&[("y", Value::Int(6))])]]);

    // Duplicate keys: last write wins.
    let r = c
        .query("WITH [2, 3, 4] AS l RETURN {i IN l | 'd': i * 3} AS r")
        .unwrap();
    assert_eq!(r.rows, vec![vec![map(&[("d", Value::Int(12))])]]);

    // Null sources are null; a non-string key is a type error.
    let r = c.query("RETURN {k : v IN null | k : v} AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::Null]]);
    assert!(c.query("RETURN {i IN [7] | i : i} AS r").is_err());

    // `{k: v IN m}` without a pipe stays a map literal (value = IN expr).
    let r = c
        .query("WITH [2] AS m, 7 AS v RETURN {k: v IN m} AS r")
        .unwrap();
    assert_eq!(r.rows, vec![vec![map(&[("k", Value::Bool(false))])]]);

    // Bindings do not leak out of the comprehension.
    assert!(c
        .query("WITH [3] AS l WITH {i IN l | 'c': i} AS m RETURN i AS r")
        .is_err());

    // The comprehension source may contain aggregation (the
    // list-comprehension equivalent already worked); the runtime
    // aggregate evaluator needs a MapComp arm or this errors with
    // "collect() is an aggregating function".
    let r = c
        .query("UNWIND [8, 9] AS x RETURN {i IN collect(x) | 'q' + toString(i): i} AS r")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![map(&[("q8", Value::Int(8)), ("q9", Value::Int(9))])]]
    );

    // Aggregation in the projection/WHERE is a compile-time error, same
    // as for list comprehensions.
    assert!(c
        .query("UNWIND [8, 9] AS x RETURN {i IN collect(x) | 'q': count(i)} AS r")
        .is_err());
    assert!(c
        .query("UNWIND [8, 9] AS x RETURN {i IN collect(x) WHERE count(i) > 0 | 'q': i} AS r")
        .is_err());
}

// Round-3 review: a brace literal with no top-level `|` is a map
// literal, and the comprehension speculation must not full-parse it first. That
// speculation re-parsed the same tokens, doubling the work per nesting level
// (pre-fix, in a debug build: depth 12 took 6.5 ms, depth 16 took 105 ms, depth
// 20 took 1.7 s), so 50 pipeless levels is a bounded-time assertion the pre-fix
// parser cannot satisfy. Parsing and evaluating it recurse per level, deeper
// than a default test-thread stack allows, so the work runs on an explicitly
// sized one -- the same reason the spec harness's eval suite sets
// RUST_MIN_STACK.
#[test]
fn pipe_less_nesting_stays_a_map_literal() {
    relate_test_support::run_with_big_stack(|| {
        let mut c = connection();
        let depth = 50;
        // Each level is the reviewed shape: a map-pair prefix whose value is
        // an `IN` over the nested literal, wrapped in a list so the innermost
        // source is a real list and every level evaluates (`9 IN [<map>]` is
        // false, so the whole literal is one pair per level).
        let mut expr = String::from("m");
        for _ in 0..depth {
            expr = format!("{{a: b IN [{expr}]}}");
        }
        let r = c
            .query(&format!("WITH [9] AS m, 9 AS b RETURN {expr} AS r"))
            .unwrap();
        assert_eq!(r.rows, vec![vec![map(&[("a", Value::Bool(false))])]]);
    });
}

// Explicit GROUP BY keys.
#[test]
fn group_by() {
    let mut c = connection();

    let r = c
        .query("UNWIND [2,2,5,5,8] AS x RETURN x AS k, count(*) AS c GROUP BY x ORDER BY k")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![Value::Int(2), Value::Int(2)],
            vec![Value::Int(5), Value::Int(2)],
            vec![Value::Int(8), Value::Int(1)],
        ]
    );

    // ALL = non-aggregating items as keys.
    let r = c
        .query("UNWIND [3,3,7] AS x RETURN x, count(*) AS c GROUP BY ALL ORDER BY x")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![Value::Int(3), Value::Int(2)],
            vec![Value::Int(7), Value::Int(1)],
        ]
    );

    // () = one global group (aggregate-only projections).
    let r = c
        .query("UNWIND [4,9] AS x RETURN count(*) AS c GROUP BY ()")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(2)]]);

    // Key differs from the projection; alias reference; WHERE filters.
    let r = c
        .query("UNWIND [4,4,7,9] AS x RETURN x + 0 AS k, count(*) AS c GROUP BY x ORDER BY k")
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    let r = c
        .query("UNWIND [6,6,7] AS x WITH x AS k, count(*) AS c GROUP BY k WHERE c > 1 RETURN k, c")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(6), Value::Int(2)]]);

    // Non-aggregating GROUP BY acts as DISTINCT.
    let r = c.query("UNWIND [5,5,9] AS x RETURN x GROUP BY x").unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(5)], vec![Value::Int(9)]]);

    // Regression (PR #62 review): explicit non-empty keys over an empty
    // input yield zero rows, not a phantom global group. All-aggregate
    // projection items are the shape that used to slip through the
    // empty-input special case.
    let mut empty = connection();
    let r = empty
        .query("MATCH (n) RETURN count(*) AS c GROUP BY n.x")
        .unwrap();
    assert!(r.rows.is_empty(), "phantom global group: {:?}", r.rows);
    // ... while the keyless forms still emit their one global group.
    let r = empty
        .query("MATCH (n) RETURN count(*) AS c GROUP BY ()")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(0)]]);
    let r = empty.query("MATCH (n) RETURN count(*) AS c").unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(0)]]);

    // Regression (PR #62 review): the one-hop fold keys on non-aggregating
    // projection items, so with an explicit GROUP BY it must stand down.
    // Here there is no non-aggregating item at all: a fold that keys on the
    // (empty) item list would collapse the two rows into one group, while
    // the explicit keys `(n.v, m.w)` keep them apart. (`RETURN n.v ...
    // GROUP BY m.w` is not a valid probe: a non-grouping project item is
    // 42I18.)
    c.query(
        "CREATE (p1:Port {v: 4}), (s1:Star {w: 11}), (p1)-[:LINK]->(s1)              CREATE (p2:Port {v: 6}), (s2:Star {w: 13}), (p2)-[:LINK]->(s2)",
    )
    .unwrap();
    let r = c
        .query("MATCH (n:Port)-[r:LINK]->(m:Star) RETURN count(*) AS c GROUP BY n.v, m.w")
        .unwrap();
    assert_eq!(r.rows.len(), 2, "fold ignored GROUP BY: {:?}", r.rows);
    assert!(r.rows.iter().all(|row| row[0] == Value::Int(1)));

    // Regression (PR #62 round 2): a grouping key mixed into an aggregation
    // expression is valid -- `x` is grouped, so `count(*) + x` is not
    // ambiguous. The grouping-key set must come from the explicit GROUP BY,
    // not only from non-aggregating projection items.
    let r = c
        .query("UNWIND [4,4,9] AS x RETURN count(*) + x AS c GROUP BY x ORDER BY c")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::Int(6)], vec![Value::Int(10)]],
        "GROUP BY key rejected inside aggregation expression"
    );
    let r = c
        .query("UNWIND [4,4,9] AS x WITH count(*) + x AS c GROUP BY x RETURN c ORDER BY c")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(6)], vec![Value::Int(10)]]);

    // Without the GROUP BY the same expression is still rejected: `x` is
    // not a grouping key then.
    assert!(
        c.query("UNWIND [4,4,9] AS x RETURN count(*) + x AS c")
            .is_err(),
        "ungrouped variable must stay ambiguous"
    );
    // A variable that is not a grouping key stays ambiguous even with keys.
    assert!(
        c.query("UNWIND [4,4,9] AS x UNWIND [3] AS y RETURN count(*) + y AS c GROUP BY x")
            .is_err(),
        "non-key variable must stay ambiguous"
    );

    // Regression (PR #62 round 2): `GROUP BY ()` before a subquery's closing
    // brace -- the `()` must be recognised as the empty grouping set, not as
    // a parenthesized key.
    let r = c
        .query("CALL { UNWIND [5,8] AS x RETURN count(*) AS c GROUP BY () } RETURN c")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(2)]]);
}

// SET resolves against pre-clause state (dynamic targets must not
// observe earlier assignments in the same clause).
#[test]
fn set_lhs_precedence() {
    let mut c = connection();
    c.query("CREATE (:Deck {p: 0, q: 1})").unwrap();
    c.query("MATCH (n:Deck) SET n['p'] = n.q, n['q'] = n.p")
        .unwrap();
    let r = c.query("MATCH (n) RETURN n.p AS p, n.q AS q").unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(1), Value::Int(0)]]);

    c.query("CREATE (:Hull {p: 0, q: 1})").unwrap();
    c.query("MATCH (n:Hull) SET n.p = n.q, n.q = n.p").unwrap();
    let r = c.query("MATCH (n:Hull) RETURN n.p AS p, n.q AS q").unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(1), Value::Int(0)]]);

    // Dynamic key resolved pre-clause: writes go to the old key's name.
    c.query("CREATE (:Yard {key: 'aim', aim: 'stale'})")
        .unwrap();
    c.query("MATCH (n:Yard) SET n[n.key] = 'fresh', n.key = 'done'")
        .unwrap();
    let r = c
        .query("MATCH (n:Yard) RETURN n.key AS key, n.done AS done, n.aim AS aim")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![
            Value::String("done".into()),
            Value::Null,
            Value::String("fresh".into())
        ]]
    );
}

// The UUID type.
#[test]
fn uuid_type() {
    let mut c = connection();
    let id = "5d8f3a2c-1e47-4b69-9c02-7f3a8d5e6b10";

    let r = c
        .query(&format!("RETURN toString(uuid('{id}')) AS r"))
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String(id.into())]]);

    // The two-integer construction round-trip; the integer pair's UUID is
    // recomputed on this engine (the construction itself was verified
    // against live Neo4j).
    let r = c
        .query("RETURN toString(uuid(-1234567890123456789, 876543210987654321)) AS r")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::String(
            "eeddef0b-8216-7eeb-0c2a-1b6801330cb1".into()
        )]]
    );

    let r = c
        .query(&format!(
            "RETURN uuid('{id}') IS :: UUID AS p, valueType(uuid('{id}')) AS t"
        ))
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![
            Value::Bool(true),
            Value::String("UUID NOT NULL".into())
        ]]
    );
}

// Relationship property value access (static and dynamic).
#[test]
fn rel_value_access() {
    let mut c = connection();
    c.query("CREATE (:Port)-[:LINK {tag: 'yes'}]->(:Star)")
        .unwrap();
    let r = c
        .query("MATCH ()-[r:LINK]->() RETURN r['tag'] AS a, r.tag AS b")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![
            Value::String("yes".into()),
            Value::String("yes".into())
        ]]
    );
    c.query("MATCH ()-[r:LINK]->() SET r['mark'] = 7 REMOVE r['tag']")
        .unwrap();
    let r = c
        .query("MATCH ()-[r:LINK]->() RETURN r.mark AS mark, r.tag AS tag")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(7), Value::Null]]);
}

// Fulltext SEARCH + vector search property filters.
#[test]
fn fulltext_and_vector_filters() {
    let mut c = connection();
    c.query("CREATE FULLTEXT INDEX ft FOR (p:Person) ON EACH [p.name]")
        .unwrap();
    c.query("CREATE (:Person {name: 'red fox', age: 30}), (:Person {name: 'fox den', age: 10}), (:Person {name: 'unrelated'})").unwrap();

    let r = c
        .query("MATCH (p) SEARCH p IN (FULLTEXT INDEX ft FOR 'red den' LIMIT 10) RETURN DISTINCT p.name AS n")
        .unwrap();
    assert_eq!(r.rows.len(), 2);

    // Case-insensitive; SCORE AS positive; empty on no match.
    let r = c
        .query("MATCH (p) SEARCH p IN (FULLTEXT INDEX ft FOR 'Red' LIMIT 10) SCORE AS s RETURN DISTINCT p.name AS n, s > 0 AS pos")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::String("red fox".into()), Value::Bool(true)]]
    );
    let r = c
        .query("MATCH (p) SEARCH p IN (FULLTEXT INDEX ft FOR 'absent' LIMIT 10) RETURN p.name AS n")
        .unwrap();
    assert!(r.rows.is_empty());
    // A null query matches nothing — null propagates to no candidates,
    // never the STRING type error.
    let r = c
        .query("MATCH (p) SEARCH p IN (FULLTEXT INDEX ft FOR null LIMIT 10) RETURN p.name AS n")
        .unwrap();
    assert!(r.rows.is_empty());

    // WHERE with FULLTEXT is rejected; missing index raises.
    assert!(c
        .query("MATCH (p) SEARCH p IN (FULLTEXT INDEX ft FOR 'red' WHERE p.age > 20 LIMIT 10) RETURN p.name")
        .is_err());
    assert!(c
        .query("MATCH (p) SEARCH p IN (FULLTEXT INDEX absent FOR 'red' LIMIT 10) RETURN p.name")
        .is_err());

    // Vector search with a WITH-declared property filter; SKIP is
    // fulltext-only.
    c.query("CREATE VECTOR INDEX vidx FOR (p:Person) ON p.embed WITH [p.age]")
        .unwrap();
    c.query("CREATE (:Person {name: 'zed', age: 45, embed: [0.0, 1.0]})")
        .unwrap();
    let r = c
        .query("MATCH (p) SEARCH p IN (VECTOR INDEX vidx FOR [0.0, 1.0] WHERE p.age > 40 LIMIT 10) RETURN DISTINCT p.name AS n")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("zed".into())]]);
    assert!(c
        .query("MATCH (p) SEARCH p IN (VECTOR INDEX vidx FOR [0.0, 1.0] SKIP 1 LIMIT 10) RETURN p.name")
        .is_err());
}

// Chained SEARCH clauses intersect; an already-bound binding variable is
// not re-bound.
#[test]
fn hybrid_search_intersection() {
    let mut c = connection();
    c.query("CREATE VECTOR INDEX vidx FOR (p:Person) ON p.embed")
        .unwrap();
    c.query("CREATE FULLTEXT INDEX ft FOR (p:Person) ON EACH [p.name]")
        .unwrap();
    c.query("CREATE (:Person {name: 'ka', embed: [1.0, 0.0]}), (:Person {name: 'lo', embed: [0.0, 1.0]})").unwrap();

    let r = c
        .query("MATCH (p) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] LIMIT 10) WITH p MATCH (p) SEARCH p IN (FULLTEXT INDEX ft FOR 'ka' LIMIT 10) RETURN DISTINCT p.name AS n")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("ka".into())]]);

    let r = c
        .query("MATCH (p) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] LIMIT 10) WITH p MATCH (p) SEARCH p IN (FULLTEXT INDEX ft FOR 'nohit' LIMIT 10) RETURN count(*) AS c")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(0)]]);

    assert!(c
        .query("MATCH (p) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] LIMIT 10) SEARCH p IN (FULLTEXT INDEX ft FOR 'ka' LIMIT 10) RETURN p.name")
        .is_err());
}

// Pre-bound binds: a null binding joins no candidate, and a non-null one
// keeps the LIMIT as a hard candidate window.
#[test]
fn prebound_null_binding_matches_nothing() {
    let mut c = connection();
    c.query("CREATE VECTOR INDEX ridx FOR ()-[r:TOW]->() ON r.vec")
        .unwrap();
    c.query(
        "CREATE (d:Deck)-[:TOW {name: 'alpha', vec: [1.0, 0.0]}]->(h:Hull), \
                (h)-[:TOW {name: 'beta', vec: [0.9, 0.1]}]->(d), \
                (:Log)",
    )
    .unwrap();

    // A pre-bound relationship search with a null relationship from
    // OPTIONAL MATCH: the null binding matches nothing (live Neo4j
    // 2026.10.1: no rows), never every candidate.
    let r = c
        .query(
            "OPTIONAL MATCH (:Log)-[r:TOW]->() MATCH ()-[r]->() \
             SEARCH r IN (VECTOR INDEX ridx FOR [1.0, 0.0] LIMIT 2) RETURN r.name AS name",
        )
        .unwrap();
    assert!(
        r.rows.is_empty(),
        "a null pre-bound relationship matches no candidate: {:?}",
        r.rows
    );

    // Pre-bound rows keep their window: `beta` is pre-bound on one incoming
    // row but ranks below `LIMIT 1`, so it is dropped rather than widening
    // the search (live Neo4j 2026.10.1: `alpha` alone).
    let r = c
        .query(
            "MATCH ()-[r:TOW]->() MATCH ()-[r]->() \
             SEARCH r IN (VECTOR INDEX ridx FOR [1.0, 0.0] LIMIT 1) RETURN r.name AS name",
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("alpha".into())]]);
}

// UUID property values (cherry-picked from #55).
#[test]
fn uuid_property_values() {
    let mut c = connection();
    let id = "5d8f3a2c-1e47-4b69-9c02-7f3a8d5e6b10";
    let other = "00000000-0000-0000-0000-00000000000a";

    // CREATE with a UUID property.
    let r = c
        .query(&format!(
            "CREATE (n:P {{id: uuid('{id}')}}) RETURN toString(n.id) AS r"
        ))
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String(id.into())]]);

    // SET a UUID property.
    let r = c
        .query(&format!(
            "MATCH (n:P) SET n.other = uuid('{other}') RETURN toString(n.other) AS r"
        ))
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String(other.into())]]);

    // SET += map with a UUID value.
    let r = c
        .query(&format!(
            "MATCH (n:P) SET n += {{tag: uuid('{other}')}} RETURN toString(n.tag) AS r"
        ))
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String(other.into())]]);

    // MERGE keyed on a UUID property.
    let r = c
        .query(&format!(
            "MERGE (m:Q {{k: uuid('{id}')}}) RETURN toString(m.k) AS r"
        ))
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String(id.into())]]);

    // Relationship property.
    let r = c
        .query(&format!(
            "CREATE (:Port)-[rel:LINK {{u: uuid('{id}')}}]->(:Star) RETURN toString(rel.u) AS r"
        ))
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String(id.into())]]);

    // Lists of UUIDs (homogeneous list kind must accept UUID).
    let r = c
        .query(&format!(
            "CREATE (n:L {{ids: [uuid('{id}'), uuid('{other}')]}}) \
             RETURN [x IN n.ids | toString(x)] AS r"
        ))
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::List(
            vec![Value::String(id.into()), Value::String(other.into())].into()
        )]]
    );

    // Non-property types are still rejected.
    assert!(c.query("CREATE (n:Bad {m: {a: 1}})").is_err());
}

// A UUID property survives a durable WAL replay (file reopen).
#[test]
fn uuid_property_wal_roundtrip() {
    let id = "5d8f3a2c-1e47-4b69-9c02-7f3a8d5e6b10";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    {
        let db = Database::open(path).unwrap();
        let mut c = db.connect().unwrap();
        c.query(&format!("CREATE (:P {{id: uuid('{id}')}})"))
            .unwrap();
    }
    let db = Database::open(path).unwrap();
    let mut c = db.connect().unwrap();
    let r = c.query("MATCH (n:P) RETURN toString(n.id) AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String(id.into())]]);
}

// Persistence round-trip of the UUID value through the WAL tag.
#[test]
fn uuid_wal_roundtrip() {
    let id = "5d8f3a2c-1e47-4b69-9c02-7f3a8d5e6b10";
    let parsed = relate::uuid_parse(id).unwrap();
    let v = Value::Uuid(parsed);
    assert_eq!(relate::uuid_hyphenated(parsed), id);
    let tagged = v.to_json_tagged();
    assert_eq!(Value::from_json_tagged(&tagged).unwrap(), v);
}

// A homogeneous list literal is a static `LIST<T>`, not the `LIST<ANY>`
// the `toString` family accepts. The rejection is a compile-time one —
// it fires under EXPLAIN and in a CASE arm that never evaluates — while
// a dynamically typed list renders.
#[test]
fn tostring_rejects_a_static_list_at_compile_time() {
    let mut c = connection();

    for query in [
        "RETURN toString([4, 6]) AS r",
        "RETURN toStringOrNull([4, 6]) AS r",
        "EXPLAIN RETURN toString([4, 6]) AS r",
        "RETURN CASE WHEN false THEN toString([4, 6]) ELSE 'x' END AS r",
    ] {
        let err = c.query(query).expect_err(query);
        assert!(
            err.to_string().contains("InvalidArgumentValue"),
            "`{query}` should be a compile-time type error, got {err}"
        );
    }

    // A list read from a variable has no statically known element type.
    let r = c
        .query("UNWIND [[4, 6]] AS xs RETURN toString(xs) AS r")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("[4, 6]".into())]]);

    // Mixed classes (and a null element) are `LIST<ANY>`.
    let r = c.query("RETURN toString([4, 'k']) AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("[4, k]".into())]]);
    let r = c.query("RETURN toString([4, null, 6]) AS r").unwrap();
    assert_eq!(r.rows, vec![vec![Value::String("[4, null, 6]".into())]]);
}
