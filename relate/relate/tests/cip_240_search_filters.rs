//! CIP-240 "Vector search with single-stage filtering": the accepted filter
//! forms (existence, property `IN`, the entity-set match filter, and a
//! property predicate naming the FOR entity variable) and the rejected
//! shapes, plus the CIP-271 vector `toString` form. The expectations are
//! Neo4j's acceptance-feature tables (`SearchWithSingleStageAcceptance`,
//! `TypeConversion4`, `StringInterpolationAcceptance`).

mod common;

use relate::Value;

/// The node corpus: one vector index with two additional properties, three
/// rated people and one unrated (so `IS NULL` has a distinct answer).
fn store() -> relate::Connection {
    let mut c = common::connection();
    c.query("CREATE VECTOR INDEX vidx FOR (p:Person) ON p.embed WITH [p.rating, p.year]")
        .unwrap();
    c.query(
        "CREATE (:Person {name: 'a', rating: 7.3, year: 1950, embed: [1.0, 0.0]}), \
         (:Person {name: 'b', rating: 7.6, year: 1937, embed: [1.0, 0.1]}), \
         (:Person {name: 'c', rating: 7.7, year: 1998, embed: [0.9, 0.1]}), \
         (:Person {name: 'd', year: 2000, embed: [0.0, 1.0]})",
    )
    .unwrap();
    c
}

/// The relationship-index corpus for the relationship candidate arm.
fn relationship_store() -> relate::Connection {
    let mut c = common::connection();
    c.query("CREATE VECTOR INDEX ridx FOR ()-[r:REL]->() ON r.vec WITH [r.name]")
        .unwrap();
    c.query(
        "CREATE (:A)-[:REL {name: 'x', vec: [1.0, 0.0]}]->(:B), \
         (:A)-[:REL {name: 'y', vec: [0.0, 1.0]}]->(:B)",
    )
    .unwrap();
    c
}

/// The one string column of a result, one entry per row.
fn names(result: &relate::QueryResult) -> Vec<String> {
    result
        .rows
        .iter()
        .map(|row| match &row[0] {
            Value::String(name) => name.to_string(),
            other => panic!("expected a string name, got {other:?}"),
        })
        .collect()
}

/// The accepted forms beside the plain comparison: existence, `IN` on an
/// additional property, the entity-set match filter, and a property
/// predicate naming the FOR entity variable.
#[test]
fn cip_240_search_filter_forms() {
    let mut c = store();

    // `IS [NOT] NULL`: existence on an additional property.
    assert_eq!(
        names(
            &c.query(
                "MATCH (p:Person) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] \
                 WHERE p.rating IS NOT NULL LIMIT 10) RETURN p.name AS n ORDER BY n"
            )
            .unwrap()
        ),
        ["a", "b", "c"]
    );
    assert_eq!(
        names(
            &c.query(
                "MATCH (p:Person) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] \
                 WHERE p.rating IS NULL LIMIT 10) RETURN p.name AS n ORDER BY n"
            )
            .unwrap()
        ),
        ["d"]
    );

    // `IN` on an additional property.
    assert_eq!(
        names(
            &c.query(
                "MATCH (p:Person) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] \
                 WHERE p.rating IN [7.6, 7.7] LIMIT 10) RETURN p.name AS n ORDER BY n"
            )
            .unwrap()
        ),
        ["b", "c"]
    );

    // `IN null` is null, not an error: neither the property nor the entity
    // form returns a row (one null-RHS policy for every IN operand).
    for null_filter in ["p.rating IN null", "p IN null"] {
        assert!(
            names(
                &c.query(&format!(
                    "MATCH (p:Person) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] \
                     WHERE {null_filter} LIMIT 10) RETURN p.name AS n"
                ))
                .unwrap()
            )
            .is_empty(),
            "`{null_filter}` filters every candidate without raising"
        );
    }

    // The entity-set match filter keeps only the collected candidates.
    assert_eq!(
        names(
            &c.query(
                "MATCH (candidate:Person) WHERE candidate.name IN ['b', 'c'] \
                 WITH collect(candidate) AS candidates \
                 MATCH (p:Person) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] \
                 WHERE p IN candidates LIMIT 10) RETURN p.name AS n ORDER BY n"
            )
            .unwrap()
        ),
        ["b", "c"]
    );

    // A property predicate may name the FOR entity variable; its property
    // value is the per-row RHS. The binding variable's side is the
    // candidate property whichever way round the comparison reads, so both
    // orders keep the same window (live Neo4j 2026.10.1:
    // SearchWithSingleStageAcceptance's "independent property as LHS" and
    // its RHS mirror return the same rows).
    for predicate in ["q.rating <= p.rating", "p.rating >= q.rating"] {
        assert_eq!(
            names(
                &c.query(&format!(
                    "MATCH (q:Person {{name: 'a'}}) MATCH (p:Person) \
                     SEARCH p IN (VECTOR INDEX vidx FOR q.embed \
                     WHERE {predicate} LIMIT 10) RETURN p.name AS n ORDER BY n"
                ))
                .unwrap()
            ),
            ["a", "b", "c"],
            "`{predicate}`"
        );
        // `LIMIT 2` counts the candidates the predicate keeps: `d` carries
        // no rating, and the two best-ranked rated candidates survive.
        assert_eq!(
            names(
                &c.query(&format!(
                    "MATCH (q:Person {{name: 'a'}}) MATCH (p:Person) \
                     SEARCH p IN (VECTOR INDEX vidx FOR q.embed \
                     WHERE {predicate} LIMIT 2) RETURN p.name AS n ORDER BY n"
                ))
                .unwrap()
            ),
            ["a", "b"],
            "`{predicate}` LIMIT 2"
        );
    }

    // A re-bound binding variable searches once per incoming row: four
    // rows, the two best-rated candidates each.
    assert_eq!(
        names(
            &c.query(
                "MATCH (p:Person) WITH 1 AS sq MATCH (p:Person) \
                 SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] \
                 WHERE p.rating IS NOT NULL LIMIT 2) RETURN p.name AS n ORDER BY n"
            )
            .unwrap()
        ),
        ["a", "a", "a", "a", "b", "b", "b", "b"]
    );
}

