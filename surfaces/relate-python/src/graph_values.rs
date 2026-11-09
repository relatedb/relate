//! The rich graph-value pyclasses returned from a query: [`Node`],
//! [`Relationship`], [`Path`] and [`Point`].
//!
//! `convert` builds these, so their fields are crate-visible; the pymodule
//! registers the classes from the crate root.

use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// A graph node returned from a query. Exposes its integer `id`, its
/// `element_id` (the engine's `elementId()` string), `labels`, and
/// `properties` (an ordered dict). Property values are accessible either via
/// `node.properties["key"]` or the shorthand `node["key"]`.
#[pyclass(module = "relate._native")]
pub(super) struct Node {
    #[pyo3(get)]
    pub(super) id: i64,
    /// Exactly what Cypher `elementId()` returns for this node (ADR-0502 §6,
    /// `4:relate-neo4j:n<id>` in the `neo4j` namespace), so
    /// `WHERE elementId(n) = $id` round-trips. `None` only in a namespace
    /// whose elements have no element ids (`system`), where `elementId()`
    /// itself fails.
    #[pyo3(get)]
    pub(super) element_id: Option<String>,
    #[pyo3(get)]
    pub(super) labels: Vec<String>,
    pub(super) properties: Py<PyDict>,
}

#[pymethods]
impl Node {
    #[getter]
    fn properties(&self, py: Python<'_>) -> Py<PyDict> {
        self.properties.clone_ref(py)
    }

    /// The property pairs, like the driver entity's `items()`.
    fn items(&self, py: Python<'_>) -> PyResult<PyObject> {
        Ok(self.properties.bind(py).call_method0("items")?.unbind())
    }

    fn __getitem__(&self, py: Python<'_>, key: &str) -> PyResult<PyObject> {
        match self.properties.bind(py).get_item(key)? {
            Some(v) => Ok(v.unbind()),
            None => Err(PyKeyError::new_err(key.to_string())),
        }
    }

    fn __contains__(&self, py: Python<'_>, key: &str) -> PyResult<bool> {
        Ok(self.properties.bind(py).get_item(key)?.is_some())
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        match other.downcast::<Node>() {
            Ok(o) => o.borrow().id == self.id,
            Err(_) => false,
        }
    }

    fn __hash__(&self) -> i64 {
        self.id
    }

    fn __repr__(&self) -> String {
        format!("Node(id={}, labels={:?})", self.id, self.labels)
    }
}

/// A relationship returned from a query. Exposes its integer `id`, `type`, the
/// `start_node` / `end_node` integer ids (so topology is recoverable), the
/// matching `element_id` / `start_node_element_id` / `end_node_element_id`
/// strings, and its `properties`.
#[pyclass(module = "relate._native")]
pub(super) struct Relationship {
    #[pyo3(get)]
    pub(super) id: i64,
    /// Exactly what Cypher `elementId()` returns for this relationship
    /// (ADR-0502 §6, `5:relate-neo4j:r<id>` in the `neo4j` namespace); `None`
    /// only where `Node.element_id` is.
    #[pyo3(get)]
    pub(super) element_id: Option<String>,
    pub(super) rel_type: String,
    #[pyo3(get)]
    pub(super) start_node: i64,
    #[pyo3(get)]
    pub(super) end_node: i64,
    /// `elementId(startNode(r))`.
    #[pyo3(get)]
    pub(super) start_node_element_id: Option<String>,
    /// `elementId(endNode(r))`.
    #[pyo3(get)]
    pub(super) end_node_element_id: Option<String>,
    pub(super) properties: Py<PyDict>,
}

#[pymethods]
impl Relationship {
    #[getter(r#type)]
    fn get_type(&self) -> String {
        self.rel_type.clone()
    }

    #[getter]
    fn properties(&self, py: Python<'_>) -> Py<PyDict> {
        self.properties.clone_ref(py)
    }

