//! Relate Python bindings via pyo3.
//!
//! Compiled as the private extension module `relate._native`. The public
//! Python API (`relate.connect()`, `Connection`, `Cursor`, the DB-API
//! exception tree) lives in `python/relate/` and wraps the `Database` class
//! exposed here.
//!
//! # What crosses the boundary
//!
//! Query results are returned as *rich* Python objects, not magic-key dicts:
//! nodes and relationships become [`Node`] / [`Relationship`] instances that
//! expose their element ids and topology (a `Relationship` carries
//! `start_node` / `end_node` ids, so a graph can be reconstructed on the Python
//! side); every entity carries the engine's `elementId()` string (ADR-0502
//! §6), relationships their endpoints' too; paths become [`Path`]; points
//! become [`Point`]; durations become [`Duration`] (months, days, seconds,
//! nanoseconds — lossless, ADR-0502 §1); vectors become [`Vector`]; the other
//! temporal values become the matching `datetime` objects where those are
//! lossless and [`Temporal`] where they are not (sub-microsecond nanoseconds,
//! or a named zone); geometries become self-describing dicts (the canonical
//! `relate_model::wire` shape).
//!
//! Parameters convert in the other direction too: Python `datetime` /
//! `date` / `time` / `timedelta`, [`Duration`], [`Point`] and [`Vector`] map
//! onto the matching `CypherValue`, and a truly unconvertible object raises a
//! typed error instead of being silently stringified.
//!
//! # Errors
//!
//! Relate failures raise [`RelateNativeError`] (a subclass of
//! `RuntimeError`, for backward compatibility) carrying `.kind` and
//! `.tck_class` attributes from `relate::binding_error_metadata`, plus the
//! engine's classification facts from `relate::error_info` (ADR-0502 §2):
//! `.neo4j_code`, `.gql_status`, `.stage` and `.retryable`. The Python
//! package maps `kind`/`tck_class` onto the DB-API exception tree
//! (`IntegrityError`, `DataError`, `ProgrammingError`, …) and carries every
//! attribute over to the mapped exception.
//!
//! # Threading
//!
//! `execute` / `execute_arrow` release the GIL (`py.allow_threads`) around the
//! Rust engine call, so other Python threads make progress while a query runs.
//! A `Database` is thread-affine (the DB-API `threadsafety` level is 1: module
//! shareable, connections not): it records the thread that opened it and every
//! engine access from another thread fails with a typed
//! [`RelateNativeError`] (`kind = "WrongThread"`, which the Python package
//! maps to `ProgrammingError`) — the `sqlite3` behavior, never a pyo3 panic.
//! Dropping the last reference on another thread is safe: the Rust `Drop`
//! impl runs the checkpoint from that thread.

#![deny(unsafe_op_in_unsafe_fn)]

use pyo3::create_exception;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use relate::{
    binding_error_metadata, cypher_to_value, error_info, query_result_to_cypher_rows,
    Connection as RelateConnection, Database as RelateDatabase, Error as RelateError, ErrorInfo,
    OpenMode, OpenOptions, Value,
};
use relate_arrow::record_batch::result_to_record_batch;
use std::sync::{Mutex, MutexGuard, PoisonError};

mod convert;
mod graph_values;
mod result;
mod temporal_values;
mod vector_values;
mod walk;

use convert::optional_params_to_cypher;
use graph_values::{Node, Path, Point, Relationship};
use result::QueryResult;
use temporal_values::{Duration, Temporal};
use vector_values::Vector;

create_exception!(
    _native,
    RelateNativeError,
    PyRuntimeError,
    "A Relate engine error. Carries `.kind` and `.tck_class` string \
     attributes drawn from the typed engine error, which the `relate` \
     package maps onto the DB-API 2.0 exception tree, and the engine's \
     classification: `.neo4j_code`, `.gql_status`, `.stage` \
     (`compile`/`runtime`/`internal`) and `.retryable` — all `None` on a \
     binding-level failure, which the engine never classified."
);

