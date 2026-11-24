//! The SEARCH clause's unified window (ADR-0678): SKIP and LIMIT page over
//! the entities the whole MATCH statement keeps — for every pattern shape,
//! both index kinds — plus the inner WHERE's entity-membership predicates
//! and the still-populating rejection of an index the open transaction
//! created. The complex-pattern shapes themselves are pinned in
//! `search_complex_patterns.rs`; the per-property filter forms in
//! `cip_240_search_filters.rs`.

mod common;

use common::{connection, err_text, first_column_strings};

use relate::Value;

/// The corpus the acceptance suites' claim rows use: a two-label vector
/// index whose best raw hits are the two distractors, and a constrained
/// subset (label, property, negation) whose best candidates are the two
/// candidates.
fn claim_store() -> relate::Connection {
    let mut c = connection();
    c.query(
        "CREATE VECTOR INDEX claimNodes FOR (n:Movie|Other) ON n.embedding \
         OPTIONS {indexConfig: {`vector.dimensions`: 2, \
         `vector.similarity_function`: 'cosine', \
         `vector.quantization.type`: 'none'}}",
    )
    .unwrap();
    c.query(
        "CREATE (a:Other {name: 'distractor1', selected: false, embedding: [1.0, 0.0]}), \
                (b:Other {name: 'distractor2', selected: false, embedding: [0.99, 0.01]}), \
                (c:Movie:Selected {name: 'candidate1', selected: true, embedding: [0.8, 0.6]}), \
                (d:Movie:Selected {name: 'candidate2', selected: true, embedding: [0.6, 0.8]})",
    )
    .unwrap();
    c
}

/// A score read inside a correlated subquery body is still a score
/// conjunct (the window split's whole-tree classification): it rides
/// the SEARCH clause's post-filter with the score attached — on the
/// deferred shape the scope-level classification would leave it in the
/// pattern-side WHERE, where the probe row carries no score binding at
/// all (an UndefinedVariable where the query should run).
#[test]
fn a_score_read_inside_a_subquery_body_post_filters() {
    let mut c = connection();
    c.query("CREATE VECTOR INDEX plotVectors FOR (m:Movie) ON m.embedding")
        .unwrap();
    c.query(
        "CREATE (a:Movie {title: 'A', released: 2000, embedding: [1.0, 0.0]}), \
                (b:Movie {title: 'B', released: 1200, embedding: [0.5, 0.866]}), \
                (e:Movie {title: 'E', released: 1500})",
    )
    .unwrap();
    // The source reads a pattern variable (seed), so the clause runs the
    // deferred shape; the EXISTS body reads the score per kept row: A
    // (score 1.0, threshold 2000) finds no movie released later and
    // drops, B
    // (score 0.5, threshold 1000) finds E and keeps.
    let titles = first_column_strings(
        &c.query(
            "MATCH (seed:Movie {title: 'A'}), (m:Movie) \
             SEARCH m IN (VECTOR INDEX plotVectors FOR seed.embedding LIMIT 3) \
             SCORE AS score \
             WHERE EXISTS { MATCH (q:Movie) WHERE q.released > score * 2000 } \
             RETURN m.title AS title",
        )
        .unwrap(),
    );
    assert_eq!(
        titles,
        vec!["B".to_string()],
        "the score read inside the body post-filters per kept row; A's \
         threshold excludes every movie, B's admits E"
    );
}

/// The planner's MATCH WHERE policy covers both halves of a split outer
/// WHERE (ADR-0678): an aggregation inside the score half — the conjuncts
/// that ride the SEARCH clause — is rejected exactly as one in the
/// pattern half is, never silently planned.
#[test]
fn an_aggregate_in_the_score_half_of_the_where_is_rejected() {
    let mut c = connection();
    c.query("CREATE VECTOR INDEX relVectors FOR ()-[r:TOW]->() ON r.vec")
        .unwrap();
    c.query(
        "CREATE (d:Deck)-[:TOW {name: 'near', vec: [1.0, 0.0]}]->(h:Hull), \
                (h)-[:TOW {name: 'far', vec: [0.0, 1.0]}]->(d)",
    )
    .unwrap();
    let err = c
        .query(
            "MATCH ()-[rel:TOW]->() \
             SEARCH rel IN (VECTOR INDEX relVectors FOR [1.0, 0.0] LIMIT 2) \
             SCORE AS score WHERE score > max(1, 2) \
             RETURN rel.name AS name",
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("InvalidAggregation"),
        "unexpected error: {err}"
    );
}

