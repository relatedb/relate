//! The FFI bookmark surface (ADR-0502 §7): the engine's commit sequence
//! through both of the ABI's accessors — the auto-commit result's
//! result-owned string and the explicit commit's caller-owned
//! `bookmark_out` out-param.

use crate::abi::relate_status;
use crate::relate_free_string;
use crate::result::*;
use crate::support::{open_memory, run, take_error_json};
use crate::tabular::*;
use relate::{error_info, Error};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::ptr;

/// Read a result's bookmark, NULL -> `None` — the one copy of the
/// result-owned accessor's read these tests share.
///
/// # Safety
/// `result` must be a live result from `run`.
unsafe fn result_bookmark(result: *mut relate_query_result) -> Option<String> {
    // SAFETY: the caller guarantees a live result; the accessor returns a
    // result-owned NUL-terminated string, or NULL when the statement
    // committed nothing.
    unsafe {
        let bookmark = relate_result_bookmark(result);
        if bookmark.is_null() {
            None
        } else {
            Some(
                CStr::from_ptr(bookmark)
                    .to_str()
                    .expect("the bookmark is UTF-8")
                    .to_string(),
            )
        }
    }
}

/// Take a commit's caller-owned `*bookmark_out` string, NULL -> `None`,
/// freeing it — the one copy of the caller-owned half's read.
///
/// # Safety
/// `bookmark` must be null or the string `relate_conn_commit`'s
/// `bookmark_out` out-param handed over.
unsafe fn take_commit_bookmark(bookmark: *mut c_char) -> Option<String> {
    // SAFETY: the caller guarantees a returned string, taken back exactly
    // once through `relate_free_string`.
    unsafe {
        if bookmark.is_null() {
            None
        } else {
            let text = CStr::from_ptr(bookmark)
                .to_str()
                .expect("the bookmark is UTF-8")
                .to_string();
            relate_free_string(bookmark);
            Some(text)
        }
    }
}

/// The `relate:<n>` sequence number of a bookmark — the module's one
/// parse, so every assertion reads the same ADR-0310 form. The engine
/// crate's identical helper is `cfg(test)`-private there, so this is the
/// FFI suite's own test-local parse, not a second public path.
fn sequence(bookmark: Option<String>, label: &str) -> u64 {
    let Some(bookmark) = bookmark else {
        panic!("{label}: expected a bookmark, found none");
    };
    let Some(number) = bookmark.strip_prefix("relate:") else {
        panic!("{label}: not the ADR-0310 form `relate:<n>`: {bookmark}");
    };
    number
        .parse()
        .unwrap_or_else(|_| panic!("{label}: not a sequence number: {bookmark}"))
}