/// Build a [`RelateNativeError`] from a typed Relate error, attaching the
/// `.kind` and `.tck_class` attributes so the Python package can map onto the
/// DB-API exception tree without parsing the message, and the engine's
/// [`ErrorInfo`] facts (ADR-0502 §2).
///
/// `relate::binding_error_metadata` is the single source of truth for
/// the binding vocabulary — it already resolves OpenCypher error names nested
/// inside an outer message (e.g. `ConstraintVerificationFailed` under a
/// semantic wrapper), so no message re-scanning happens here — and
/// `relate::error_info` is the single source of the Neo4j code,
/// GQLSTATUS, stage and retryable verdict: the binding holds no
/// kind-to-code table.
fn relate_err_to_py(py: Python<'_>, e: &RelateError) -> PyErr {
    let (kind, tck_class) = binding_error_metadata(e);
    native_error(py, e.to_string(), kind, tck_class, Some(error_info(e)))
}

/// Set one attribute on a fresh [`RelateNativeError`] — the invariant
/// [`native_error`] documents.
fn set_error_attr<'py>(value: &Bound<'py, PyAny>, name: &str, attr: impl IntoPyObject<'py>) {
    value
        .setattr(name, attr)
        .expect("setting attributes on a fresh RelateNativeError");
}

/// The binding-level error kinds, spelled exactly once: each string must
/// byte-match a key of `relate.exceptions._KIND_TO_EXCEPTION`, which maps
/// it onto the DB-API tree. Closing the set at this boundary means a
/// misspelling fails to compile instead of silently misclassifying.
#[derive(Clone, Copy)]
enum BindingErrorKind {
    /// A connection used from a thread other than the one that opened it.
    WrongThread,
    /// A connection used after `close()`.
    ConnectionClosed,
    /// A binding-level invariant violation (Arrow conversion, introspection).
    InternalError,
}

impl BindingErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            BindingErrorKind::WrongThread => "WrongThread",
            BindingErrorKind::ConnectionClosed => "ConnectionClosed",
            BindingErrorKind::InternalError => "InternalError",
        }
    }
}

/// Build a [`RelateNativeError`] for a binding-level failure — one raised
/// by the binding itself, not carried out of the engine. `tck_class` stays
/// empty: a binding-level failure has no openCypher error class, so the
/// Python package maps it by `kind` alone through the same
/// `exception_for(kind, tck_class)` path. The engine never classified it,
/// so its [`ErrorInfo`] facts are absent (`None` in Python), never
/// fabricated.
fn binding_err_to_py(py: Python<'_>, kind: BindingErrorKind, message: String) -> PyErr {
    native_error(py, message, kind.as_str(), "", None)
}

/// The one native-error constructor: build a [`RelateNativeError`] and
/// attach the `.kind` / `.tck_class` metadata the Python package's
/// `exception_for(kind, tck_class)` mapping reads, plus the four
/// [`ErrorInfo`] facts (`.neo4j_code`, `.gql_status`, `.stage`,
/// `.retryable`; each `None` when `info` is). Engine failures
/// ([`relate_err_to_py`]) and binding-level failures
/// ([`binding_err_to_py`]) both pass through here, so the attribute set has
/// exactly one home.
///
/// Setting attributes on a fresh instance of our own exception type cannot
/// fail (a heap type with a `__dict__`, plain `str`/`bool`/`None` values) —
/// a failure would mean the error contract itself is broken, so it is an
/// invariant, never a silently dropped classification.
fn native_error(
    py: Python<'_>,
    message: String,
    kind: &str,
    tck_class: &str,
    info: Option<ErrorInfo>,
) -> PyErr {
    let err = RelateNativeError::new_err(message);
    let value = err.value(py).as_any();
    set_error_attr(value, "kind", kind);
    set_error_attr(value, "tck_class", tck_class);
    set_error_attr(value, "neo4j_code", info.map(|i| i.neo4j_code));
    set_error_attr(value, "gql_status", info.map(|i| i.gql_status));
    set_error_attr(value, "stage", info.map(|i| i.stage.as_str()));
    set_error_attr(value, "retryable", info.map(|i| i.retryable));
    err
}

