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
use relate_model::VectorCoords;

/// The coordinate payload with the family encoded once at construction:
/// [`Coordinates::from_engine`] selects the variant from
/// `VectorCoordKind::is_integer()` at the one construction site, so every
/// later read — `values()`, `__hash__`, `to_cypher()` — matches the
/// variant instead of re-deriving `Int`/`Float` per element. The variant
/// IS the engine's packed family (ADR-0706); the declared kind is carried
/// beside it.
#[derive(Debug, Clone, PartialEq)]
enum Coordinates {
    /// The packed `Int` family: one `i64` per coordinate (legal under a
    /// float kind too — its equality with the float spelling is the exact
    /// numeric rule, ADR-0706).
    Ints(Vec<i64>),
    /// The packed float families (`F32`/`F64`): one `f64` per coordinate.
    Floats(Vec<f64>),
}

impl Coordinates {
    /// Build the payload for one engine vector (ADR-0706's packed read):
    /// the payload variant IS the family — selected from the packed
    /// variant alone, the kind carried beside it by the caller. The `Int`
    /// family under a float kind is a legal value (its equality with the
    /// float spelling is the exact numeric rule), so it surfaces as
    /// `Ints` and `to_cypher` round-trips it rather than failing closed.
    fn from_engine(coords: &VectorCoords) -> Self {
        match coords {
            VectorCoords::Int(xs) => Self::Ints(xs.to_vec()),
            // The F32 family's coordinates widen to the floats Python reads.
            VectorCoords::F32(xs) => Self::Floats(xs.iter().map(|&x| f64::from(x)).collect()),
            VectorCoords::F64(xs) => Self::Floats(xs.to_vec()),
        }
    }

    /// The coordinates back as engine values — the parameter conversion
    /// and the `str()` rendering both read this.
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
    pub(super) fn from_engine(coord: VectorCoordKind, coords: &VectorCoords) -> Self {
        Self {
            coord,
            values: Coordinates::from_engine(coords),
        }
    }

    /// The payload's numeric key — each coordinate tagged by its numeric
    /// domain (integral values key as their `i64`, others by float bit
    /// pattern), the same normalization the engine's equivalence keys
    /// use. Tagging keeps the two domains from colliding, so `__eq__`
    /// and `__hash__` agree with each other and with Cypher `=`; it is
    /// Rust-internal (not a Python attribute).
    fn numeric_key(&self) -> Vec<(u8, u64)> {
        match &self.values {
            Coordinates::Ints(ints) => ints.iter().map(|&i| (0u8, i as u64)).collect(),
            Coordinates::Floats(floats) => floats
                .iter()
                .map(|&f| match relate_model::canonical_key::f64_exact_i64(f) {
                    Some(i) => (0u8, i as u64),
                    None => (1u8, f.to_bits()),
                })
                .collect(),
        }
    }

    /// The engine value this vector carries — the parameter conversion and
    /// the `str()` rendering both read it.
    pub(super) fn to_cypher(&self) -> CypherValue {
        // The one packing policy (ADR-0706): the payload's family matches
        // the kind by construction (from_engine selected it), so the
        // canonical constructor accepts — this expect is the invariant
        // spelling, not a handled failure.
        let coords = VectorCoords::from_cypher_values(self.coord, &self.values.to_values())
            .expect("from_engine pairs the payload family with the kind");
        CypherValue::vector(self.coord, coords)
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
                // The engine's one vector-equality rule (ADR-0706): the
                // kind gate plus the numeric comparison over the payload
                // key, so an `Int` payload equals its float spelling
                // exactly as `=` does in Cypher — and `__hash__` reads
                // the same key, so the two operators cannot disagree.
                self.coord == o.coord && self.numeric_key() == o.numeric_key()
            }
            Err(_) => false,
        }
    }

    /// Consistent with `__eq__`: hashes the coordinate type and the same
    /// numeric key equality compares, so equal vectors hash equal (dict
    /// keys, set members).
    fn __hash__(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.coord.hash(&mut hasher);
        for key in self.numeric_key() {
            key.hash(&mut hasher);
        }
        hasher.finish()
    }
}
