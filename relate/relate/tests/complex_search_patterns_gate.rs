//! The `complex-search-patterns` preview gate, ENABLED side (ADR-0371):
//! with `RELATE_PREVIEW=complex-search-patterns` a
//! SEARCH clause's MATCH statement keeps its own pattern shape — several
//! components, multi-hop and variable-length relationships, several bound
//! variables, predicates outside the binding — for a FULLTEXT source
//! exactly as for a VECTOR one (Neo4j's complex-pattern mode).
//!
//! `RELATE_PREVIEW` is process-global and read once per `Database::open`
//! (ADR-0371), so the enabled and closed sides are separate test binaries:
//! `complex_search_patterns_closed.rs` pins the pattern-shape rejections and must
//! never share a process with this file.

mod common;

use relate::Value;

use common::{connection, set_preview_once};

/// Two FULLTEXT indexes over the corpus's shape: node bios and relationship
/// notes, with the small graph the acceptance scenarios search.
fn search_store() -> relate::Connection {
    set_preview_once("complex-search-patterns");
    let mut c = connection();
    c.query(
        "CREATE FULLTEXT INDEX personBios IF NOT EXISTS \
         FOR (p:Person) ON EACH [p.bio]",
    )
    .unwrap();
    c.query(
        "CREATE FULLTEXT INDEX relBios IF NOT EXISTS \
         FOR ()-[r:KNOWS]->() ON EACH [r.note]",
    )
    .unwrap();
    c.query(
        "CREATE (alice:Person {name: 'Alice', bio: 'brave explorer'}), \
                (bob:Person {name: 'Bob', bio: 'quiet scholar'}), \
                (carol:Person {name: 'Carol', bio: 'bold inventor'}), \
                (alice)-[:KNOWS {note: 'brave trusted ally'}]->(bob), \
                (bob)-[:KNOWS {note: 'gentle mentor guide'}]->(carol)",
    )
    .unwrap();
    c
}

fn names(result: &relate::QueryResult) -> Vec<String> {
    result
        .rows
        .iter()
        .map(|row| match &row[0] {
            Value::String(name) => name.to_string(),
            other => panic!("expected a name string, got {other:?}"),
        })
        .collect()
}

#[test]
fn a_second_bound_variable_rides_along() {
    // FulltextSearchWithComplexPatternAcceptance "node full-text search
    // inside a connected pattern with a second bound variable".
    let mut c = search_store();
    let result = c
        .query(
            "MATCH (p:Person)-[:KNOWS]->(friend) \
             SEARCH p IN (FULLTEXT INDEX personBios FOR 'brave' LIMIT 5) \
             RETURN friend.name AS friend",
        )
        .unwrap();
    assert_eq!(names(&result), vec!["Bob".to_string()]);
}

#[test]
fn a_multi_hop_relationship_pattern_searches_its_own_edge() {
    let mut c = search_store();
    let result = c
        .query(
            "MATCH (x)-[r:KNOWS]->(y)-[r2:KNOWS]->(z) \
             SEARCH r IN (FULLTEXT INDEX relBios FOR 'trusted' LIMIT 5) \
             RETURN z.name AS z",
        )
        .unwrap();
    assert_eq!(names(&result), vec!["Carol".to_string()]);
}

#[test]
fn several_components_with_a_cross_component_predicate() {
    // FulltextSearchWithComplexPatternAcceptance "multiple components with
    // cross-component predicate": the second component's bound variable is
    // filtered by a predicate naming the first.
    let mut c = search_store();
    let result = c
        .query(
            "MATCH (seed:Person {name: 'Alice'}), (p:Person) \
             SEARCH p IN (FULLTEXT INDEX personBios FOR 'scholar' LIMIT 5) \
             WHERE seed.name <> p.name \
             RETURN p.name AS person",
        )
        .unwrap();
    assert_eq!(names(&result), vec!["Bob".to_string()]);
}

#[test]
fn a_variable_length_hop_beyond_the_search_binding() {
    let mut c = search_store();
    let result = c
        .query(
            "MATCH (p:Person)-[:KNOWS*1..2]->(target:Person {name: 'Carol'}) \
             SEARCH p IN (FULLTEXT INDEX personBios FOR 'brave' LIMIT 5) \
             RETURN p.name AS p",
        )
        .unwrap();
    assert_eq!(names(&result), vec!["Alice".to_string()]);
}

