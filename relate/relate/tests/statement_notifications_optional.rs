//! The redundant-`OPTIONAL` detectors: `03N62` for an `OPTIONAL CALL { … }`
//! whose body projects no columns (a *unit* subquery), and `03N61` for an
//! `OPTIONAL CALL` over a procedure that declares no return columns (a
//! *void* procedure). Both triggers and both texts were read off live Neo4j
//! 2026.10.1 (`gql_status_objects`, `EXPLAIN`).

mod common;

use common::{connection, has_notification, notifications_of_kind};
use relate_model::wire::NotificationKind;

/// `OPTIONAL CALL { … }` over a body with no `RETURN` is a unit subquery:
/// the `OPTIONAL` adds nothing, so 03N62 fires — in both the scoped and the
/// unscoped spelling. Without `OPTIONAL` it stays silent, and a body that
/// projects columns is never a unit subquery.
#[test]
fn redundant_optional_subquery_fires_only_for_a_unit_body() {
    let mut conn = connection();

    let scoped = conn
        .query("EXPLAIN OPTIONAL CALL () { CREATE(:A) }")
        .unwrap();
    assert!(has_notification(
        &scoped,
        NotificationKind::RedundantOptionalSubquery
    ));

    let unscoped = conn.query("EXPLAIN OPTIONAL CALL { CREATE(:A) }").unwrap();
    assert!(has_notification(
        &unscoped,
        NotificationKind::RedundantOptionalSubquery
    ));

    let without_optional = conn.query("EXPLAIN CALL () { CREATE(:A) }").unwrap();
    assert!(!has_notification(
        &without_optional,
        NotificationKind::RedundantOptionalSubquery
    ));

    let projects = conn
        .query("EXPLAIN OPTIONAL CALL () { CREATE(:A) RETURN 1 AS x }")
        .unwrap();
    assert!(!has_notification(
        &projects,
        NotificationKind::RedundantOptionalSubquery
    ));
}

/// `OPTIONAL CALL` over a procedure with no return columns is redundant, so
/// 03N61 fires. A procedure that yields a column is not, and dropping
/// `OPTIONAL` removes the notification entirely.
#[test]
fn redundant_optional_procedure_fires_only_for_a_void_procedure() {
    let mut conn = connection();

    let void = conn
        .query("EXPLAIN OPTIONAL CALL db.createLabel('A')")
        .unwrap();
    let fired = notifications_of_kind(&void, NotificationKind::RedundantOptionalProcedure);
    assert_eq!(fired.len(), 1);
    // The rendered call drops the argument, exactly as live renders it.
    assert!(
        fired[0]
            .description()
            .contains("`CALL db.createLabel()` is a void procedure"),
        "unexpected description: {}",
        fired[0].description()
    );

    let without_optional = conn.query("EXPLAIN CALL db.createLabel('A')").unwrap();
    assert!(!has_notification(
        &without_optional,
        NotificationKind::RedundantOptionalProcedure
    ));

    let yielding = conn.query("EXPLAIN OPTIONAL CALL db.labels()").unwrap();
    assert!(!has_notification(
        &yielding,
        NotificationKind::RedundantOptionalProcedure
    ));
}