/// The window split's score half (ADR-0678): a `SCORE AS`-referencing
/// WHERE conjunct post-filters the kept rows — it does not restrict the
/// candidate window, because the score is a product of the ranking
/// itself. `top` ranks first; a LIMIT 2 window keeps the two best raw
/// candidates and the score predicate then drops the one that fails.
#[test]
fn a_score_predicate_post_filters_the_window() {
    let mut c = connection();
    c.query("CREATE VECTOR INDEX relVectors FOR ()-[r:TOW]->() ON r.vec")
        .unwrap();
    c.query(
        "CREATE (d:Deck)-[:TOW {name: 'near', vec: [1.0, 0.0]}]->(h:Hull), \
                (h)-[:TOW {name: 'far', vec: [0.0, 1.0]}]->(d)",
    )
    .unwrap();
    let names = first_column_strings(
        &c.query(
            "MATCH ()-[rel:TOW]->() \
             SEARCH rel IN (VECTOR INDEX relVectors FOR [1.0, 0.0] LIMIT 2) \
             SCORE AS score WHERE score < 0.9 \
             RETURN rel.name AS name",
        )
        .unwrap(),
    );
    assert_eq!(
        names,
        vec!["far".to_string()],
        "the LIMIT window keeps the two best raw candidates; the score \
         predicate drops the top-scored one without widening"
    );
    // The entity side of the same WHERE still restricts the window: a
    // name predicate pages over the entities that satisfy it.
    let names = first_column_strings(
        &c.query(
            "MATCH ()-[rel:TOW]->() \
             SEARCH rel IN (VECTOR INDEX relVectors FOR [1.0, 0.0] LIMIT 1) \
             WHERE rel.name = 'far' \
             RETURN rel.name AS name",
        )
        .unwrap(),
    );
    assert_eq!(
        names,
        vec!["far".to_string()],
        "an entity predicate restricts the candidates the window pages over"
    );
}

/// The MATCH statement's own constraints — a label, a property predicate,
/// a conjunction label — restrict the candidate set before the window, so
/// LIMIT counts the constrained entities, not raw hits.
#[test]
fn match_constraints_restrict_candidates_before_the_window() {
    let mut c = claim_store();
    for (pattern, expected) in [
        ("(n:Movie)", vec!["candidate1", "candidate2"]),
        ("(n {selected: true})", vec!["candidate1", "candidate2"]),
        ("(n:Movie&Selected)", vec!["candidate1", "candidate2"]),
        ("(n:!Other)", vec!["candidate1", "candidate2"]),
    ] {
        let query = format!(
            "MATCH {pattern} \
             SEARCH n IN (VECTOR INDEX claimNodes FOR [1.0, 0.0] LIMIT 2) \
             RETURN n.name AS name"
        );
        let mut names = first_column_strings(&c.query(&query).unwrap());
        names.sort();
        assert_eq!(
            names, expected,
            "the window pages over the entities {pattern} keeps"
        );
    }
}

/// A constraint a second MATCH states binds the entity before the search:
/// the pre-bound value is the only candidate that can survive, however
/// deep it sits in the raw ranking.
#[test]
fn a_pre_bound_binding_is_the_only_candidate() {
    let mut c = connection();
    c.query("CREATE VECTOR INDEX deep FOR (n:N) ON n.embedding")
        .unwrap();
    c.query(
        "CREATE (:N {name: 'far', embedding: [0.2, 0.98]}), \
                (:N {name: 'near1', embedding: [1.0, 0.0]}), \
                (:N {name: 'near2', embedding: [0.99, 0.01]})",
    )
    .unwrap();
    // `far` ranks third of three: a raw LIMIT 1 window never holds it, and
    // the pre-bound join must still return it.
    let names = first_column_strings(
        &c.query(
            "MATCH (n:N {name: 'far'}) WITH n \
             MATCH (n) SEARCH n IN (VECTOR INDEX deep FOR [1.0, 0.0] LIMIT 1) \
             RETURN n.name AS name",
        )
        .unwrap(),
    );
    assert_eq!(names, vec!["far".to_string()]);
}

