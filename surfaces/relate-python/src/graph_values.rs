//! The rich graph-value pyclasses returned from a query: [`Node`],
//! [`Relationship`], [`Path`] and [`Point`].
//!
//! `convert` builds these, so their fields are crate-visible; the pymodule
//! registers the classes from the crate root.

use std::collections::HashSet;

use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::convert::{enter_container, leave_container, PY_VALUE_DEPTH_CAP};

/// A copy of a property value with every list and map copied down the tree,
/// so mutating a returned `data()` / `to_dict()` never reaches the row. Other
/// values (scalars, points, vectors, temporals) are immutable and shared.
/// Nesting deeper than [`PY_VALUE_DEPTH_CAP`] and self-referential lists or
/// maps raise `ValueError` instead of overflowing the stack.
fn copy_value<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    copy_value_at(value, 0, &mut HashSet::new())
}

fn copy_value_at<'py>(
    value: &Bound<'py, PyAny>,
    depth: usize,
    visiting: &mut HashSet<usize>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    if let Ok(list) = value.downcast::<PyList>() {
        within_container(value, depth, visiting, |visiting| {
            let out = PyList::empty(py);
            for item in list.iter() {
                out.append(copy_value_at(&item, depth + 1, visiting)?)?;
            }
            Ok(out.into_any())
        })
    } else if let Ok(dict) = value.downcast::<PyDict>() {
        within_container(value, depth, visiting, |visiting| {
            let out = PyDict::new(py);
            for (k, v) in dict.iter() {
                out.set_item(k, copy_value_at(&v, depth + 1, visiting)?)?;
            }
            Ok(out.into_any())
        })
    } else {
        Ok(value.clone())
    }
}

/// Run `copy` with `container` on the recursion path, after the depth and
/// cycle checks, and take it off the path again however `copy` ends.
fn within_container<'py>(
    container: &Bound<'py, PyAny>,
    depth: usize,
    visiting: &mut HashSet<usize>,
    copy: impl FnOnce(&mut HashSet<usize>) -> PyResult<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    if depth > PY_VALUE_DEPTH_CAP {
        return Err(PyValueError::new_err(format!(
            "property nesting exceeds the maximum depth of {PY_VALUE_DEPTH_CAP}"
        )));
    }
    enter_container(container, visiting)?;
    let copied = copy(visiting);
    leave_container(container, visiting);
    copied
}

/// An entity's property dict, copied with [`copy_value`].
fn copy_properties<'py>(py: Python<'py>, properties: &Py<PyDict>) -> PyResult<Bound<'py, PyDict>> {
    Ok(copy_value(properties.bind(py).as_any())?.downcast_into::<PyDict>()?)
}

/// A graph node returned from a query. Exposes its integer `id`, its
/// `element_id` (the engine's `elementId()` string), `labels`, and
/// `properties` (an ordered dict). Property values are accessible either via
/// `node.properties["key"]` or the shorthand `node["key"]`.
#[pyclass(module = "relate._native")]
pub(super) struct Node {
    #[pyo3(get)]
    pub(super) id: i64,
    /// Exactly what Cypher `elementId()` returns for this node (ADR-0502 §6,
    /// `4:rl-id:n<id>`), so `WHERE elementId(n) = $id` round-trips. Never
    /// `None`: the no-segment namespace (`system`) fails the conversion
    /// with the same error `elementId()` itself gives.
    #[pyo3(get)]
    pub(super) element_id: String,
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

    /// The property keys, so `dict(node)` is the property dict (the Neo4j
    /// driver's behavior) and `**node` works.
    fn keys(&self, py: Python<'_>) -> PyResult<PyObject> {
        Ok(self.properties.bind(py).call_method0("keys")?.unbind())
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

    /// The node as a plain `dict` of its properties (a copy), like the Neo4j
    /// driver's `Record.data()` renders a node. Ids and labels are left out;
    /// `to_dict()` keeps them.
    fn data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        copy_properties(py, &self.properties)
    }

