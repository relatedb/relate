//! The 01N31/01N30 planner-hint notifications' integration tests
//! (statement_notifications' hints module): an index hint no catalog node
//! index can serve, and every join hint (the planner never fulfils one),
//! verified against live Neo4j 2026.10.1 (probed 2026-11-07). Split from
//! `statement_notifications.rs` to keep both files under the 1k ceiling.

mod common;

// `notifications_of_kind` is the shared filter these tests assert through:
// Relate is allowed to warn more than Neo4j (LABS-710), so an exact-list
// assertion would pin the extra coverage as if it were the contract; the
// hint kinds are what these tests pin.
use common::{connection, has_notification, kinds, notifications_of_kind, run};
use relate_model::wire::NotificationKind;

/// The index-hint rule: a hint with no catalog index on its label warns
/// 01N31 with the live text naming the hinted index, an existing covering
/// index suppresses, and each hint of a multi-hint statement warns on its
/// own.
#[test]
fn index_hint_notification_fires_and_suppresses() {
    let mut conn = connection();
    // Fire: no index on :Person(name).
    let explained = conn
        .query("EXPLAIN MATCH (n:Person) USING INDEX n:Person(name) WHERE n.name = 'John' RETURN n")
        .unwrap();
    let fired = notifications_of_kind(&explained, NotificationKind::HintedIndexNotFound);
    assert_eq!(fired.len(), 1, "{:?}", kinds(&explained));
    let notification = fired[0];
    assert_eq!(
        notification.code(),
        "Neo.ClientNotification.Schema.HintedIndexNotFound"
    );
    assert_eq!(notification.gql_status(), "01N31");
    assert_eq!(notification.severity().as_str(), "WARNING");
    assert_eq!(notification.category().as_str(), "HINT");
    assert_eq!(
        notification.description(),
        "warn: hinted index does not exist. Unable to create a plan with \
         'INDEX :Person(name)' because the index does not exist."
    );
    // Suppress: the index exists and covers the hinted property.
    run(&mut conn, "CREATE INDEX hint_ix FOR (n:Person) ON (n.name)");
    let explained = conn
        .query("EXPLAIN MATCH (n:Person) USING INDEX n:Person(name) WHERE n.name = 'John' RETURN n")
        .unwrap();
    assert!(
        !has_notification(&explained, NotificationKind::HintedIndexNotFound),
        "the existing index serves the hint: {:?}",
        kinds(&explained)
    );
    // Suppress: a composite index covering both hinted properties.
    run(
        &mut conn,
        "CREATE INDEX hint_ix2 FOR (n:Person) ON (n.name, n.surname)",
    );
    let explained = conn
        .query(
            "EXPLAIN MATCH (n:Person) USING INDEX n:Person(name, surname) \
         WHERE n.name = 'John' AND n.surname = 'Smith' RETURN n",
        )
        .unwrap();
    assert!(
        !has_notification(&explained, NotificationKind::HintedIndexNotFound),
        "a covering composite index serves the hint: {:?}",
        kinds(&explained)
    );
    // Fire: the index exists on a different property.
    let explained = conn
        .query("EXPLAIN MATCH (n:Person) USING INDEX n:Person(age) WHERE n.age = 30 RETURN n")
        .unwrap();
    assert_eq!(
        notifications_of_kind(&explained, NotificationKind::HintedIndexNotFound).len(),
        1,
        "{:?}",
        kinds(&explained)
    );
    // Each hint warns on its own (live: one row per unfulfilled hint), on
    // a store with no index at all — the same statement's earlier indexes
    // would serve the Person hint.
    let mut conn = connection();
    let explained = conn
        .query(
            "EXPLAIN MATCH (n:Person), (m:Party), (k:Animal) \
         USING INDEX n:Person(name) \
         USING INDEX m:Party(city) \
         USING INDEX k:Animal(species) \
         WHERE n.name = 'John' AND m.city = 'Reykjavik' AND k.species = 'Sloth' \
         RETURN n",
        )
        .unwrap();
    let fired = notifications_of_kind(&explained, NotificationKind::HintedIndexNotFound);
    assert_eq!(fired.len(), 3, "{:?}", kinds(&explained));
    assert_eq!(
        fired[1].description(),
        "warn: hinted index does not exist. Unable to create a plan with \
         'INDEX :Party(city)' because the index does not exist."
    );
}

