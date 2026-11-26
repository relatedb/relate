//! The statement timeout (ADR-0692) on the binding's connection.

use super::*;

#[test]
fn a_statement_past_its_budget_raises_the_timeout_kind_and_the_connection_survives() {
    with_py(|py| {
        let db = new_database(":memory:");
        assert_eq!(db.statement_timeout(py).unwrap(), None);
        db.set_statement_timeout(py, Some(0.2)).unwrap();
        assert_eq!(db.statement_timeout(py).unwrap(), Some(0.2));
        let err = db
            .execute(
                py,
                "UNWIND range(1, 2000000000) AS x RETURN count(x) AS n",
                None,
            )
            .err()
            .expect("the long statement times out");
        assert_eq!(err_attr(&err, py, "kind"), "StatementTimeout");
        assert!(
            err.to_string().contains("query timed out after 200ms"),
            "unexpected message: {err}"
        );
        db.set_statement_timeout(py, None).unwrap();
        assert_eq!(db.statement_timeout(py).unwrap(), None);
        db.execute(py, "RETURN 1 AS n", None).unwrap();
    });
}

#[test]
fn zero_clears_and_a_bad_value_is_a_value_error() {
    with_py(|py| {
        let db = new_database(":memory:");
        db.set_statement_timeout(py, Some(5.0)).unwrap();
        db.set_statement_timeout(py, Some(0.0)).unwrap();
        assert_eq!(db.statement_timeout(py).unwrap(), None);
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            let err = db.set_statement_timeout(py, Some(bad)).unwrap_err();
            assert!(
                err.is_instance_of::<pyo3::exceptions::PyValueError>(py),
                "{bad}"
            );
        }
    });
}