/// Existence, membership and the entity-set filter on a relationship index
/// (the relationship candidate arm).
#[test]
fn cip_240_relationship_index_filters() {
    let mut c = relationship_store();

    assert_eq!(
        names(
            &c.query(
                "MATCH ()-[rel:REL]->() SEARCH rel IN (VECTOR INDEX ridx FOR [1.0, 0.0] \
                 WHERE rel.name IN ['x', 'y'] LIMIT 10) RETURN rel.name AS n ORDER BY n"
            )
            .unwrap()
        ),
        ["x", "y"]
    );
    // Independent property on either side of the relationship arm: the
    // searched relationship's `name` is the candidate property and the
    // pre-bound `r.name` the per-row RHS, so the LIMIT counts only the
    // candidates the predicate keeps (live Neo4j 2026.10.1:
    // SearchWithSingleStageAcceptance "independent property as LHS - rel
    // index" and its mirror both return `cs` alone).
    for predicate in ["rel.name = r.name", "r.name = rel.name"] {
        assert_eq!(
            names(
                &c.query(&format!(
                    "MATCH ()-[r {{name: 'x'}}]->() MATCH ()-[rel]->() \
                     SEARCH rel IN (VECTOR INDEX ridx FOR r.vec \
                     WHERE {predicate} LIMIT 2) RETURN rel.name AS n ORDER BY n"
                ))
                .unwrap()
            ),
            ["x"],
            "`{predicate}`"
        );
    }
    // The entity-set filter's relationship-id branch.
    assert_eq!(
        names(
            &c.query(
                "MATCH ()-[rel:REL {name: 'x'}]->() WITH collect(rel) AS rels \
                 MATCH ()-[candidate:REL]->() \
                 SEARCH candidate IN (VECTOR INDEX ridx FOR [1.0, 0.0] \
                 WHERE candidate IN rels LIMIT 10) RETURN candidate.name AS n ORDER BY n"
            )
            .unwrap()
        ),
        ["x"]
    );
}

/// CIP-271: a vector's `toString`/string-interpolation form is the value
/// constructor, not a `NOT NULL`-suffixed type expression.
#[test]
fn vector_to_string_uses_the_value_constructor() {
    let mut c = common::connection();
    let r = c
        .query("RETURN toString(vector([1, 2], 2, INTEGER8)) AS s")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::String("vector([1, 2], 2, INTEGER8)".into())]]
    );
    let r = c
        .query("RETURN s\"A value of: { vector([1, 2], 2, INTEGER8) }\" AS s")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::String(
            "A value of: vector([1, 2], 2, INTEGER8)".into()
        )]]
    );
}

