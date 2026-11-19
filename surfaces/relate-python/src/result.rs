//! The Python [`QueryResult`] object and the assembly of one engine result into
//! it: row dicts, mutation stats, result metadata and notifications.
//!
//! The value conversions themselves live in [`crate::convert`]; this module
//! only arranges its output on the result object.

use super::convert::{cypher_value_to_py, json_to_py, notifications_to_py};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use relate::{query_stats_to_wire, value_to_cypher};
use std::collections::HashMap;

/// A Relate query result.
///
/// Beyond the rows, every result carries two construction-time stamps
/// (ADR-0285): `namespace`, the statement's *resolved* namespace (ADR-0117
/// §7) — the `USE` target when the statement carries one, else the session
/// namespace — and `query_type`, the plan-time classification (ADR-0162) as
/// its Neo4j Query API v2 wire code: `"r"` (read), `"w"` (a write with no
/// returned columns), `"rw"` (a write with returned columns) or `"s"`
/// (schema), the split per ADR-0332.
#[pyclass(unsendable)]
#[derive(Debug)]
pub(super) struct QueryResult {
    #[pyo3(get)]
    pub(super) columns: Vec<String>,
    #[pyo3(get)]
    pub(super) data: Vec<Py<PyDict>>,
    #[pyo3(get)]
    pub(super) stats: HashMap<String, i64>,
    #[pyo3(get)]
    pub(super) namespace: String,
    #[pyo3(get)]
    pub(super) query_type: String,
    /// The statement's bookmark (ADR-0502 §7): an auto-commit write's
    /// commit sequence spelled `relate:<n>` (ADR-0310), monotonic per
    /// store; `None` when the statement committed nothing — a read, or any
    /// statement inside an explicit transaction (whose bookmark the commit
    /// returns).
    #[pyo3(get)]
    pub(super) bookmark: Option<String>,
    /// The engine-reported statement elapsed in wall-clock milliseconds
    /// (ADR-0221).
    #[pyo3(get)]
    pub(super) elapsed_ms: f64,
    /// The statement's PARSE phase in milliseconds (ADR-0491) — the
    /// front-end parse and lowering window of `elapsed_ms`.
    #[pyo3(get)]
    pub(super) parse_ms: f64,
    /// The statement's PLAN phase in milliseconds (ADR-0491) — binding,
    /// logical planning, admission classification.
    #[pyo3(get)]
    pub(super) plan_ms: f64,
    /// The statement's EXECUTE phase in milliseconds (ADR-0491) — the
    /// dispatch, or the EXPLAIN plan render.
    #[pyo3(get)]
    pub(super) execute_ms: f64,
    /// Per-column result types (ADR-0224): one of the 18 canonical type
    /// names per column, same length and order as `columns`, derived from
    /// the first row (`ANY` for a Null cell or a result with no rows).
    #[pyo3(get)]
    pub(super) column_types: Vec<String>,
    /// The query notifications derived with the plan tree (ADR-0363):
    /// `EXPLAIN`/`PROFILE` populate the list, every plain run leaves it
    /// empty. Each entry is a dict in the engine-native shape — `code`,
    /// `gql_status`, `severity`, `category`, `title`, `description`,
    /// `remedy` and, on operator-scoped kinds, `operator_id` — serialized
    /// by the shared `relate_model::wire` encoder.
    #[pyo3(get)]
    pub(super) notifications: Vec<Py<PyDict>>,
    /// The statement's structured plan as a `relate.plan/v1` document
    /// (LABS-370, ADR-0381), or `None` when the statement carried no plan.
    /// Each node's `warnings` carry the engine's remedy-bearing
    /// notifications (ADR-0363), joined by operator id.
    #[pyo3(get)]
    pub(super) plan: Option<Py<PyDict>>,
}

impl QueryResult {
    /// Build the Python result from one engine result.
    pub(super) fn from_engine(py: Python<'_>, result: relate::QueryResult) -> PyResult<Self> {
        // Engine rows are rectangular — one value per column, in column
        // order — so zip the columns with each row's values: every cell is
        // present by construction (no map lookup, no missing-key fallback).
        // Element ids are spelled in the statement's resolved namespace —
        // the namespace `elementId()` reads (ADR-0116 §7).
        let ns = result.header.namespace;
        let mut data: Vec<Py<PyDict>> = Vec::with_capacity(result.rows.len());
        for row in &result.rows {
            let dict = PyDict::new(py);
            for (col, val) in result.header.columns.iter().zip(row) {
                dict.set_item(col, cypher_value_to_py(py, &value_to_cypher(val), ns)?)?;
            }
            data.push(dict.unbind());
        }

        let stats = stats_to_map(&result.stats);
        let namespace = result.header.namespace.as_str().to_string();
        let query_type = result.header.query_type.as_str().to_string();
        let elapsed_ms = result.elapsed_ms();
        // The ADR-0491 phase split, read from the same engine result before
        // its header fields are moved out below.
        let parse_ms = result.parse_ms();
        let plan_ms = result.plan_ms();
        let execute_ms = result.execute_ms();
        let notifications = notifications_to_py(py, result.notifications())?;
        // The structured plan (LABS-370, ADR-0381): the model's one
        // projection over the engine's plan root, with the result's
        // notifications joined in per operator — `None` when the statement
        // carried no plan (a plain run). Built before `header` is moved
        // out below. A plan past the plan-wire budget (ADR-0628) raises
        // the typed refusal — its text states the scope: the statement
        // itself executed; only its plan document is refused.
        let plan = match relate::plan_wire_json(&result) {
            Ok(Some(document)) => Some(json_to_py(py, &document)?.bind(py).extract()?),
            Ok(None) => None,
            Err(e) => return Err(crate::relate_err_to_py(py, &e)),
        };
        let column_types = result.header.column_types;
        Ok(QueryResult {
            columns: result.header.columns,
            data,
            stats,
            namespace,
            query_type,
            bookmark: result.bookmark,
            elapsed_ms,
            parse_ms,
            plan_ms,
            execute_ms,
            column_types,
            notifications,
            plan,
        })
    }
}

#[pymethods]
impl QueryResult {
    fn __repr__(&self) -> String {
        format!(
            "QueryResult(columns={:?}, rows={})",
            self.columns,
            self.data.len()
        )
    }
}

/// All 11 mutation counters as an `i64` map (zeros included).
pub(super) fn stats_to_map(stats: &relate::QueryStats) -> HashMap<String, i64> {
    let stats = query_stats_to_wire(stats);
    let mut out = HashMap::with_capacity(11);
    out.insert("nodes_created".to_string(), stats.nodes_created as i64);
    out.insert("nodes_deleted".to_string(), stats.nodes_deleted as i64);
    out.insert(
        "relationships_created".to_string(),
        stats.relationships_created as i64,
    );
    out.insert(
        "relationships_deleted".to_string(),
        stats.relationships_deleted as i64,
    );
    out.insert("properties_set".to_string(), stats.properties_set as i64);
    out.insert("labels_added".to_string(), stats.labels_added as i64);
    out.insert("labels_removed".to_string(), stats.labels_removed as i64);
    out.insert("indexes_added".to_string(), stats.indexes_added as i64);
    out.insert("indexes_removed".to_string(), stats.indexes_removed as i64);
    out.insert(
        "constraints_added".to_string(),
        stats.constraints_added as i64,
    );
    out.insert(
        "constraints_removed".to_string(),
        stats.constraints_removed as i64,
    );
    out
}
