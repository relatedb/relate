use crate::result::*;
use crate::support::{open_memory, run};
use crate::tabular::*;
use std::ffi::CStr;
use std::ptr;

/// Read `relate_result_plan_json` and parse it — the one copy of the
/// accessor's read, so its owned-string contract is spelled once.
///
/// # Safety
/// `result` must be a live result. The accessor returns NULL when the
/// statement carried no plan; that NULL is a loud failure here, never
/// dereferenced.
unsafe fn result_plan_json(result: *mut relate_query_result) -> serde_json::Value {
    // SAFETY: the caller guarantees a live result; the accessor returns a
    // result-owned NUL-terminated string, or NULL when the statement
    // carried no plan.
    let document = unsafe { relate_result_plan_json(result) };
    assert!(
        !document.is_null(),
        "the result carries a plan document (a plain run returns NULL)"
    );
    // SAFETY: `document` is non-null and NUL-terminated per the accessor's
    // contract.
    let text = unsafe { CStr::from_ptr(document) }
        .to_str()
        .expect("the plan document is UTF-8");
    serde_json::from_str(text).expect("the plan document is JSON")
}

/// `relate_result_query_type` reports the wrapped statement's plan-time
/// type on an EXPLAIN (ADR-0162): `EXPLAIN CREATE (n)` classifies `w`, and
/// EXPLAIN never executes — a follow-up MATCH shows the node was not created.
#[test]
fn ffi_explain_create_reports_write_type_without_creating() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();

        let result = run(conn, "EXPLAIN CREATE (n)");
        let qt = CStr::from_ptr(relate_result_query_type(result))
            .to_str()
            .unwrap();
        assert_eq!(qt, "w");
        relate_destroy_result(result);

        // EXPLAIN must not have executed the CREATE.
        let result = run(conn, "MATCH (n) RETURN n");
        assert_eq!(relate_row_count(result), 0);
        relate_destroy_result(result);

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// The plan document's `warnings` carry the engine's remedy-bearing
/// notifications (ADR-0381 + ADR-0363), joined by operator id: an agent
/// reads the diagnosis and the action beside it.
#[test]
fn ffi_result_plan_json_warnings_carry_the_remedy() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();
        let result = run(
            conn,
            "EXPLAIN MATCH (a:Person), (b:Person) RETURN a.name AS a, b.name AS b",
        );
        let doc = result_plan_json(result);
        // Find the named warning anywhere in the tree.
        fn find_warning<'a>(
            node: &'a serde_json::Value,
            code: &str,
        ) -> Option<&'a serde_json::Value> {
            if let Some(found) = node["warnings"]
                .as_array()
                .and_then(|list| list.iter().find(|warning| warning["code"] == code))
            {
                return Some(found);
            }
            node["children"]
                .as_array()?
                .iter()
                .find_map(|child| find_warning(child, code))
        }
        let warning = find_warning(
            &doc["root"],
            "Neo.ClientNotification.Statement.CartesianProduct",
        )
        .expect("the CartesianProduct warning rides a node");
        assert!(warning["severity"].is_string());
        assert!(
            warning["remedy"].as_str().is_some_and(|r| !r.is_empty()),
            "the warning carries an actionable remedy: {warning}"
        );
        relate_destroy_result(result);
        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// `relate_result_namespace` reports the statement's resolved namespace
/// (ADR-0117 §7): a USE-routed statement resolves to its USE target while
/// the session namespace stays `neo4j` for unrouted statements.
#[test]
fn ffi_result_namespace_reports_resolved_not_session_namespace() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();

        let result = run(conn, "USE system SHOW DATABASES YIELD name RETURN name");
        let ns = CStr::from_ptr(relate_result_namespace(result))
            .to_str()
            .unwrap();
        assert_eq!(ns, "system");
        relate_destroy_result(result);

        // The session namespace is unchanged: an unrouted statement still
        // resolves to neo4j.
        let result = run(conn, "RETURN 1 AS n");
        let ns = CStr::from_ptr(relate_result_namespace(result))
            .to_str()
            .unwrap();
        assert_eq!(ns, "neo4j");
        relate_destroy_result(result);

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// The metadata accessors follow the result-accessor contract: a NULL
/// result yields NULL.
#[test]
fn ffi_result_metadata_accessors_null_safe() {
    // SAFETY: passing NULL exercises the documented null contract.
    unsafe {
        assert!(relate_result_query_type(ptr::null_mut()).is_null());
        assert!(relate_result_namespace(ptr::null_mut()).is_null());
        assert!(relate_result_notifications(ptr::null_mut()).is_null());
    }
}