/// The post-filtering rule: a *plain* SEARCH MATCH shape (the search
/// element alone) keeps the `LIMIT` as a hard candidate window, so its
/// outer `WHERE` — before or after the SEARCH — and the MATCH's label
/// post-filter the window instead of widening it. Probed on live Neo4j
/// 2026.10.1: `LIMIT 2` over Alpha (1950), Beta (2013) with
/// `year < 2000` returns Alpha alone — the deeper Gamma (1995) never
/// enters.
fn post_filter_store() -> relate::Connection {
    let mut c = common::connection();
    c.query("CREATE VECTOR INDEX filmVectors FOR (f:Film) ON f.embedding")
        .unwrap();
    c.query(
        "CREATE (:Film {title: 'Alpha', year: 1950, embedding: [1.0, 0.0]}), \
         (:Film {title: 'Beta', year: 2013, embedding: [0.9, 0.1]}), \
         (:Film {title: 'Gamma', year: 1995, embedding: [0.8, 0.2]}), \
         (:Film {title: 'Delta', year: 1990, embedding: [0.7, 0.3]})",
    )
    .unwrap();
    c
}

#[test]
fn a_plain_search_shape_post_filters_its_outer_where() {
    let mut c = post_filter_store();
    for query in [
        "MATCH (movie:Film) SEARCH movie IN (VECTOR INDEX filmVectors FOR [1.0, 0.0] \
         LIMIT 2) SCORE AS score WHERE movie.year < 2000 RETURN movie.title AS title",
        "MATCH (movie:Film) WHERE movie.year < 2000 \
         SEARCH movie IN (VECTOR INDEX filmVectors FOR [1.0, 0.0] LIMIT 2) \
         SCORE AS score RETURN movie.title AS title",
    ] {
        assert_eq!(
            names(&c.query(query).unwrap()),
            ["Alpha"],
            "the WHERE post-filters the two-candidate window: {query}"
        );
    }
}

/// The label restriction is a post-filter too: a multi-label index yields a
/// candidate the MATCH's own label rejects, and the search does not widen
/// past it (live Neo4j 2026.10.1: FulltextSearchAcceptance [37] "label
/// filter applies after LIMIT" — its LIMIT 1 page holds no Movie at all).
#[test]
fn a_plain_search_shape_post_filters_the_match_label() {
    let mut c = common::connection();
    c.query("CREATE VECTOR INDEX mediaVectors FOR (n:Alpha|Beta) ON n.embedding")
        .unwrap();
    c.query(
        "CREATE (:Alpha {id: 'a1', embedding: [1.0, 0.0]}), \
         (:Beta {id: 'b1', embedding: [0.9, 0.1]}), \
         (:Alpha {id: 'a2', embedding: [0.8, 0.2]})",
    )
    .unwrap();
    assert_eq!(
        names(
            &c.query(
                "MATCH (n:Alpha) SEARCH n IN (VECTOR INDEX mediaVectors FOR [1.0, 0.0] \
                 LIMIT 2) RETURN n.id AS id"
            )
            .unwrap()
        ),
        ["a1"],
        "the Beta candidate consumes a window slot the label filter frees"
    );
}

/// The rejected filter shapes: wrong LHS variable, self-reference,
/// property outside the index's additional set, and incompatible
/// same-property predicates.
#[test]
fn cip_240_search_filter_rejections() {
    let mut c = store();

    for (statement, expected) in [
        (
            "MATCH (m:Person {name: 'a'}) MATCH (p:Person) \
             SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] WHERE m.rating > 7 LIMIT 5) \
             RETURN p.name",
            "WrongVariableForVectorSearchFilter",
        ),
        (
            "MATCH (p:Person) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] \
             WHERE p.rating = p.year LIMIT 5) RETURN p.name",
            "SelfReferencingVectorSearchFilter",
        ),
        (
            "MATCH (p:Person) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] \
             WHERE p.name = 'a' LIMIT 5) RETURN p.name",
            "WrongPropertyForVectorSearchFilter",
        ),
        (
            "MATCH (p:Person) SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] \
             WHERE p.rating > 7 AND p.rating > 8 LIMIT 5) RETURN p.name",
            "InvalidSearchFilterPredicate",
        ),
    ] {
        let error = c.query(statement).expect_err("the filter must be rejected");
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?} in {error}"
        );
    }
}
