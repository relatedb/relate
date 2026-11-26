//! The traversal-depth admission limit (LABS-610, ADR-0466): an explicit
//! hop bound above the resolved limit is a syntax error at admission, so
//! the statement never reaches the recursive walker; the knob raises it.

use relate::{Database, OpenOptions};

#[test]
fn a_huge_explicit_hop_bound_fails_at_admission_and_never_runs() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    // A tiny graph: if the statement reached the walker, a 1,000,000-hop
    // walk on this cycle would be the pathological case the limit exists
    // for. Admission rejects it first.
    conn.execute("CREATE (a:Node {id: 0}), (b:Node {id: 1}), (a)-[:NEXT]->(b), (b)-[:NEXT]->(a)")
        .unwrap();

    let error = conn
        .execute("MATCH (a:Node)-[*1..1000000]->(b) RETURN count(*) AS n")
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("syntax error: TraversalDepthExceeded: "),
        "{error}"
    );
    assert!(error.contains("paths up to 1,000,000 hops"), "{error}");
    assert!(error.contains("--max-traversal-depth"), "{error}");

    // The same shape with a legitimate bound still runs (relationship
    // uniqueness caps a two-relationship cycle at two hops: 4 trails).
    let result = conn
        .execute("MATCH (a:Node)-[*1..100]->(b) RETURN count(*) AS n")
        .unwrap();
    assert_eq!(result.rows[0][0].as_int(), Some(4));
}

#[test]
fn the_open_option_raises_the_admission_limit() {
    let db = Database::open_with(
        ":memory:",
        OpenOptions {
            max_traversal_depth: Some(600),
            ..Default::default()
        },
    )
    .unwrap();
    let mut conn = db.connect().unwrap();

    // 600 is admissible under the raised limit; the default would reject it.
    conn.execute("MATCH (a)-[*1..600]->(b) RETURN count(*) AS n")
        .unwrap();
    let error = conn
        .execute("MATCH (a)-[*1..601]->(b) RETURN count(*) AS n")
        .unwrap_err()
        .to_string();
    assert!(error.contains("paths up to 601 hops"), "{error}");
}

#[test]
fn an_out_of_range_open_option_fails_before_the_store_exists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store");
    let uri = path.display().to_string();
    for bad in [0, relate::MAX_TRAVERSAL_DEPTH_CEILING + 1] {
        let error = match Database::open_with(
            &uri,
            OpenOptions {
                max_traversal_depth: Some(bad),
                ..Default::default()
            },
        ) {
            Ok(_) => panic!("max_traversal_depth {bad} must fail the open"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("invalid max_traversal_depth: must be between 1 and 2000"),
            "{bad}: {error}"
        );
        assert!(
            !path.exists(),
            "a rejected open must not lease the store directory"
        );
    }
}