#[test]
fn the_plain_rule_0_shape_keeps_working() {
    // The opt-in widens acceptance; it never changes the single-node shape.
    let mut c = search_store();
    let result = c
        .query(
            "MATCH (p:Person) \
             SEARCH p IN (FULLTEXT INDEX personBios FOR 'brave' LIMIT 5) \
             RETURN p.name AS p",
        )
        .unwrap();
    assert_eq!(names(&result), vec!["Alice".to_string()]);
}

/// The top-K corpus: one query vector, four films whose similarity falls
/// with their distance from it, and Pacino in all but `Beta` — so the
/// best-scoring film the pattern rejects sits inside the window the LIMIT
/// covers.
fn top_k_store() -> relate::Connection {
    set_preview_once("complex-search-patterns");
    let mut c = connection();
    c.query("CREATE VECTOR INDEX plotVectors FOR (m:Movie) ON m.embedding")
        .unwrap();
    c.query(
        "CREATE (pacino:Actor {name: 'Pacino'}), \
                (alpha:Movie {title: 'Alpha', released: 1972, embedding: [1.0, 0.0]}), \
                (beta:Movie {title: 'Beta', released: 1974, embedding: [0.95, 0.1]}), \
                (gamma:Movie {title: 'Gamma', released: 1975, embedding: [0.9, 0.2]}), \
                (delta:Movie {title: 'Delta', released: 1976, embedding: [0.85, 0.3]}), \
                (pacino)-[:ACTED_IN]->(alpha), \
                (pacino)-[:ACTED_IN]->(gamma), \
                (pacino)-[:ACTED_IN]->(delta)",
    )
    .unwrap();
    c
}

/// SearchWithComplexPatternAcceptance "guaranteed top-K when at least K
/// candidates match the pattern": the LIMIT counts candidates the pattern
/// keeps, so a rejected higher-ranked candidate frees its slot. Live Neo4j
/// 2026.10.1: Alpha, Gamma — Beta (the second-best vector) has no Pacino.
#[test]
fn the_search_limit_counts_candidates_the_pattern_keeps() {
    let mut c = top_k_store();
    let result = c
        .query(
            "MATCH (movie:Movie)<-[:ACTED_IN]-({name: 'Pacino'}) \
             SEARCH movie IN (VECTOR INDEX plotVectors FOR [1.0, 0.0] LIMIT 2) \
             SCORE AS score \
             RETURN movie.title AS title ORDER BY score DESC",
        )
        .unwrap();
    assert_eq!(
        names(&result),
        vec!["Alpha".to_string(), "Gamma".to_string()]
    );
}

/// "disconnected components with cross-component predicate": the MATCH's
/// own WHERE rejects a candidate the same way the pattern does — Alpha is
/// released in 1972 and Gamma takes its slot. Live Neo4j 2026.10.1:
/// Beta, Gamma.
#[test]
fn a_cross_component_predicate_does_not_shrink_the_top_k() {
    let mut c = top_k_store();
    let result = c
        .query(
            "MATCH (seed:Movie {title: 'Alpha'}), (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX plotVectors FOR [1.0, 0.0] LIMIT 2) \
             SCORE AS score \
             WHERE movie.released > 1972 \
             RETURN movie.title AS title ORDER BY score DESC",
        )
        .unwrap();
    assert_eq!(
        names(&result),
        vec!["Beta".to_string(), "Gamma".to_string()]
    );
}

/// A LIMIT-0 clause keeps nothing and terminates immediately: with a
/// pre-bound binding under a complex pattern the growth loop's base was
/// zero, so the window never widened and the loop spun forever (the
/// round-2 High). The shape has no live oracle (42I70 rejects two bound
/// variables); the CIP's LIMIT-0 reading is the authority: no candidate
/// is kept.
#[test]
fn a_limit_zero_search_terminates_with_no_rows() {
    let mut c = top_k_store();
    let result = c
        .query(
            "MATCH (q:Movie {title: 'Alpha'}), (seed:Movie {title: 'Gamma'}) \
             WITH q, seed \
             MATCH (seed)-[r:ACTED_IN]->(other:Movie) \
             SEARCH seed IN (VECTOR INDEX plotVectors FOR q.embedding LIMIT 0) \
             RETURN seed.title AS title",
        )
        .unwrap();
    assert_eq!(names(&result), Vec::<String>::new());
}