    /// Everything the node holds as a plain, JSON-ready `dict`: `id`,
    /// `element_id`, `labels` and a copy of `properties`.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(py);
        out.set_item("id", self.id)?;
        out.set_item("element_id", &self.element_id)?;
        out.set_item("labels", &self.labels)?;
        out.set_item("properties", self.data(py)?)?;
        Ok(out)
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
    /// (ADR-0502 §6, `5:rl-id:r<id>`). Never `None`: the no-segment
    /// namespace fails the conversion, as for `Node::element_id`.
    #[pyo3(get)]
    pub(super) element_id: String,
    pub(super) rel_type: String,
    #[pyo3(get)]
    pub(super) start_node: i64,
    #[pyo3(get)]
    pub(super) end_node: i64,
    /// `elementId(startNode(r))`. Never `None`, like `element_id`.
    #[pyo3(get)]
    pub(super) start_node_element_id: String,
    /// `elementId(endNode(r))`. Never `None`, like `element_id`.
    #[pyo3(get)]
    pub(super) end_node_element_id: String,
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

    /// The property keys, so `dict(relationship)` is the property dict (the
    /// Neo4j driver's behavior) and `**relationship` works.
    fn keys(&self, py: Python<'_>) -> PyResult<PyObject> {
        Ok(self.properties.bind(py).call_method0("keys")?.unbind())
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

    /// The relationship as a plain `dict` of its properties (a copy). Type
    /// and endpoints are left out; `to_dict()` keeps them.
    fn data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        copy_properties(py, &self.properties)
    }

    /// Everything the relationship holds as a plain, JSON-ready `dict`:
    /// `id`, `element_id`, `type`, `start_node`, `end_node`,
    /// `start_node_element_id`, `end_node_element_id` and a copy of
    /// `properties`.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(py);
        out.set_item("id", self.id)?;
        out.set_item("element_id", &self.element_id)?;
        out.set_item("type", &self.rel_type)?;
        out.set_item("start_node", self.start_node)?;
        out.set_item("end_node", self.end_node)?;
        out.set_item("start_node_element_id", &self.start_node_element_id)?;
        out.set_item("end_node_element_id", &self.end_node_element_id)?;
        out.set_item("properties", self.data(py)?)?;
        Ok(out)
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

    /// The path as the Neo4j driver's `Record.data()` renders one: a list
    /// alternating a node's property dict and the relationship type.
    /// `to_dict()` keeps ids, labels and endpoints.
    fn data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        self.check_shape()?;
        let out = PyList::empty(py);
        let (first, rest) = self.nodes.split_first().expect("check_shape: one node");
        out.append(first.borrow(py).data(py)?)?;
        for (rel, node) in self.relationships.iter().zip(rest) {
            out.append(rel.borrow(py).rel_type.as_str())?;
            out.append(node.borrow(py).data(py)?)?;
        }
        Ok(out)
    }

    /// The path as a plain, JSON-ready `dict`: `nodes` and `relationships`,
    /// each a list of the corresponding `to_dict()` dicts in path order.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let nodes = PyList::empty(py);
        for n in &self.nodes {
            nodes.append(n.borrow(py).to_dict(py)?)?;
        }
        let rels = PyList::empty(py);
        for r in &self.relationships {
            rels.append(r.borrow(py).to_dict(py)?)?;
        }
        let out = PyDict::new(py);
        out.set_item("nodes", nodes)?;
        out.set_item("relationships", rels)?;
        Ok(out)
    }
}

impl Path {
    /// A path holds one more node than relationships; a value built any other
    /// way cannot be rendered, so say so instead of indexing out of range.
    fn check_shape(&self) -> PyResult<()> {
        if self.nodes.len() == self.relationships.len() + 1 {
            Ok(())
        } else {
            Err(PyValueError::new_err(format!(
                "malformed path: {} nodes, {} relationships",
                self.nodes.len(),
                self.relationships.len()
            )))
        }
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