/// The fulltext SKIP window pages over surviving entities exactly as the
/// LIMIT does (the complex-pattern suite's "SKIP and LIMIT apply to the
/// entities satisfying the pattern" row).
#[test]
fn fulltext_skip_and_limit_page_over_survivors() {
    let mut c = connection();
    c.query("CREATE FULLTEXT INDEX personBios FOR (p:Person) ON EACH [p.bio]")
        .unwrap();
    c.query(
        "CREATE (alice:Person {name: 'Alice', bio: 'quiet scholar'}), \
                (bob:Person {name: 'Bob', bio: 'quiet scholar'}), \
                (carol:Person {name: 'Carol', bio: 'quiet scholar'})",
    )
    .unwrap();
    let top = first_column_strings(
        &c.query(
            "MATCH (p:Person) SEARCH p IN (FULLTEXT INDEX personBios FOR 'quiet scholar' \
             LIMIT 1) RETURN p.name AS name",
        )
        .unwrap(),
    );
    let after_skip = first_column_strings(
        &c.query(
            "MATCH (p:Person) SEARCH p IN (FULLTEXT INDEX personBios FOR 'quiet scholar' \
             SKIP 1 LIMIT 1) RETURN p.name AS name",
        )
        .unwrap(),
    );
    assert_eq!(top.len(), 1, "the LIMIT window keeps one survivor");
    assert_eq!(
        after_skip.len(),
        1,
        "SKIP 1 consumes the first survivor, LIMIT keeps the next"
    );
    assert_ne!(top, after_skip, "the SKIP window advanced the page");
    // A SKIP past every survivor pages nothing out — the OPTIONAL row
    // still rides through, its binding null (the suite's "OPTIONAL MATCH
    // where SKIP removes every hit preserves the row").
    let result = c
        .query(
            "OPTIONAL MATCH (p:Person) SEARCH p IN (FULLTEXT INDEX personBios \
             FOR 'quiet scholar' SKIP 3 LIMIT 1) RETURN p.name AS name",
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1, "the unmatched input row survives");
    assert_eq!(
        result.rows[0][0],
        relate::Value::Null,
        "the binding is null"
    );
}

/// The inner WHERE serves FULLTEXT sources and both membership spellings:
/// the entity-set form and its id twin, a correlated `COLLECT` subquery on
/// the right-hand side included.
#[test]
fn fulltext_inner_where_entity_membership_filters() {
    let mut c = connection();
    c.query("CREATE FULLTEXT INDEX personBios FOR (p:Person) ON EACH [p.bio]")
        .unwrap();
    c.query(
        "CREATE (:Person {name: 'Alice', bio: 'brave explorer'}), \
                (:Person {name: 'Bob', bio: 'brave scholar'})",
    )
    .unwrap();
    // A full-text source's DDL declares no additional-properties set, so
    // its inner WHERE reads the entity's own properties (the vector
    // source's WITH-set contract does not apply).
    let names = first_column_strings(
        &c.query(
            "MATCH (p:Person) \
             SEARCH p IN (FULLTEXT INDEX personBios FOR 'brave' \
             WHERE p.name = 'Bob' LIMIT 10) \
             RETURN p.name AS name",
        )
        .unwrap(),
    );
    assert_eq!(names, vec!["Bob".to_string()], "a property predicate");

    let names = first_column_strings(
        &c.query(
            "MATCH (candidate:Person) WHERE candidate.name = 'Bob' \
             WITH collect(candidate) AS candidates \
             MATCH (p:Person) \
             SEARCH p IN (FULLTEXT INDEX personBios FOR 'brave' \
             WHERE p IN candidates LIMIT 10) \
             RETURN p.name AS name",
        )
        .unwrap(),
    );
    assert_eq!(
        names,
        vec!["Bob".to_string()],
        "the entity-set match filter"
    );

    let names = first_column_strings(
        &c.query(
            "MATCH (candidate:Person) WHERE candidate.name = 'Bob' \
             WITH collect(id(candidate)) AS candidates \
             MATCH (p:Person) \
             SEARCH p IN (FULLTEXT INDEX personBios FOR 'brave' \
             WHERE id(p) IN candidates LIMIT 10) \
             RETURN p.name AS name",
        )
        .unwrap(),
    );
    assert_eq!(names, vec!["Bob".to_string()], "the id-membership twin");

    // The id form's right-hand side may be a correlated COLLECT subquery:
    // for each Alice row the filter keeps only the entities whose note
    // starts like hers (SearchAcceptance "SEARCH WHERE with a correlated
    // COLLECT subquery").
    let names = first_column_strings(
        &c.query(
            "MATCH (x:Person {name: 'Alice'}) \
             MATCH (p:Person) \
             SEARCH p IN (FULLTEXT INDEX personBios FOR 'brave' \
             WHERE id(p) IN COLLECT { \
                 MATCH (s:Person) \
                 WHERE left(toLower(s.name), 1) = left(toLower(x.name), 1) \
                 RETURN id(s) \
             } LIMIT 10) \
             RETURN p.name AS name",
        )
        .unwrap(),
    );
    assert_eq!(
        names,
        vec!["Alice".to_string()],
        "a correlated subquery evaluates per row"
    );
}