    /// The property pairs, like the driver entity's `items()`.
    fn items(&self, py: Python<'_>) -> PyResult<PyObject> {
        Ok(self.properties.bind(py).call_method0("items")?.unbind())
    }

    /// Short aliases of `start_node`/`end_node` (the integer endpoint ids),
    /// for edge-list-style consumers: `r.start`, `r.end`.
    #[getter]
    fn start(&self) -> i64 {
        self.start_node
    }

    #[getter]
    fn end(&self) -> i64 {
        self.end_node
    }

    fn __getitem__(&self, py: Python<'_>, key: &str) -> PyResult<PyObject> {
        match self.properties.bind(py).get_item(key)? {
            Some(v) => Ok(v.unbind()),
            None => Err(PyKeyError::new_err(key.to_string())),
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        match other.downcast::<Relationship>() {
            Ok(o) => o.borrow().id == self.id,
            Err(_) => false,
        }
    }

    fn __hash__(&self) -> i64 {
        self.id
    }

    fn __repr__(&self) -> String {
        format!(
            "Relationship(id={}, type={:?}, start_node={}, end_node={})",
            self.id, self.rel_type, self.start_node, self.end_node
        )
    }
}

/// A path returned from a query: alternating nodes and relationships, with
/// `path.nodes` (length n+1) and `path.relationships` (length n).
#[pyclass(module = "relate._native")]
pub(super) struct Path {
    pub(super) nodes: Vec<Py<Node>>,
    pub(super) relationships: Vec<Py<Relationship>>,
}

#[pymethods]
impl Path {
    #[getter]
    fn nodes(&self, py: Python<'_>) -> Vec<Py<Node>> {
        self.nodes.iter().map(|n| n.clone_ref(py)).collect()
    }

    #[getter]
    fn relationships(&self, py: Python<'_>) -> Vec<Py<Relationship>> {
        self.relationships.iter().map(|r| r.clone_ref(py)).collect()
    }

    fn __len__(&self) -> usize {
        self.relationships.len()
    }

    fn __repr__(&self) -> String {
        format!(
            "Path(nodes={}, relationships={})",
            self.nodes.len(),
            self.relationships.len()
        )
    }
}

/// A spatial point. Cartesian points expose `x`/`y`/`z`; geographic points also
/// expose `longitude`/`latitude`/`height` aliases. `srid` and `crs` identify
/// the coordinate reference system. A `Point` round-trips: it is accepted as a
/// query parameter and returned from a query.
#[pyclass(module = "relate._native")]
#[derive(Clone)]
pub(super) struct Point {
    #[pyo3(get)]
    pub(super) srid: i32,
    #[pyo3(get)]
    pub(super) x: f64,
    #[pyo3(get)]
    pub(super) y: f64,
    #[pyo3(get)]
    pub(super) z: Option<f64>,
    #[pyo3(get)]
    pub(super) crs: String,
}

#[pymethods]
impl Point {
    #[new]
    #[pyo3(signature = (srid, x, y, z=None))]
    pub(super) fn new(srid: i32, x: f64, y: f64, z: Option<f64>) -> Self {
        Point {
            srid,
            x,
            y,
            z,
            crs: relate_model::wire::crs_name(srid).to_string(),
        }
    }

    #[getter]
    fn longitude(&self) -> f64 {
        self.x
    }

    #[getter]
    fn latitude(&self) -> f64 {
        self.y
    }

    #[getter]
    fn height(&self) -> Option<f64> {
        self.z
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        match other.downcast::<Point>() {
            Ok(o) => {
                let o = o.borrow();
                o.srid == self.srid && o.x == self.x && o.y == self.y && o.z == self.z
            }
            Err(_) => false,
        }
    }

    fn __repr__(&self) -> String {
        match self.z {
            Some(z) => format!(
                "Point(srid={}, x={}, y={}, z={})",
                self.srid, self.x, self.y, z
            ),
            None => format!("Point(srid={}, x={}, y={})", self.srid, self.x, self.y),
        }
    }
}