/// `relate_result_notifications` returns the engine-native notification
/// list (ADR-0363 decision 9): on a store holding one `:Person` and one
/// `:City`, the disconnected `EXPLAIN MATCH` plans as a Cartesian product and
/// carries the `CartesianProduct` entry with its operator association. A
/// plain run derives no notifications and returns the empty array `[]`; the
/// string stays result-owned and readable until the result is destroyed.
#[test]
fn ffi_result_notifications_explain_carries_cartesian_product() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();
        for query in ["CREATE (:Person)", "CREATE (:City)"] {
            let result = run(conn, query);
            assert_eq!(relate_row_count(result), 0);
            relate_destroy_result(result);
        }

        let result = run(conn, "EXPLAIN MATCH (a:Person), (b:City) RETURN a, b");
        let notifications_ptr = relate_result_notifications(result);
        let notifications_text = CStr::from_ptr(notifications_ptr)
            .to_str()
            .unwrap()
            .to_string();
        let notifications: serde_json::Value = serde_json::from_str(&notifications_text).unwrap();
        let entries = notifications
            .as_array()
            .expect("notifications is a JSON array");
        assert_eq!(
            entries.len(),
            1,
            "one CartesianProduct notification: {notifications}"
        );
        assert_eq!(
            entries[0]["code"],
            "Neo.ClientNotification.Statement.CartesianProduct"
        );
        assert_eq!(entries[0]["gql_status"], "03N90");
        assert_eq!(entries[0]["severity"], "INFORMATION");
        assert_eq!(entries[0]["category"], "PERFORMANCE");
        assert!(entries[0]["operator_id"].is_number());

        // Result-owned: the string outlives later work on the connection and
        // stays valid until its own result is destroyed.
        let later_result = run(conn, "RETURN 1 AS n");
        assert_eq!(
            CStr::from_ptr(notifications_ptr).to_str().unwrap(),
            notifications_text
        );
        relate_destroy_result(later_result);
        relate_destroy_result(result);

        let result = run(conn, "MATCH (a:Person), (b:City) RETURN a, b");
        let empty = CStr::from_ptr(relate_result_notifications(result))
            .to_str()
            .unwrap();
        assert_eq!(empty, "[]");
        relate_destroy_result(result);

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// `relate_result_elapsed_ms` reports the engine-stamped statement
/// elapsed (ADR-0221): finite and non-negative on a real statement, `0.0`
/// on a NULL result per the accessor contract.
#[test]
fn ffi_result_elapsed_ms_is_engine_stamped_and_null_safe() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        assert_eq!(relate_result_elapsed_ms(ptr::null_mut()), 0.0);

        let (db, conn) = open_memory();
        let result = run(conn, "RETURN 1 AS n");
        let elapsed = relate_result_elapsed_ms(result);
        assert!(
            elapsed.is_finite() && elapsed >= 0.0,
            "elapsed must be finite and non-negative, got {elapsed}"
        );
        relate_destroy_result(result);

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// The ADR-0491 phase accessors report the engine-stamped split of the
/// elapsed: each finite and non-negative, each `0.0` on a NULL result, and
/// — the ADR-0491 invariant — their sum never exceeds the elapsed total.
#[test]
fn ffi_result_phase_ms_split_the_elapsed_and_are_null_safe() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        assert_eq!(relate_result_parse_ms(ptr::null_mut()), 0.0);
        assert_eq!(relate_result_plan_ms(ptr::null_mut()), 0.0);
        assert_eq!(relate_result_execute_ms(ptr::null_mut()), 0.0);

        let (db, conn) = open_memory();
        let result = run(conn, "RETURN 1 AS n");
        let parse = relate_result_parse_ms(result);
        let plan = relate_result_plan_ms(result);
        let execute = relate_result_execute_ms(result);
        let elapsed = relate_result_elapsed_ms(result);
        for (name, value) in [("parse", parse), ("plan", plan), ("execute", execute)] {
            assert!(
                value.is_finite() && value >= 0.0,
                "{name} must be finite and non-negative, got {value}"
            );
        }
        assert!(
            parse + plan + execute <= elapsed,
            "the phases partition the elapsed window (ADR-0491): \
             parse {parse} + plan {plan} + execute {execute} > elapsed {elapsed}"
        );
        relate_destroy_result(result);

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// `relate_result_plan_json` reports the statement's plan as a
/// `relate.plan/v1` document (LABS-370): the schema identifier, the
/// profiled flag, and the root operator — and NULL on a plain run, which
/// carries no plan.
#[test]
fn ffi_result_plan_json_reports_the_agent_native_document() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();

        // EXPLAIN: the document, profiled false, no runtime metrics.
        let result = run(conn, "EXPLAIN MATCH (n:Person) RETURN n.name AS name");
        let doc = result_plan_json(result);
        assert_eq!(doc["schema"], relate_model::wire::PLAN_SCHEMA);
        assert_eq!(doc["profiled"], false);
        assert_eq!(doc["root"]["operator"], "ProduceResults");
        assert!(
            doc["root"]["actual"].is_null(),
            "an EXPLAIN tree carries no metrics"
        );
        relate_destroy_result(result);

        // PROFILE: profiled true, the root carries its runtime metrics.
        let result = run(conn, "PROFILE MATCH (n:Person) RETURN n.name AS name");
        let doc = result_plan_json(result);
        assert_eq!(doc["profiled"], true);
        assert!(doc["root"]["actual"]["rows"].is_number());
        relate_destroy_result(result);

        // A plain run carries no plan: NULL, never a document (the C
        // convention for absent, like the out-of-range result accessors).
        let result = run(conn, "RETURN 1 AS n");
        assert!(
            relate_result_plan_json(result).is_null(),
            "a statement with no plan returns NULL"
        );
        relate_destroy_result(result);

        relate_disconnect(conn);
        relate_database_close(db);
    }
}