/// The join-hint rule: the planner is not cost-based and never plans a
/// hint-directed join, so every `USING JOIN ON` warns 01N30 with the
/// live text naming the hinted keys, one row per hint.
#[test]
fn join_hint_notifications_fire_per_hint() {
    let mut conn = connection();
    // One hint with two keys.
    let explained = conn
        .query(
            "EXPLAIN MATCH (person) \
         WITH person, avg(person.weight) AS weight \
         OPTIONAL MATCH (person)-[]->(friend) \
         USING JOIN ON person, friend \
         RETURN *",
        )
        .unwrap();
    let fired = notifications_of_kind(&explained, NotificationKind::JoinHintUnfulfillable);
    assert_eq!(fired.len(), 1, "{:?}", kinds(&explained));
    let notification = fired[0];
    assert_eq!(
        notification.code(),
        "Neo.ClientNotification.Statement.JoinHintUnfulfillableWarning"
    );
    assert_eq!(notification.gql_status(), "01N30");
    assert_eq!(notification.severity().as_str(), "WARNING");
    assert_eq!(notification.category().as_str(), "HINT");
    assert_eq!(
        notification.description(),
        "warn: join hint unfulfillable. Unable to create a plan with \
         'JOIN ON `person`, `friend`'. Try to change the join key(s) or \
         restructure your query."
    );
    // Two hints, one key each: two rows (live: one per hint).
    let explained = conn
        .query(
            "EXPLAIN MATCH (person) \
         WITH person, avg(person.weight) AS weight \
         OPTIONAL MATCH (person)-[]->(friend) \
         USING JOIN ON person \
         USING JOIN ON friend \
         RETURN *",
        )
        .unwrap();
    let fired = notifications_of_kind(&explained, NotificationKind::JoinHintUnfulfillable);
    assert_eq!(fired.len(), 2, "{:?}", kinds(&explained));
    // The statement-scoped merge sorts by code then description, so the
    // two same-code rows arrive in description order (`friend` < `person`).
    assert_eq!(
        fired[0].description(),
        "warn: join hint unfulfillable. Unable to create a plan with \
         'JOIN ON `friend`'. Try to change the join key(s) or restructure \
         your query."
    );
    assert_eq!(
        fired[1].description(),
        "warn: join hint unfulfillable. Unable to create a plan with \
         'JOIN ON `person`'. Try to change the join key(s) or restructure \
         your query."
    );
}

/// Occurrence scoping (ADR-0363 decision 6): an identical hint written
/// twice is two occurrences, never collapsed into one row — the kind's
/// `occurrence_scoped` row column, not a distant list, is what decides it.
#[test]
fn identical_hints_are_reported_once_per_occurrence() {
    let mut conn = connection();
    let explained = conn
        .query(
            "EXPLAIN MATCH (person) \
         WITH person, avg(person.weight) AS weight \
         OPTIONAL MATCH (person)-[]->(friend) \
         USING JOIN ON person \
         USING JOIN ON person \
         RETURN *",
        )
        .unwrap();
    assert_eq!(
        notifications_of_kind(&explained, NotificationKind::JoinHintUnfulfillable).len(),
        2,
        "two identical hints are two occurrences: {:?}",
        kinds(&explained)
    );
}

/// The index-kind rule, live-verified 2026-11-08: a vector index never
/// serves the hint, so the warning still fires; a text index serves it
/// (live suppresses 01N31 through range, text, point and fulltext
/// indexes alike).
#[test]
fn a_vector_index_does_not_serve_but_a_text_index_does() {
    let mut conn = connection();
    run(
        &mut conn,
        "CREATE VECTOR INDEX hint_vec FOR (n:Person) ON (n.embedding) \
         OPTIONS { indexConfig: { `vector.dimensions`: 3, `vector.similarity_function`: 'cosine' } }",
    );
    let explained = conn
        .query("EXPLAIN MATCH (n:Person) USING INDEX n:Person(embedding) WHERE n.embedding = 1 RETURN n")
        .unwrap();
    assert_eq!(
        notifications_of_kind(&explained, NotificationKind::HintedIndexNotFound).len(),
        1,
        "a vector index cannot serve the hint: {:?}",
        kinds(&explained)
    );
    // A text index serves the hint (live: no 01N31).
    run(
        &mut conn,
        "CREATE TEXT INDEX hint_text FOR (n:Person) ON (n.name)",
    );
    let explained = conn
        .query("EXPLAIN MATCH (n:Person) USING INDEX n:Person(name) WHERE n.name CONTAINS 'x' RETURN n")
        .unwrap();
    assert!(
        !has_notification(&explained, NotificationKind::HintedIndexNotFound),
        "a text index serves the hint: {:?}",
        kinds(&explained)
    );
}