/// An index created inside the open transaction is populating: a SEARCH
/// over it fails with the transient rejection until the transaction ends
/// (ADR-0678; the acceptance suite's POPULATING row). The failed statement
/// marks the transaction for rollback, so the pin rolls it back and
/// re-creates the index outside a transaction for the committed half.
#[test]
fn an_index_created_in_the_open_transaction_is_populating() {
    let mut c = connection();
    c.begin().unwrap();
    c.query("CREATE VECTOR INDEX populating FOR (n:Label) ON n.embedding")
        .unwrap();
    let error = err_text(c.query(
        "MATCH (n:Label) SEARCH n IN (VECTOR INDEX populating FOR [1.0, 0.0] LIMIT 5) \
         RETURN n.prop AS prop",
    ));
    assert!(
        error.contains("IndexStillPopulating"),
        "the in-transaction create is not scannable: {error}"
    );
    c.rollback().unwrap();
    // Committed (an autocommit create), the same statement runs: an empty
    // graph, an empty answer.
    c.query("CREATE VECTOR INDEX populating IF NOT EXISTS FOR (n:Label) ON n.embedding")
        .unwrap();
    let result = c
        .query(
            "MATCH (n:Label) SEARCH n IN (VECTOR INDEX populating FOR [1.0, 0.0] LIMIT 5) \
             RETURN n.prop AS prop",
        )
        .unwrap();
    assert_eq!(result.rows.len(), 0, "the committed index is searchable");
    // An IF NOT EXISTS create over the committed index records no create,
    // so the index stays searchable in the same transaction.
    c.begin().unwrap();
    c.query("CREATE VECTOR INDEX populating IF NOT EXISTS FOR (n:Label) ON n.embedding")
        .unwrap();
    let result = c
        .query(
            "MATCH (n:Label) SEARCH n IN (VECTOR INDEX populating FOR [1.0, 0.0] LIMIT 5) \
             RETURN n.prop AS prop",
        )
        .unwrap();
    assert_eq!(
        result.rows.len(),
        0,
        "the pre-committed index stays scannable"
    );
    c.commit().unwrap();
}

// Pre-bound binds: a null binding joins no candidate, and a non-null one
// pages the unified window over the entities it can keep (ADR-0678).
#[test]
fn a_null_pre_bound_binding_matches_nothing() {
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

    // Pre-bound rows join their own value however deep it ranks: each
    // incoming row's window pages over the entities its pre-bound
    // binding can keep — one — so `beta`'s row returns `beta` even
    // though `alpha` outranks it (the corpus's "pre-bound relationship
    // search with several incoming relationships"; the unified window,
    // ADR-0678 — the 2026.10.1 hard-window split is retired).
    let r = c
        .query(
            "MATCH ()-[r:TOW]->() MATCH ()-[r]->() \
             SEARCH r IN (VECTOR INDEX ridx FOR [1.0, 0.0] LIMIT 1) RETURN r.name AS name",
        )
        .unwrap();
    let mut names: Vec<String> = r
        .rows
        .into_iter()
        .map(|row| match &row[0] {
            Value::String(name) => name.to_string(),
            other => panic!("expected a name, got {other:?}"),
        })
        .collect();
    names.sort();
    assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
}
