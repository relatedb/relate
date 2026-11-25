//! LABS-801: typed expansion walks the adjacency by interned rel-type id,
//! the written type names resolved once at the walk's construction. These
//! pin the behavior through the full engine: one type, several types, a
//! type the store never held, and typed hops mixed with untyped ones —
//! read-only, inside a write transaction (the overlay's name-keyed arm),
//! and through a variable-length walk.

use crate::common;

use relate::Value;

use common::{connection, rows, run, scalar_one};

/// One rel type: only the `R` edges expand.
#[test]
fn labs801_typed_expansion_single_type() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (a:A)-[:R]->(b:B), (a)-[:S]->(d:D), (a)-[:R]->(e:E)",
    );
    assert_eq!(
        scalar_one(&mut c, "MATCH (:A)-[:R]->(x) RETURN count(x)"),
        Value::Int(2)
    );
}

/// Several types: the disjunction expands both types' chunks, in neither
/// direction reading a string per edge.
#[test]
fn labs801_typed_expansion_multiple_types() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (a:A)-[:R]->(b:B), (a)-[:S]->(d:D), (a)-[:T]->(e:E)",
    );
    assert_eq!(
        scalar_one(&mut c, "MATCH (:A)-[:R|S]->(x) RETURN count(x)"),
        Value::Int(2)
    );
    assert_eq!(
        scalar_one(&mut c, "MATCH (:A)-[:R|S|T]->(x) RETURN count(x)"),
        Value::Int(3)
    );
}

/// A type the store never held has no interned id: the typed expansion
/// matches nothing rather than erroring, alone or beside a known type.
#[test]
fn labs801_unknown_type_matches_nothing() {
    let mut c = connection();
    run(&mut c, "CREATE (a:A)-[:R]->(b:B)");
    assert_eq!(
        scalar_one(&mut c, "MATCH (:A)-[:GHOST]->(x) RETURN count(x)"),
        Value::Int(0)
    );
    assert_eq!(
        scalar_one(&mut c, "MATCH (:A)-[:GHOST|R]->(x) RETURN count(x)"),
        Value::Int(1)
    );
}

/// Typed and untyped hops in one pattern: the untyped hop walks every
/// chunk, the typed ones their own.
#[test]
fn labs801_mixed_typed_and_untyped_hops() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (a:A)-[:R]->(b:B)-[:S]->(d:D), (a)-[:T]->(e:E)-[:S]->(d)",
    );
    // Typed first hop, untyped second.
    assert_eq!(
        scalar_one(&mut c, "MATCH (:A)-[:R]->()-[]->(x) RETURN count(x)"),
        Value::Int(1)
    );
    // Untyped first hop, typed second.
    assert_eq!(
        scalar_one(&mut c, "MATCH (:A)-[]->()-[:S]->(x) RETURN count(x)"),
        Value::Int(2)
    );
    // A variable-length typed walk.
    assert_eq!(
        scalar_one(&mut c, "MATCH (:A)-[:R|T*1..2]->(x) RETURN count(x)"),
        Value::Int(2)
    );
}

/// The same filters inside a write transaction: the overlay's rel copies
/// carry type strings, so its arm of the resolved walk compares names — a
/// rel created in the transaction of a never-committed type is found by
/// its typed expansion.
#[test]
fn labs801_typed_expansion_sees_own_writes() {
    let mut c = connection();
    run(&mut c, "CREATE (a:A)-[:R]->(b:B)");
    let got = rows(
        &mut c,
        "MATCH (a:A) CREATE (a)-[:NEWTYPE]->(z:Z) \
         WITH a MATCH (a)-[:NEWTYPE]->(x) RETURN count(x)",
    );
    assert_eq!(got, vec![vec![Value::Int(1)]]);
    // The committed type still expands beside the scratch rels.
    let got = rows(
        &mut c,
        "MATCH (a:A) CREATE (a)-[:R]->(z:Z) \
         WITH a MATCH (a)-[:R]->(x) RETURN count(x)",
    );
    assert_eq!(got, vec![vec![Value::Int(2)]]);
}
