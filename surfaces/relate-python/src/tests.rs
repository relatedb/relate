use super::*;
use crate::convert::{
    cypher_value_to_py, date_to_py, duration_to_py, py_params_to_cypher, py_to_cypher_value,
    py_type_name,
};
use crate::walk::PY_VALUE_DEPTH_CAP;
use indexmap::IndexMap;
use pyo3::exceptions::{PyOverflowError, PyTypeError, PyValueError};
use pyo3::types::{PyDict, PyList, PyTuple};
use relate::Namespace;
use relate_model::value::IsoDuration;
use relate_model::CypherValue;

/// Initialize the embedded interpreter (idempotent) and run `f` under the
/// GIL. `cargo test` links libpython but does not auto-initialize it.
fn with_py<F, R>(f: F) -> R
where
    F: for<'py> FnOnce(Python<'py>) -> R,
{
    pyo3::prepare_freethreaded_python();
    Python::with_gil(f)
}

fn new_database(path: &str) -> Database {
    new_database_ns(path, "neo4j")
}

fn new_database_ns(path: &str, namespace: &str) -> Database {
    try_new_database_ns(path, namespace).unwrap()
}

/// Fallible twin of [`new_database_ns`] for tests that assert on a failed
/// connect.
fn try_new_database_ns(path: &str, namespace: &str) -> PyResult<Database> {
    with_py(|py| Database::new(py, path.to_string(), namespace.to_string(), false))
}

/// The `match cv { Variant(x) => x, other => panic!(…) }` extraction,
/// written once per variant used in this suite (F-1-15): the panic keeps
/// the `{other:?}` payload, `#[track_caller]` keeps the call-site location.
#[track_caller]
fn expect_list(v: CypherValue) -> Vec<CypherValue> {
    match v {
        CypherValue::List(items) => items,
        other => panic!("expected List, got {other:?}"),
    }
}

/// See [`expect_list`].
#[track_caller]
fn expect_str(v: CypherValue) -> String {
    match v {
        CypherValue::Str(s) => s,
        other => panic!("expected Str, got {other:?}"),
    }
}

/// See [`expect_list`].
#[track_caller]
fn expect_map(v: CypherValue) -> IndexMap<String, CypherValue> {
    match v {
        CypherValue::Map(map) => map,
        other => panic!("expected Map, got {other:?}"),
    }
}

/// See [`expect_list`].
#[track_caller]
fn expect_date(v: CypherValue) -> chrono::NaiveDate {
    match v {
        CypherValue::Date(d) => d,
        _ => panic!("expected a Date value"),
    }
}

/// See [`expect_list`].
#[track_caller]
fn expect_point(v: CypherValue) -> relate_model::value::PointValue {
    match v {
        CypherValue::Point(p) => p,
        _ => panic!("expected a Point value"),
    }
}

/// Extract the string attribute `name` (`kind`, `tck_class`, …) from the
/// value carried by a raised `PyErr`.
fn err_attr(err: &PyErr, py: Python<'_>, name: &str) -> String {
    err.value(py)
        .as_any()
        .getattr(name)
        .unwrap()
        .extract()
        .unwrap()
}

/// Run one raw engine statement through the guarded accessor — the tests'
/// direct engine access, bypassing the pymethod layer. The affinity check
/// does run and passes because `with_py` takes the GIL on the calling thread,
/// which is the thread that created the database; a foreign-thread call is
/// refused like any other engine access.
fn raw_query(db: &mut Database, query: &str) -> relate::QueryResult {
    with_py(|py| db.lock_conn(py).unwrap().query(query).unwrap())
}

/// Run `query` on `db` and return the first row's `key` value as a Python
/// object bound to `py`.
fn query_first_py<'py>(
    py: Python<'py>,
    db: &mut Database,
    query: &str,
    key: &str,
) -> Bound<'py, PyAny> {
    let result = raw_query(db, query);
    let rows = query_result_to_cypher_rows(&result);
    cypher_value_to_py(py, rows[0].get(key).unwrap(), result.header.namespace)
        .unwrap()
        .into_bound(py)
}

/// Evaluate `expr` as `RETURN <expr> AS v` and return the value together with
/// the name of the Python type it arrived as. Shared by the temporal-family
/// leaf modules (temporal_tests, duration_tests).
fn temporal_result<'py>(py: Python<'py>, expr: &str) -> (Bound<'py, PyAny>, String) {
    let mut db = new_database(":memory:");
    let value = query_first_py(py, &mut db, &format!("RETURN {expr} AS v"), "v");
    let type_name = py_type_name(&value);
    (value, type_name)
}

/// One attribute of a `relate.Temporal` result, extracted as `T`.
/// Shared by the temporal-family leaf modules (temporal_tests,
/// duration_tests).
fn temporal_attr<T>(value: &Bound<'_, PyAny>, name: &str) -> T
where
    T: for<'py> FromPyObject<'py>,
{
    value
        .getattr(name)
        .unwrap_or_else(|e| panic!("Temporal.{name} missing: {e}"))
        .extract()
        .unwrap_or_else(|e| panic!("Temporal.{name} has an unexpected type: {e}"))
}

mod duration_tests;
mod lifecycle_tests;
mod namespace_tests;
mod params_tests;
mod result_tests;
mod temporal_tests;