/// "pattern with a predicate on a non-search element": the FOR reads the
/// sibling component's embedding, so the search runs after the pattern and
/// the same top-K guarantee applies to the surviving bindings (the shape
/// has no live oracle — 42I70 rejects two bound variables; the CIP's
/// top-K rule is the authority).
#[test]
fn a_deferred_search_counts_only_surviving_candidates() {
    set_preview_once("complex-search-patterns");
    let mut c = connection();
    c.query("CREATE VECTOR INDEX plotVectors FOR (m:Movie) ON m.embedding")
        .unwrap();
    c.query(
        "CREATE (pacino:Actor {name: 'Pacino', embedding: [1.0, 0.0]}), \
                (seed:Movie {title: 'Seed', embedding: [1.0, 0.0]}), \
                (beta:Movie {title: 'Beta', embedding: [0.95, 0.1]}), \
                (gamma:Movie {title: 'Gamma', embedding: [0.9, 0.2]}), \
                (delta:Movie {title: 'Delta', embedding: [0.85, 0.3]}), \
                (pacino)-[:ACTED_IN]->(gamma), \
                (pacino)-[:ACTED_IN]->(delta)",
    )
    .unwrap();
    // The source reads `actor`, a variable this same MATCH binds, so the
    // pattern matches first and each row's binding is filtered against
    // its own candidate list (the deferred path; live Neo4j rejects the
    // two-bound-variable shape with 42I70, so the CIP's top-K rule is
    // the authority here). Both movies have a row, so both survive the
    // pattern -- but the LIMIT counts surviving candidates: LIMIT 1
    // keeps only the best-ranked survivor, Gamma.
    let result = c
        .query(
            "MATCH (movie:Movie)<-[:ACTED_IN]-(actor {name: 'Pacino'}) \
             SEARCH movie IN (VECTOR INDEX plotVectors FOR actor.embedding LIMIT 1) \
             SCORE AS score \
             RETURN movie.title AS title ORDER BY score DESC",
        )
        .unwrap();
    assert_eq!(names(&result), vec!["Gamma".to_string()]);
}

/// The relationship-index corpus: three ACTED_IN edges whose similarity
/// falls with their distance from the query vector, the middle one with a
/// born-1950 actor (`born < 1940` rejects it).
fn relationship_top_k_store() -> relate::Connection {
    set_preview_once("complex-search-patterns");
    let mut c = connection();
    c.query("CREATE VECTOR INDEX relVectors FOR ()-[r:ACTED_IN]->() ON r.vector")
        .unwrap();
    c.query(
        "CREATE (old:Actor {name: 'Old', born: 1930}), \
                (young:Actor {name: 'Young', born: 1950}), \
                (one:Movie {title: 'One'}), \
                (two:Movie {title: 'Two'}), \
                (three:Movie {title: 'Three'}), \
                (old)-[:ACTED_IN {name: 'old-one', vector: [1.0, 0.0]}]->(one), \
                (young)-[:ACTED_IN {name: 'young-two', vector: [0.9, 0.1]}]->(two), \
                (old)-[:ACTED_IN {name: 'old-three', vector: [0.8, 0.2]}]->(three)",
    )
    .unwrap();
    c
}

/// "relationship search with a predicate on a pattern node": the
/// relationship candidate arm widens its window the same way — young-two
/// ranks between the two old-one/old-three candidates and is filtered out
/// by `a.born < 1940`. Live Neo4j 2026.10.1: old-one, old-three.
#[test]
fn a_relationship_predicate_does_not_shrink_the_top_k() {
    let mut c = relationship_top_k_store();
    let result = c
        .query(
            "MATCH (a:Actor)-[rel:ACTED_IN]->(:Movie) WHERE a.born < 1940 \
             SEARCH rel IN (VECTOR INDEX relVectors FOR [1.0, 0.0] LIMIT 2) \
             SCORE AS score \
             RETURN rel.name AS rel ORDER BY score DESC",
        )
        .unwrap();
    assert_eq!(
        names(&result),
        vec!["old-one".to_string(), "old-three".to_string()]
    );
}

/// "SEARCH with undirected relationship should preserve cardinality of the
/// original MATCH": an undirected edge pattern matches one relationship
/// both ways round, and the LIMIT counts the relationship, not its rows —
/// so the window advances past the two rejected candidates and both rows
/// of old-three ride along. Live Neo4j 2026.10.1: old-three twice.
#[test]
fn an_undirected_match_keeps_every_row_of_a_kept_candidate() {
    let mut c = relationship_top_k_store();
    let result = c
        .query(
            "MATCH (start)-[rel:ACTED_IN]-(end) \
             SEARCH rel IN (VECTOR INDEX relVectors FOR [1.0, 0.0] LIMIT 1) \
             WHERE rel.name = 'old-three' RETURN rel.name AS rel",
        )
        .unwrap();
    assert_eq!(
        names(&result),
        vec!["old-three".to_string(), "old-three".to_string()]
    );
}