/// A Relate graph database instance.
///
/// `Database()` or `Database(":memory:")` opens an in-memory database.
/// Any other path opens a Relate directory with a WAL; its data persists on
/// each commit and is checkpointed by `flush()` / `close()`.
///
/// The optional second argument selects the session namespace (ADR-0117 §4):
/// `"neo4j"` (default), `"system"` or `"system_ontologies"`, matched
/// case-insensitively.
///
/// Thread-affine (DB-API `threadsafety = 1`): the opening thread is recorded
/// and every engine access from any other thread raises a
/// [`RelateNativeError`] with `kind = "WrongThread"` (mapped to
/// `ProgrammingError` by the Python package). The class is sendable so a
/// foreign-thread *drop* stays safe — the checkpoint runs from that thread —
/// but no engine operation ever runs there.
#[pyclass]
struct Database {
    engine: Option<EngineHandles>,
    path: String,
    /// The thread that constructed this instance — the only thread allowed to
    /// touch the engine (GH #352 / LABS-700).
    owner: std::thread::ThreadId,
}

/// The live engine handles behind a [`Database`]: one liveness fact, present
/// from `new` until `close` / `reset` takes it — never one handle without the
/// other, which is why the pair is only ever built by [`EngineHandles::open`].
struct EngineHandles {
    db: RelateDatabase,
    /// The engine connection is `Send` but not `Sync` (it owns a write-scratch
    /// `RefCell` and interior `Cell`s), while a sendable pyclass must be
    /// `Sync`. The `Mutex` supplies the missing bound; it is structurally
    /// uncontended — `check_owner_thread` admits only the opening thread, so
    /// at most one guard exists at any time.
    conn: Mutex<RelateConnection>,
}

impl EngineHandles {
    /// The one constructor for the handle pair: open the store in the given
    /// mode, then connect the session namespace — `Database::new` (namespace
    /// string) and `reset` (namespace already typed) both go through here,
    /// so the open policy and the pair assembly have one home.
    fn open(
        path: &str,
        read_only: bool,
        connect: impl FnOnce(&RelateDatabase) -> Result<RelateConnection, RelateError>,
    ) -> Result<Self, RelateError> {
        let db = RelateDatabase::open_with(path, OpenOptions::with_mode(open_mode(read_only)))?;
        let conn = connect(&db)?;
        Ok(EngineHandles {
            db,
            conn: Mutex::new(conn),
        })
    }
}

fn open_mode(read_only: bool) -> OpenMode {
    if read_only {
        OpenMode::ReadOnly
    } else {
        OpenMode::Writable
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // The deferred cleanup for a foreign-thread release (GH #352): the one
        // engine touch that runs without the affinity check, from whatever
        // thread drops the last reference — so a failure is reported, never
        // swallowed.
        if let Some(engine) = self.engine.as_ref() {
            if let Err(e) = engine.db.checkpoint_if_writable() {
                eprintln!("relate: final checkpoint on drop failed: {e}");
            }
        }
    }
}

/// A Rust-side execution error carried out of `allow_threads` (where the GIL is
/// released and a `PyErr` cannot be constructed), mapped to a `PyErr` once the
/// GIL is reacquired.
enum ExecError {
    Relate(RelateError),
    Other(String),
}

impl Database {
    /// Refuse an engine access from any thread other than the opening one
    /// (DB-API `threadsafety = 1`). Typed, never a panic: the error carries
    /// `kind = "WrongThread"`, which the Python package maps to
    /// `ProgrammingError` — the `sqlite3` behavior (GH #352 / LABS-700).
    ///
    /// Called by the handle accessors below and by `close`; `Drop` is the
    /// one engine touch that deliberately runs without it (a foreign-thread
    /// release has to clean up, GH #352).
    fn check_owner_thread(&self, py: Python<'_>) -> PyResult<()> {
        if self.owner == std::thread::current().id() {
            return Ok(());
        }
        Err(binding_err_to_py(
            py,
            BindingErrorKind::WrongThread,
            "Relate connection used from a thread other than the one that \
             opened it: connections are thread-affine (DB-API threadsafety = 1). \
             Create the connection on the thread that uses it."
                .to_string(),
        ))
    }

    /// The closed-store refusal, one spelling for both handle accessors.
    /// Typed like every other binding-level refusal: `kind =
    /// "ConnectionClosed"`, which the Python package maps to
    /// `ProgrammingError` (its documented "operating on a closed connection"
    /// case).
    fn closed_err(py: Python<'_>) -> PyErr {
        binding_err_to_py(
            py,
            BindingErrorKind::ConnectionClosed,
            "database is closed".to_string(),
        )
    }