/// Two sequential auto-commit writes return strictly increasing bookmarks
/// through the result accessor, and everything that commits nothing
/// returns the absent form: a read, an EXPLAIN, and an error result.
#[test]
fn ffi_auto_commit_bookmarks_increase_and_reads_are_absent() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();

        let first = run(conn, "CREATE (:Kept)");
        let second = run(conn, "CREATE (:Kept)");
        let first_seq = sequence(result_bookmark(first), "first write");
        let second_seq = sequence(result_bookmark(second), "second write");
        relate_destroy_result(first);
        relate_destroy_result(second);
        assert!(
            second_seq > first_seq,
            "the commit sequence is monotonic: {second_seq} vs {first_seq}"
        );

        // A read-only statement committed nothing: NULL, the documented
        // absent form.
        let read = run(conn, "MATCH (n:Kept) RETURN n");
        assert_eq!(result_bookmark(read), None);
        relate_destroy_result(read);

        // EXPLAIN never executes, so nothing commits.
        let explained = run(conn, "EXPLAIN CREATE (:Kept)");
        assert_eq!(result_bookmark(explained), None);
        relate_destroy_result(explained);

        // A statement failure rides the result (ADR-0502 §2) and carries
        // no bookmark: the statement never committed.
        let bad = std::ffi::CString::new("INVALID QUERY !!!").unwrap();
        let mut failed: *mut relate_query_result = ptr::null_mut();
        assert!(matches!(
            relate_query_params(
                conn,
                bad.as_ptr(),
                ptr::null(),
                &mut failed,
                ptr::null_mut()
            ),
            relate_status::RELATE_OK
        ));
        assert_eq!(relate_result_is_error(failed), 1);
        assert_eq!(
            result_bookmark(failed),
            None,
            "an error result carries no bookmark"
        );
        relate_destroy_result(failed);

        // A NULL result yields the absent form too.
        assert!(relate_result_bookmark(ptr::null_mut()).is_null());

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// The explicit commit hands the same `relate:<n>` form over as a
/// caller-owned string: the in-transaction statement itself carries no
/// bookmark, the commit's sorts after an earlier auto-commit's, and an
/// empty transaction commits nothing (RELATE_OK with a NULL
/// bookmark, error channel untouched).
#[test]
fn ffi_explicit_commit_hands_over_the_bookmark() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();

        let auto = run(conn, "CREATE (:Kept)");
        let auto_seq = sequence(result_bookmark(auto), "auto-commit write");
        relate_destroy_result(auto);

        assert!(matches!(
            relate_conn_begin(conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let in_txn = run(conn, "CREATE (:Kept)");
        assert_eq!(
            result_bookmark(in_txn),
            None,
            "an in-transaction statement commits at the commit call, not at the statement"
        );
        relate_destroy_result(in_txn);

        let mut error_json: *mut c_char = ptr::null_mut();
        let mut bookmark_out: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(conn, &mut bookmark_out, &mut error_json),
            relate_status::RELATE_OK
        ));
        assert!(error_json.is_null(), "the commit succeeded");
        let committed = take_commit_bookmark(bookmark_out);
        let committed_seq = sequence(committed, "transaction commit");
        assert!(
            committed_seq > auto_seq,
            "the transaction's commit sorts after the earlier auto-commit: {committed_seq} vs {auto_seq}"
        );

        // An empty transaction crossed no WAL boundary: RELATE_OK with
        // a NULL bookmark and the error channel untouched.
        assert!(matches!(
            relate_conn_begin(conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let mut error_json: *mut c_char = ptr::null_mut();
        let mut bookmark_out: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(conn, &mut bookmark_out, &mut error_json),
            relate_status::RELATE_OK
        ));
        assert_eq!(
            take_commit_bookmark(bookmark_out),
            None,
            "the empty commit simply committed nothing"
        );
        assert!(error_json.is_null(), "no error on the empty commit");

        // The sequence continues past both commits.
        let after = run(conn, "CREATE (:Kept)");
        let after_seq = sequence(result_bookmark(after), "write after the commits");
        relate_destroy_result(after);
        assert!(after_seq > committed_seq);

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// A commit with no open transaction is a status error through the one
/// error channel: `RELATE_ERROR` with `*bookmark_out` NULL and the
/// engine's own classification for the refusal (`error_info`, not
/// re-derived here) beside the engine message.
#[test]
fn ffi_commit_without_a_transaction_is_a_status_error() {
    // SAFETY: test drives the FFI with handles and out-params it owns.
    unsafe {
        let (db, conn) = open_memory();
        let mut bookmark: *mut c_char = ptr::null_mut();
        let mut error_json: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(conn, &mut bookmark, &mut error_json),
            relate_status::RELATE_ERROR
        ));
        assert!(bookmark.is_null(), "a failed commit hands over no bookmark");
        assert!(!error_json.is_null(), "the failure fills the out-param");
        let shape = take_error_json(error_json);
        let expected = error_info(&Error::semantic("no transaction"));
        assert_eq!(shape["code"], expected.neo4j_code);
        assert_eq!(shape["gql_status"], expected.gql_status);
        assert_eq!(shape["stage"], expected.stage.as_str());
        assert_eq!(shape["retryable"], expected.retryable);
        assert!(
            shape["message"]
                .as_str()
                .is_some_and(|m| m.contains("no transaction")),
            "the engine message rides the shape: {shape}"
        );

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// Opting out of the error detail loses nothing on commit: the status
/// alone distinguishes a failed commit from one that committed nothing.
#[test]
fn ffi_commit_opting_out_of_error_json_keeps_the_discrimination() {
    // SAFETY: test drives the FFI with handles and out-params it owns.
    unsafe {
        let (db, conn) = open_memory();

        // A failing commit (no transaction open) with the detail opted
        // out: the status reports it.
        let mut bookmark: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(conn, &mut bookmark, ptr::null_mut()),
            relate_status::RELATE_ERROR
        ));
        assert!(bookmark.is_null());

        // An empty transaction with the detail opted out: OK, no
        // bookmark — not the failure's status.
        assert!(matches!(
            relate_conn_begin(conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let mut bookmark: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(conn, &mut bookmark, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert_eq!(
            take_commit_bookmark(bookmark),
            None,
            "the empty commit committed nothing"
        );

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// A NULL `bookmark_out` is a refused call: `RELATE_ERROR` with the
/// internal-stage boundary shape, and the transaction NOT committed — the
/// follow-up commit with a real out-param still finds it open and hands
/// over the written transaction's bookmark.
#[test]
fn ffi_commit_null_bookmark_out_is_refused_without_committing() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();
        assert!(matches!(
            relate_conn_begin(conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let created = run(conn, "CREATE (:Kept)");
        relate_destroy_result(created);

        let mut error_json: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(conn, ptr::null_mut(), &mut error_json),
            relate_status::RELATE_ERROR
        ));
        assert!(!error_json.is_null(), "the refusal fills the out-param");
        let shape = take_error_json(error_json);
        assert_eq!(shape["stage"], "internal");
        assert!(
            shape["message"]
                .as_str()
                .is_some_and(|m| m.contains("null out pointer")),
            "the boundary message rides the shape: {shape}"
        );

        // The refused call neither committed nor rolled back: the write's
        // redo is still pending, so the follow-up commit succeeds and
        // crosses the WAL boundary (a NULL bookmark would mean the
        // transaction was gone or empty).
        let mut error_json: *mut c_char = ptr::null_mut();
        let mut bookmark_out: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(conn, &mut bookmark_out, &mut error_json),
            relate_status::RELATE_OK
        ));
        assert!(error_json.is_null(), "the follow-up commit succeeded");
        let committed_seq = sequence(
            take_commit_bookmark(bookmark_out),
            "commit after the refusal",
        );

        // The sequence continues past the refused call's non-commit.
        let after = run(conn, "CREATE (:Kept)");
        let after_seq = sequence(result_bookmark(after), "write after the commit");
        relate_destroy_result(after);
        assert!(after_seq > committed_seq);

        relate_disconnect(conn);
        relate_database_close(db);
    }
}
