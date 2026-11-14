//! Execution-phase failures are reported at the first PULL or DISCARD
//! (ADR-0319); failures before execution still fail the RUN.

use crate::packstream::Pack;
use crate::requests::Message;
use crate::results::fails_run;
use crate::testutil::{
    assert_ignored, connected_client, engine_error, failure_map, field, read_msg, read_records,
    send_begin, send_pull, send_reset, send_run, success_map,
};

const DIVIDE_BY_ZERO: &str = "UNWIND [1, 0] AS x RETURN 10 / x AS v";

/// D-49: RUN answers SUCCESS with the planned columns and `ANY` types; the
/// PULL answers the FAILURE with no RECORDs; the session is failed until
/// RESET.
#[test]
fn a_runtime_error_is_reported_at_pull() {
    let mut client = connected_client();
    send_run(&mut client, DIVIDE_BY_ZERO);
    let run = Pack::Map(success_map(read_msg(&mut client)));
    assert_eq!(
        run.get("fields"),
        Some(&Pack::List(vec![Pack::String("v".into())]))
    );
    assert_eq!(
        run.get("columnTypes"),
        Some(&Pack::List(vec![Pack::String("ANY".into())]))
    );
    assert!(matches!(run.get("t_first"), Some(Pack::Int(_))));
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    let (records, failure) = read_records(&mut client);
    assert!(records.is_empty());
    let failure = failure_map(failure);
    assert_eq!(field(&failure, "message"), engine_error(DIVIDE_BY_ZERO));
    send_run(&mut client, "RETURN 1 AS x");
    assert_ignored(&mut client, "a RUN after a failed result");
    send_reset(&mut client);
    success_map(read_msg(&mut client));
}

/// DISCARD reports a deferred failure the same way, and a constraint
/// violation (an execution-phase failure) defers too.
#[test]
fn discard_reports_a_deferred_constraint_violation() {
    let mut client = connected_client();
    for setup in [
        "CREATE CONSTRAINT u FOR (n:U) REQUIRE n.k IS UNIQUE",
        "CREATE (:U {k: 1})",
    ] {
        send_run(&mut client, setup);
        success_map(read_msg(&mut client));
        send_pull(&mut client, Message::Discard, vec![("n", Pack::Int(-1))]);
        success_map(read_msg(&mut client));
    }
    send_run(&mut client, "CREATE (:U {k: 1})");
    let run = Pack::Map(success_map(read_msg(&mut client)));
    assert_eq!(run.get("fields"), Some(&Pack::List(vec![])));
    send_pull(&mut client, Message::Discard, vec![("n", Pack::Int(-1))]);
    failure_map(read_msg(&mut client));
}

/// Failures before execution still fail the RUN: syntax, an unknown
/// procedure, an unknown variable (semantic validation).
#[test]
fn compile_time_failures_still_fail_the_run() {
    for cypher in ["RETURN", "CALL dbms.noSuchProc()", "RETURN nope AS x"] {
        let mut client = connected_client();
        send_run(&mut client, cypher);
        let failure = failure_map(read_msg(&mut client));
        assert_eq!(field(&failure, "message"), engine_error(cypher), "{cypher}");
        send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
        assert_ignored(&mut client, "a PULL after a failed RUN");
    }
}

/// In a transaction the engine fails the transaction with the statement:
/// a RUN sent before the PULL fails with `rollback required`.
#[test]
fn a_deferred_failure_fails_the_transaction() {
    let mut client = connected_client();
    send_begin(&mut client, vec![]);
    success_map(read_msg(&mut client));
    send_run(&mut client, DIVIDE_BY_ZERO);
    let run = Pack::Map(success_map(read_msg(&mut client)));
    assert_eq!(run.get("qid"), Some(&Pack::Int(0)));
    send_run(&mut client, "RETURN 1 AS x");
    let failure = failure_map(read_msg(&mut client));
    assert!(
        field(&failure, "message").contains("transaction failed; rollback required"),
        "{failure:?}"
    );
    send_reset(&mut client);
    success_map(read_msg(&mut client));
}

/// The classification: compile-class variants fail the RUN even when the
/// executor raised them; every other variant defers.
#[test]
fn compile_class_variants_fail_the_run_and_the_rest_defer() {
    use relate::Error;
    for e in [
        Error::syntax("x"),
        Error::procedure_not_found("p"),
        Error::parameter_missing("x"),
        // ADR-0489: a DDL argument rejection is validated before any write.
        Error::argument_error("InvalidIndexConfig"),
    ] {
        assert!(fails_run(&e), "{e:?}");
    }
    for e in [
        Error::semantic("x"),
        Error::ty("x"),
        Error::execution("x"),
        Error::conflict(),
        Error::capacity(2, 1),
        Error::internal("x"),
        Error::storage("x"),
        Error::io("x"),
    ] {
        assert!(!fails_run(&e), "{e:?}");
    }
}
