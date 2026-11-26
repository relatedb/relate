//! [`Vector`] — the result type of a Cypher `VECTOR` value (ADR-0502 §1).
//!
//! `convert` builds these; the pymodule registers the class from the crate
//! root. A vector is its own class at every depth — inside lists, maps and
//! entity properties included — so it is never confused with a map that
//! happens to carry `type`/`dim`/`values` keys.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyList;
use relate_model::value::VectorCoordKind;
use relate_model::CypherValue;

/// The coordinate payload with the family encoded once at construction:
/// [`Coordinates::from_engine`] selects the variant from
/// `VectorCoordKind::is_integer()` at the one construction site, so every
/// later read — `values()`, `__hash__`, `to_cypher()` — matches the variant
/// instead of re-deriving `Int`/`Float` per element.
#[derive(Debug, Clone, PartialEq)]
enum Coordinates {
    /// The integer-family kinds: one `i64` per coordinate.
    Ints(Vec<i64>),
    /// The floating-point kinds: one `f64` per coordinate.
    Floats(Vec<f64>),
}

impl Coordinates {
    /// Build the payload for one engine vector. The engine guarantees the
    /// family: `values` elements are always `CypherValue::Int` exactly when
    /// `coord.is_integer()` (the `vector()` constructor is the only way to
    /// produce one and enforces it) — a violation of that invariant is the
    /// typed error below, never silently re-sniffed.
    fn from_engine(coord: VectorCoordKind, values: &[CypherValue]) -> PyResult<Self> {
        let mismatched = |expected: &str, other: &CypherValue| {
            PyRuntimeError::new_err(format!(
                "vector coordinate is not {expected}, as the {} coordinate family requires: {other:?}",
                coord.canonical_name()
            ))
        };
        if coord.is_integer() {
            let ints = values
                .iter()
                .map(|value| match value {
                    CypherValue::Int(i) => Ok(*i),
                    other => Err(mismatched("an integer", other)),
                })
                .collect::<PyResult<Vec<_>>>()?;
            Ok(Self::Ints(ints))
        } else {
            let floats = values
                .iter()
                .map(|value| match value {
                    CypherValue::Float(f) => Ok(*f),
                    other => Err(mismatched("a float", other)),
                })
                .collect::<PyResult<Vec<_>>>()?;
            Ok(Self::Floats(floats))
        }
    }

    /// The coordinates back as engine values — the parameter conversion and
    /// the `str()` rendering both read this.
    fn to_values(&self) -> Vec<CypherValue> {
        match self {
            Self::Ints(ints) => ints.iter().map(|i| CypherValue::Int(*i)).collect(),
            Self::Floats(floats) => floats.iter().map(|f| CypherValue::Float(*f)).collect(),
        }
    }

    /// The number of coordinates.
    fn len(&self) -> usize {
        match self {
            Self::Ints(ints) => ints.len(),
            Self::Floats(floats) => floats.len(),
        }
    }
}

/// Hashes as the vector's own equality demands: integer coordinates by
/// value, float coordinates by bit pattern with `-0.0` folded onto `0.0`
/// (the two compare equal); `NaN` never compares equal, so its bit pattern
/// is free to hash as-is. The policy lives on the type that encodes the
/// family, next to its other operations.
impl std::hash::Hash for Coordinates {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Ints(ints) => {
                for i in ints {
                    i.hash(state);
                }
            }
            Self::Floats(floats) => {
                for f in floats {
                    let folded = if *f == 0.0 { 0.0f64 } else { *f };
                    folded.to_bits().hash(state);
                }
            }
        }
    }
}

/// A Cypher vector: a coordinate type and its coordinates.
///
/// `coordinate_type` is the engine's canonical coordinate-type name
/// (`INTEGER8`, `INTEGER16`, `INTEGER32`, `INTEGER`, `FLOAT16`, `BFLOAT16`,
/// `FLOAT32`, `FLOAT`); `values` is the coordinate list — `int`s for the
/// integer kinds, `float`s for the floating-point kinds; `dimension` (also
/// `len()`) is its length. `str()` is the engine's own rendering
/// (`vector([1.0, 2.0], 2, FLOAT32 NOT NULL)`).
///
/// A `Vector` read from a result binds back as a query parameter unchanged.
/// It has no Python constructor: build a new vector in Cypher with
/// `vector($list, $dimension, FLOAT32)` — the engine's constructor owns the
/// coordinate validation (range, precision narrowing).
#[pyclass(module = "relate._native", frozen)]
pub(super) struct Vector {
    coord: VectorCoordKind,
    /// The coordinates with the family encoded once —
    /// [`Coordinates::Ints`] for the integer kinds,
    /// [`Coordinates::Floats`] for the floating-point kinds.
    values: Coordinates,
}

impl Vector {
    /// Build the vector from one engine value's parts. The payload family
    /// is selected from the kind here, and the fields are private, so a
    /// `Vector` can never carry a kind/payload mismatch — this is the only
    /// construction site (convert.rs).
    pub(super) fn from_engine(coord: VectorCoordKind, values: &[CypherValue]) -> PyResult<Self> {
        Ok(Self {
            coord,
            values: Coordinates::from_engine(coord, values)?,
        })
    }

    /// The engine value this vector carries — the parameter conversion and
    /// the `str()` rendering both read it.
    pub(super) fn to_cypher(&self) -> CypherValue {
        CypherValue::Vector {
            coord: self.coord,
            dim: self.values.len(),
            values: self.values.to_values(),
        }
    }
}

#[pymethods]
impl Vector {
    /// The canonical coordinate-type name (`FLOAT32`, `INTEGER8`, …).
    #[getter]
    fn coordinate_type(&self) -> &'static str {
        self.coord.canonical_name()
    }

    /// The number of coordinates.
    #[getter]
    fn dimension(&self) -> usize {
        self.values.len()
    }

    /// The coordinates as a new list (`int`s or `floats` by coordinate type).
    #[getter]
    fn values(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        let list = match &self.values {
            Coordinates::Ints(ints) => PyList::new(py, ints)?,
            Coordinates::Floats(floats) => PyList::new(py, floats)?,
        };
        Ok(list.unbind())
    }

    fn __len__(&self) -> usize {
        self.values.len()
    }

    fn __str__(&self) -> String {
        self.to_cypher().to_string()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "Vector(coordinate_type={:?}, values={})",
            self.coord.canonical_name(),
            self.values(py)?.bind(py).repr()?
        ))
    }

    /// Equal when the coordinate type and every coordinate are equal — the
    /// engine's vector equality.
    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        match other.cast::<Vector>() {
            Ok(o) => {
                let o = o.get();
                o.coord == self.coord && o.values == self.values
            }
            Err(_) => false,
        }
    }

    /// Consistent with `__eq__`: hashes the coordinate type and every
    /// coordinate (the `Hash` impl on `Coordinates` owns the per-family
    /// policy), so equal vectors hash equal (dict keys, set members).
    fn __hash__(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.coord.hash(&mut hasher);
        self.values.hash(&mut hasher);
        hasher.finish()
    }
}
