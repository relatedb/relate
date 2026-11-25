//! An aggregate in a pattern comprehension's projection or WHERE is the
//! comprehension family's 42I24 (ADR-0685): the form lowers to its
//! `collect { … }` subquery, whose aggregating RETURN would otherwise fold
//! the aggregate into one element — `[(n)-->(m) | count(m)]` answering
//! `[2]` instead of rejecting the spelling.

use crate::common;

use common::{connection, refused_literal};

/// Both non-aggregating slots of the pattern form are rejected at compile
/// time with the family's class, exactly as the list and map forms are.
#[test]
fn an_aggregate_in_a_pattern_comprehension_is_rejected() {
    // Both statements are rejected before any evaluation, so no graph is
    // needed — the empty store is the fixture.
    let mut c = connection();
    for query in [
        "MATCH (n:N {id: 1}) RETURN [(n)-[:R]->(m) | count(m)] AS l",
        "MATCH (n:N {id: 1}) RETURN [(n)-[:R]->(m) WHERE count(m) > 0 | m] AS l",
    ] {
        let error = refused_literal(&mut c, query).to_string();
        assert!(
            error.contains("InvalidAggregation"),
            "{query}\n  expected the InvalidAggregation rejection, got: {error}"
        );
    }
}
