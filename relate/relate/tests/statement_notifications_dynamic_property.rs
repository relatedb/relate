//! The 03N95 dynamic-property notification's integration tests
//! (statement_notifications' dynamic_property module): firing, suppression
//! and the entity-kind rule, verified against live Neo4j 2026.10.1
//! (probed 2026-11-07). Split from `statement_notifications.rs` to keep
//! both files under the 1k ceiling.

mod common;

use common::{connection, has_notification, kinds, run};
use relate_model::wire::NotificationKind;

/// The firing and suppression rules: the label predicate may sit in the
/// pattern or the WHERE, a static predicate on the indexed property
/// suppresses, and a label without an index — or no label at all — never
/// fires. Verified against live Neo4j 2026.10.1 (probed 2026-11-07).
#[test]
fn dynamic_property_notification_fires_and_suppresses() {
    let mut conn = connection();
    run(
        &mut conn,
        "CREATE INDEX dyn_prop FOR (p:Person) ON (p.name)",
    );
    // Fire: the label predicate sits in the WHERE.
    let explained = conn
        .query("EXPLAIN MATCH (n) WHERE n['key-' + n.name] = 1 AND n:Person RETURN n")
        .unwrap();
    assert_eq!(kinds(&explained), [NotificationKind::DynamicProperty]);
    let notification = &explained.notifications()[0];
    assert_eq!(
        notification.code(),
        "Neo.ClientNotification.Statement.DynamicProperty"
    );
    assert_eq!(notification.gql_status(), "03N95");
    assert_eq!(notification.severity().as_str(), "INFORMATION");
    assert_eq!(notification.category().as_str(), "PERFORMANCE");
    assert_eq!(
        notification.title(),
        "Queries using dynamic properties will use neither index seeks nor \
         index scans for those properties"
    );
    assert_eq!(
        notification.description(),
        "Using a dynamic property makes it impossible to use an index \
         lookup for this query (indexed label is: (:Person))"
    );
    // Fire: the label predicate sits in the pattern.
    let explained = conn
        .query("EXPLAIN MATCH (n:Person) WHERE n['key-' + n.name] IS NOT NULL RETURN n")
        .unwrap();
    assert_eq!(kinds(&explained), [NotificationKind::DynamicProperty]);
    // Suppress: a static predicate on the indexed property can serve it.
    let explained = conn
        .query(
            "EXPLAIN MATCH (n:Person) WHERE n['key-' + n.name] IS NOT NULL \
             AND n.name IS NOT NULL RETURN n",
        )
        .unwrap();
    assert!(
        !has_notification(&explained, NotificationKind::DynamicProperty),
        "the static predicate keeps the index servable: {:?}",
        explained.notifications()
    );
    // No label predicate: never fires.
    let explained = conn
        .query("EXPLAIN MATCH (n) WHERE n['key-' + n.name] = 1 RETURN n")
        .unwrap();
    assert!(!has_notification(
        &explained,
        NotificationKind::DynamicProperty
    ));
    // A label without an index: never fires (the unknown-label warning is
    // that query's own notification).
    let explained = conn
        .query("EXPLAIN MATCH (n:Unindexed) WHERE n['key-' + n.name] = 1 RETURN n")
        .unwrap();
    assert!(!has_notification(
        &explained,
        NotificationKind::DynamicProperty
    ));
    // Without EXPLAIN no statement-scoped detector runs at all.
    let executed = conn
        .query("MATCH (n) WHERE n['key-' + n.name] = 1 AND n:Person RETURN n")
        .unwrap();
    assert!(executed.notifications().is_empty());
}

/// The relationship-type twin: the type predicate may sit in the pattern
/// or the WHERE, the description names the indexed type, and a static
/// predicate on the indexed property suppresses.
#[test]
fn dynamic_property_notification_covers_relationship_types() {
    let mut conn = connection();
    run(
        &mut conn,
        "CREATE INDEX dyn_rel FOR ()-[r:KNOWS]-() ON (r.prop)",
    );
    let explained = conn
        .query("EXPLAIN MATCH ()-[r]->() WHERE r['key-' + r.prop] = 1 AND r:KNOWS RETURN r")
        .unwrap();
    // The exact-kind list is not asserted: an unknown-property-key warning
    // for the dynamic key's inner read may ride along (the spec harness
    // filters to the expected codes the same way).
    assert!(has_notification(
        &explained,
        NotificationKind::DynamicProperty
    ));
    let dynamic = explained
        .notifications()
        .iter()
        .find(|n| n.kind() == NotificationKind::DynamicProperty)
        .unwrap();
    assert_eq!(
        dynamic.description(),
        "Using a dynamic property makes it impossible to use an index \
         lookup for this query (indexed relationship type is: (:KNOWS))"
    );
    // Suppress: the static predicate on r.prop keeps the type index
    // servable.
    let explained = conn
        .query(
            "EXPLAIN MATCH ()-[r]->() WHERE r['key-' + r.prop] IS NOT NULL \
             AND r.prop IS NOT NULL AND r:KNOWS RETURN r",
        )
        .unwrap();
    assert!(
        !has_notification(&explained, NotificationKind::DynamicProperty),
        "unexpected: {:?}",
        explained.notifications()
    );
}

/// The entity kind belongs to the index, not the variable's spelling: a
/// node variable labeled like an indexed relationship type never selects
/// that index (live Neo4j 2026.10.1 does not cross kinds, probed
/// 2026-11-07), nor the mirror.
#[test]
fn dynamic_property_notification_never_crosses_entity_kinds() {
    let mut conn = connection();
    run(
        &mut conn,
        "CREATE INDEX dyn_kind FOR ()-[r:KNOWS]-() ON (r.prop)",
    );
    // A node variable labeled KNOWS with the relationship-type index.
    run(&mut conn, "CREATE (:KNOWS)");
    let explained = conn
        .query("EXPLAIN MATCH (n:KNOWS) WHERE n['k' + n.prop] = 1 RETURN n")
        .unwrap();
    assert!(
        !has_notification(&explained, NotificationKind::DynamicProperty),
        "the relationship-type index cannot serve a node variable: {:?}",
        explained.notifications()
    );
    // The mirror: a relationship variable typed like an indexed label.
    run(
        &mut conn,
        "CREATE INDEX dyn_kind_node FOR (n:Person) ON (n.name)",
    );
    let explained = conn
        .query("EXPLAIN MATCH ()-[r:Person]->() WHERE r['k' + r.name] = 1 RETURN r")
        .unwrap();
    assert!(
        !has_notification(&explained, NotificationKind::DynamicProperty),
        "the label index cannot serve a relationship variable: {:?}",
        explained.notifications()
    );
}