    /// The one engine-handle accessor for the GIL-side paths: thread-affinity
    /// check, closed check, and the live handles. Every engine read or write
    /// on the owner thread goes through here or through its `lock_conn`
    /// twin, which re-checks; `Drop` and `close` are the two
    /// documented exceptions (`Drop` has no owner guarantee, `close` must
    /// no-op when already closed).
    fn engine(&self, py: Python<'_>) -> PyResult<&EngineHandles> {
        self.check_owner_thread(py)?;
        self.engine.as_ref().ok_or_else(|| Self::closed_err(py))
    }

    /// The engine connection guard, after the affinity and closed checks.
    /// Poisoning (a lock abandoned by an engine panic mid-query) is recovered
    /// with [`PoisonError::into_inner`]: the mutex exists only for pyo3's
    /// `Sync` bound, never for mutual exclusion, so the guard's data is
    /// exactly what the panicking call left — the same state the owner
    /// thread sees today after catching a `PanicException` and continuing.
    fn lock_conn(&self, py: Python<'_>) -> PyResult<MutexGuard<'_, RelateConnection>> {
        let engine = self.engine(py)?;
        Ok(engine.conn.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Checkpoint one engine handle, mapping the engine error — the one
    /// spelling of the checkpoint call, used by `flush`, `reset` and `close`.
    /// The engine itself no-ops read-only stores; `Drop` is the exception
    /// (no GIL, so no error mapping).
    fn checkpoint_of(engine: &EngineHandles, py: Python<'_>) -> PyResult<()> {
        engine
            .db
            .checkpoint_if_writable()
            .map_err(|e| relate_err_to_py(py, &e))
    }

    /// Checkpoint the live store after the affinity and closed checks — the
    /// policy for `flush` and `reset`, which refuse a closed store.
    fn checkpoint(&self, py: Python<'_>) -> PyResult<()> {
        Self::checkpoint_of(self.engine(py)?, py)
    }

    /// The one off-GIL engine hand-off behind `execute` and
    /// `execute_arrow`: convert the Python params, release the GIL, run the
    /// query, and feed the result to `tail` — all in one GIL-release
    /// window, with the `ExecError` mapped to a `PyErr` once the GIL is
    /// back. The connection guard stays on this thread for the window's
    /// whole duration; the closure captures a `&mut` reborrow of it, which
    /// is `Send` (pyo3's `allow_threads` takes any `Send` capture — the
    /// engine connection carries no Python/GIL-bound state), so no unsafe
    /// pointer is needed to cross the GIL-release boundary.
    fn run_engine_off_gil<T: Send>(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
        tail: impl FnOnce(relate::QueryResult) -> Result<T, ExecError> + Send,
    ) -> PyResult<T> {
        let rust_params = optional_params_to_cypher(params)?;
        let mut conn = self.lock_conn(py)?;
        let connection: &mut RelateConnection = &mut conn;
        let out = py.allow_threads(move || {
            let params = rust_params
                .into_iter()
                .map(|(key, value)| (key, cypher_to_value(&value)))
                .collect();
            let result = connection
                .query_with_params(query, params)
                .map_err(ExecError::Relate)?;
            tail(result)
        });
        // Binding-level failures (the Arrow conversion path) carry the
        // `InternalError` kind so the Python package maps them onto the DB-API
        // tree instead of degrading to the generic `DatabaseError` fallback.
        out.map_err(|e| match e {
            ExecError::Relate(e) => relate_err_to_py(py, &e),
            ExecError::Other(msg) => binding_err_to_py(py, BindingErrorKind::InternalError, msg),
        })
    }

    fn scalar_count(&self, py: Python<'_>, query: &str) -> PyResult<usize> {
        let result = self
            .lock_conn(py)?
            .query(query)
            .map_err(|e| relate_err_to_py(py, &e))?;
        match result.rows.first().and_then(|row| row.first()) {
            Some(Value::Int(value)) => usize::try_from(*value).map_err(|_| {
                binding_err_to_py(
                    py,
                    BindingErrorKind::InternalError,
                    "count is out of range".to_string(),
                )
            }),
            _ => Err(binding_err_to_py(
                py,
                BindingErrorKind::InternalError,
                "count query returned an unexpected value".to_string(),
            )),
        }
    }

    fn string_column(&self, py: Python<'_>, query: &str) -> PyResult<Vec<String>> {
        let result = self
            .lock_conn(py)?
            .query(query)
            .map_err(|e| relate_err_to_py(py, &e))?;
        result
            .rows
            .iter()
            .map(|row| match row.first() {
                Some(Value::String(value)) => Ok(String::from(&**value)),
                _ => Err(binding_err_to_py(
                    py,
                    BindingErrorKind::InternalError,
                    "introspection query returned an unexpected value".to_string(),
                )),
            })
            .collect()
    }
}

#[pymethods]
impl Database {
    #[new]
    #[pyo3(signature = (path = ":memory:".to_string(), namespace = "neo4j".to_string(), *, read_only = false))]
    fn new(py: Python<'_>, path: String, namespace: String, read_only: bool) -> PyResult<Self> {
        let engine = EngineHandles::open(&path, read_only, |db| db.connect_namespace(&namespace))
            .map_err(|e| relate_err_to_py(py, &e))?;
        Ok(Database {
            engine: Some(engine),
            path,
            owner: std::thread::current().id(),
        })
    }

    /// The session namespace this connection's statements run in when they
    /// carry no `USE` clause (ADR-0117 §4): `neo4j`, `system` or
    /// `system_ontologies`.
    fn namespace(&self, py: Python<'_>) -> PyResult<String> {
        Ok(self.lock_conn(py)?.namespace().to_string())
    }

    /// Change the session namespace (ADR-0117 §4). Names match
    /// case-insensitively; an unknown name fails with `DatabaseNotFound`.
    /// Fails while a transaction is open: namespaces change only between
    /// transactions (ADR-0116 §5).
    fn set_namespace(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let mut conn = self.lock_conn(py)?;
        conn.set_namespace(name)
            .map_err(|e| relate_err_to_py(py, &e))?;
        Ok(())
    }

    /// Execute a Cypher query and return the result.
    #[pyo3(signature = (query, params=None))]
    fn execute(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<QueryResult> {
        let result = self.run_engine_off_gil(py, query, params, Ok)?;
        QueryResult::from_engine(py, result)
    }

    /// Execute a Cypher query and return the result as Arrow IPC bytes.
    ///
    /// Returns bytes containing an Arrow IPC stream message. Deserialize in Python:
    /// ```python
    /// import pyarrow as pa
    /// buf = db.execute_arrow("MATCH (n) RETURN n.name, n.age")
    /// reader = pa.ipc.open_stream(buf)
    /// batch = reader.read_all()  # pyarrow.Table
    /// df = batch.to_pandas()
    /// ```
    #[pyo3(signature = (query, params=None))]
    fn execute_arrow(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyObject> {
        // Serialize the engine result to Arrow IPC in the same GIL-release
        // window the query ran in.
        let buf = self.run_engine_off_gil(py, query, params, |result| {
            let rows = query_result_to_cypher_rows(&result);
            let batch = result_to_record_batch(&result.header.columns, &rows)
                .map_err(|e| ExecError::Other(format!("Arrow conversion error: {e}")))?;
            let mut buf = Vec::new();
            {
                let mut writer =
                    arrow_ipc::writer::StreamWriter::try_new(&mut buf, &batch.schema())
                        .map_err(|e| ExecError::Other(format!("IPC writer error: {e}")))?;
                writer
                    .write(&batch)
                    .map_err(|e| ExecError::Other(format!("IPC write error: {e}")))?;
                writer
                    .finish()
                    .map_err(|e| ExecError::Other(format!("IPC finish error: {e}")))?;
            }
            Ok(buf)
        })?;

        Ok(PyBytes::new(py, &buf).into_any().unbind())
    }

    /// Whether the backing store provides atomic transactions.
    fn supports_transactions(&self) -> bool {
        true
    }

    /// Whether a transaction is currently active on the backing store.
    fn transaction_active(&self, py: Python<'_>) -> PyResult<bool> {
        Ok(self.lock_conn(py)?.in_transaction())
    }

    /// Begin a transaction on the backing store.
    fn begin_transaction(&self, py: Python<'_>) -> PyResult<()> {
        self.lock_conn(py)?
            .begin()
            .map_err(|e| relate_err_to_py(py, &e))
    }

    /// Durably commit the active transaction and return its bookmark
    /// (ADR-0502 §7): the engine's commit sequence spelled `relate:<n>`
    /// (ADR-0310), monotonic per store, or `None` when the transaction
    /// committed nothing (it wrote nothing).
    fn commit_transaction(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.lock_conn(py)?
            .commit()
            .map_err(|e| relate_err_to_py(py, &e))
    }

    /// Roll back the active transaction, discarding its changes.
    fn rollback_transaction(&self, py: Python<'_>) -> PyResult<()> {
        self.lock_conn(py)?
            .rollback()
            .map_err(|e| relate_err_to_py(py, &e))
    }

    /// Flush pending changes to disk (no-op for in-memory and read-only
    /// databases; fails with the typed closed-store error when closed).
    fn flush(&self, py: Python<'_>) -> PyResult<()> {
        self.checkpoint(py)
    }

    /// Flush pending changes and close the database. On a foreign thread this
    /// refuses with the typed thread-affinity error; the deferred cleanup
    /// then happens in `Drop`, which checkpoints safely from any thread.
    /// Closing an already-closed database is a no-op.
    ///
    /// Takes `&mut self` (it replaces the handle pair, which needs the
    /// exclusive borrow) — so a foreign-thread call *concurrent* with an
    /// in-flight query is refused by pyo3's own borrow check rather than by
    /// [`Database::check_owner_thread`]; either way the refusal is a typed,
    /// catchable error, never a panic. The query and introspection paths all
    /// take `&self` precisely so the affinity check is the one that answers.
    fn close(&mut self, py: Python<'_>) -> PyResult<()> {
        self.check_owner_thread(py)?;
        if let Some(engine) = self.engine.take() {
            Self::checkpoint_of(&engine, py)?;
        }
        Ok(())
    }

    /// Reset the database by reopening the same path in the live connection's
    /// current session namespace (ADR-0117 §4).
    ///
    /// The writer lease (ADR-0136) forces the old handles to be dropped before
    /// the reopen, so a failed reopen leaves the database closed (`engine =
    /// None`) and returns the open error; a successful one is atomic from the
    /// caller's view.
    fn reset(&mut self, py: Python<'_>) -> PyResult<()> {
        // The live connection is the single source of truth for the session
        // namespace; read it (typed, no string round trip) before reopening.
        // The handle accessors carry the affinity and closed checks.
        let namespace = self.lock_conn(py)?.namespace();
        let read_only = self.engine(py)?.db.is_read_only();
        self.checkpoint(py)?;
        // Drop the old handles before reopening: a second writable open of the
        // same store is refused by the writer lease (ADR-0136).
        self.engine = None;
        let engine = EngineHandles::open(&self.path, read_only, |db| db.connect_typed(namespace))
            .map_err(|e| relate_err_to_py(py, &e))?;
        self.engine = Some(engine);
        Ok(())
    }

    /// Get the number of nodes in the database.
    fn node_count(&self, py: Python<'_>) -> PyResult<usize> {
        self.scalar_count(py, "MATCH (n) RETURN count(n) AS count")
    }

    /// Get the number of relationships in the database.
    fn relationship_count(&self, py: Python<'_>) -> PyResult<usize> {
        self.scalar_count(py, "MATCH ()-[r]->() RETURN count(r) AS count")
    }

    /// Get all node labels.
    fn labels(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        self.string_column(
            py,
            "CALL db.labels() YIELD label RETURN label ORDER BY label",
        )
    }

    /// Get all relationship types.
    fn relationship_types(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        self.string_column(
            py,
            "CALL db.relationshipTypes() YIELD relationshipType RETURN relationshipType ORDER BY relationshipType",
        )
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "Database(nodes={}, relationships={})",
            self.node_count(py)?,
            self.relationship_count(py)?
        ))
    }
}

/// Relate native extension module (imported as `relate._native`).
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Database>()?;
    m.add_class::<QueryResult>()?;
    m.add_class::<Node>()?;
    m.add_class::<Relationship>()?;
    m.add_class::<Path>()?;
    m.add_class::<Point>()?;
    m.add_class::<Temporal>()?;
    m.add_class::<Duration>()?;
    m.add_class::<Vector>()?;
    m.add("RelateNativeError", m.py().get_type::<RelateNativeError>())?;
    Ok(())
}

#[cfg(test)]
mod tests;
