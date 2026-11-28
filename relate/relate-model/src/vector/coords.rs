//! The packed coordinate representation a `Vector` value carries
//! (ADR-0706). One `Arc`-ed slice per coordinate family, exact for every
//! `VectorCoordKind` it serves, produced only by the canonical
//! constructors (`CypherValue::vector` / `Value::vector`) — never built by
//! hand at a construction site.

use std::fmt;

use crate::value::CypherValue;
use std::sync::Arc;

/// A vector's coordinates, packed per coordinate family (ADR-0706):
///
/// - `Float16`/`BFloat16`/`Float32` → `F32`: every value of those kinds is
///   exactly representable in `f32` (10/7/23 mantissa bits, all ⊂ `f32`),
///   so the packing is lossless and the kernel's `f32 → f64` widen reads
///   the same number the per-`Value` walk read.
/// - `Float64` → `F64`.
/// - `Int8`/`Int16`/`Int32`/`Int64` → `Int`: lossless as `i64`; the kind's
///   width was validated when the coordinates were coerced.
///
/// Replaces `Vec<CypherValue>` (48 bytes per coordinate) at 4/8/8 bytes
/// per coordinate — the value *is* the packed form, so nothing scan-side
/// copies or invalidates it: a rewrite swaps the `Arc`, and the MVCC
/// version chain hands every reader the exact buffer its snapshot should
/// see.
#[derive(Debug, Clone)]
pub enum VectorCoords {
    F32(Arc<[f32]>),
    F64(Arc<[f64]>),
    Int(Arc<[i64]>),
}

/// One coordinate of a packed vector, for display and encoding: the
/// per-`Value` arms it replaces (`CypherValue::Int` / `CypherValue::Float`)
/// had distinct renderings, so the packed read keeps the distinction.
#[derive(Debug, Clone, Copy)]
pub enum Coord {
    F(f64),
    I(i64),
}

impl fmt::Display for Coord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // One rendering: `text()` is the canonical spelling, so `Display`
        // and every explicit call cannot drift.
        f.write_str(&self.text())
    }
}

/// The one coordinate equality — [`Coord::numeric_eq`], so the container
/// (`VectorCoords`' `PartialEq`) and its elements cannot disagree.
impl PartialEq for Coord {
    fn eq(&self, other: &Self) -> bool {
        Coord::numeric_eq(*self, *other)
    }
}

impl Coord {
    /// The coordinate's scalar text — the rendering the per-`Value` walk
    /// produced (`Value::Float`'s `float_text`, `Value::Int`'s
    /// `to_string`), identical for the literal and display walks.
    pub fn text(&self) -> String {
        match self {
            Coord::F(v) => crate::value::float_text(*v),
            Coord::I(i) => i.to_string(),
        }
    }

    /// The scalar numeric equality — the one elementwise rule: same forms
    /// by value, and the exact `eq_i64_f64` rule across `Int`/float (large
    /// integers stay exact past 2^53). The float arm erases the family, so
    /// the `F32` and `F64` spellings of one number compare equal and the
    /// relation stays transitive.
    pub(crate) fn numeric_eq(a: Coord, b: Coord) -> bool {
        match (a, b) {
            (Coord::I(x), Coord::I(y)) => x == y,
            (Coord::F(x), Coord::F(y)) => x == y,
            (Coord::I(x), Coord::F(y)) => crate::canonical_key::eq_i64_f64(x, y),
            (Coord::F(x), Coord::I(y)) => crate::canonical_key::eq_i64_f64(y, x),
        }
    }

    /// The coordinate's little-endian bytes — `i64`'s for an integer,
    /// `f64`'s for a float (the two share the byte mix the hash walks
    /// use, exactly as `Value::Int`/`Value::Float` always did).
    pub fn to_le_bytes(self) -> [u8; 8] {
        match self {
            Coord::I(v) => v.to_le_bytes(),
            Coord::F(v) => v.to_le_bytes(),
        }
    }
}

/// The one equality: [`VectorCoords::numeric_eq`] — the elementwise
/// numeric rule every value type's vector comparison already applies, so
/// the derived variant-strict form cannot disagree with it anywhere.
impl PartialEq for VectorCoords {
    fn eq(&self, other: &Self) -> bool {
        self.numeric_eq(other)
    }
}

/// One coordinate against its kind — the family/kind pairing and the
/// per-coordinate width/precision rules, spelled once (ADR-0706). The
/// `Int` family is legal under every kind (its equality with the float
/// spelling is the exact numeric rule): under a float kind it takes the
/// full `i64` range, under an integer kind the kind's width.
fn coord_conforms(kind: crate::value::VectorCoordKind, c: Coord) -> Result<(), String> {
    match (c, kind) {
        (Coord::I(n), kind) if kind.is_integer() => {
            if kind.holds_int(n) {
                Ok(())
            } else {
                Err(format!(
                    "coordinate {n} out of {} range",
                    kind.canonical_name()
                ))
            }
        }
        // The `Int` family under a float kind is legal at full range.
        (Coord::I(_), _) => Ok(()),
        // One float rule (F-10-1): finite, and exactly representable in
        // the kind — `is_exact` answers every float kind, the message
        // names the failure the value shows.
        (Coord::F(f), kind) if !kind.is_integer() => {
            if f.is_finite() && kind.is_exact(f) {
                Ok(())
            } else if !f.is_finite() {
                Err(format!(
                    "non-finite {} coordinate {f}",
                    kind.canonical_name()
                ))
            } else {
                Err(format!(
                    "coordinate {f} loses precision in {}",
                    kind.canonical_name()
                ))
            }
        }
        (Coord::F(f), kind) => Err(format!(
            "float coordinate {f} under integer kind {}",
            kind.canonical_name()
        )),
    }
}

/// The buffer's family, the one pairing fact the invariant checks
/// (ADR-0706): `F32` for the reduced float kinds, `F64` for FLOAT,
/// `Int` for the integer kinds.
fn family_of(coords: &VectorCoords) -> &'static str {
    match coords {
        VectorCoords::F32(_) => "F32",
        VectorCoords::F64(_) => "F64",
        VectorCoords::Int(_) => "Int",
    }
}

fn kind_family(kind: crate::value::VectorCoordKind) -> &'static str {
    use crate::value::VectorCoordKind;
    match kind {
        k if k.is_f32_family() => "F32",
        VectorCoordKind::Float64 => "F64",
        _ => "Int",
    }
}

impl VectorCoords {
    /// The `f32` family: `Float16`, `BFloat16`, `Float32`.
    pub fn from_f32(coords: impl Into<Arc<[f32]>>) -> Self {
        VectorCoords::F32(coords.into())
    }

    /// The `f64` family: `Float64`.
    pub fn from_f64(coords: impl Into<Arc<[f64]>>) -> Self {
        VectorCoords::F64(coords.into())
    }

    /// The integer family: `Int8`–`Int64`, held losslessly as `i64`.
    pub fn from_int(coords: impl Into<Arc<[i64]>>) -> Self {
        VectorCoords::Int(coords.into())
    }

    pub fn len(&self) -> usize {
        match self {
            VectorCoords::F32(xs) => xs.len(),
            VectorCoords::F64(xs) => xs.len(),
            VectorCoords::Int(xs) => xs.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The family's element width in bytes — the one width table the
    /// memory charges read, never re-spelled 4/8/8 at a use site.
    pub fn element_bytes(&self) -> u64 {
        match self {
            VectorCoords::F32(_) => 4,
            VectorCoords::F64(_) | VectorCoords::Int(_) => 8,
        }
    }

    /// The family's slice bytes — `len × element_bytes()`.
    pub fn byte_len(&self) -> u64 {
        (self.len() as u64) * self.element_bytes()
    }

    /// The family buffer's address — the dedup meters' identity key,
    /// family-agnostic so no meter enumerates the three arms.
    pub fn arc_id(&self) -> *const u8 {
        match self {
            VectorCoords::F32(xs) => xs.as_ptr() as *const u8,
            VectorCoords::F64(xs) => xs.as_ptr() as *const u8,
            VectorCoords::Int(xs) => xs.as_ptr() as *const u8,
        }
    }

    /// The family buffer's strong count — the ownership meters' holders
    /// figure, family-agnostic.
    pub fn arc_strong_count(&self) -> usize {
        match self {
            VectorCoords::F32(xs) => Arc::strong_count(xs),
            VectorCoords::F64(xs) => Arc::strong_count(xs),
            VectorCoords::Int(xs) => Arc::strong_count(xs),
        }
    }

    /// Whether this value is the family buffer's sole owner — the
    /// unique-heap meters' one test, over the one holders figure.
    pub fn is_unique(&self) -> bool {
        self.arc_strong_count() == 1
    }

    /// Every coordinate widened to `f64` — the query-side and conversion
    /// widenings' one loop.
    pub fn to_f64_vec(&self) -> Vec<f64> {
        match self {
            VectorCoords::F32(xs) => xs.iter().map(|&x| f64::from(x)).collect(),
            VectorCoords::F64(xs) => xs.to_vec(),
            VectorCoords::Int(xs) => xs.iter().map(|&x| x as f64).collect(),
        }
    }

    /// The elementwise numeric equality — [`Coord::numeric_eq`] over the
    /// two buffers, gated on length. The one vector-equality policy,
    /// shared by `CypherValue::eq` and the core value's `eq_non_null`
    /// (and by `PartialEq`).
    pub fn numeric_eq(&self, other: &VectorCoords) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .zip(other.iter())
                .all(|(a, b)| Coord::numeric_eq(a, b))
    }

    /// The `i`th coordinate, for display and encoding. Panics only when
    /// out of bounds — callers bound by [`VectorCoords::len`].
    pub fn coord_at(&self, i: usize) -> Coord {
        match self {
            VectorCoords::F32(xs) => Coord::F(xs[i] as f64),
            VectorCoords::F64(xs) => Coord::F(xs[i]),
            VectorCoords::Int(xs) => Coord::I(xs[i]),
        }
    }

    /// The `i`th coordinate widened to `f64` — the similarity kernels'
    /// one read. The per-`Value` walk this replaces could return `None`
    /// for a non-number coordinate; the packed form is homogeneous and
    /// numeric by construction, so the skip contract it fed cannot arise.
    /// Panics only when out of bounds.
    pub fn coord_f64(&self, i: usize) -> f64 {
        match self {
            VectorCoords::F32(xs) => xs[i] as f64,
            VectorCoords::F64(xs) => xs[i],
            VectorCoords::Int(xs) => xs[i] as f64,
        }
    }

    /// Every coordinate in order, for encoding walks (serde, wire).
    pub fn iter(&self) -> impl Iterator<Item = Coord> + '_ {
        let len = self.len();
        (0..len).map(move |i| self.coord_at(i))
    }

    /// Decoded or coerced coordinates packed per family — the ONE
    /// family-selection policy (ADR-0706): every bridge that holds
    /// already-converted coordinates (the serde decoder, the core JSON
    /// codecs, the FFI parameter bridge, `vector()`'s coercion) classifies
    /// its elements and lands here, so the family mapping, the empty
    /// vector, and the one-family-per-vector rule have one spelling.
    /// `None` when an element does not conform to the kind or the
    /// coordinates mix families (the caller maps that to its own typed
    /// error); the canonical constructors re-assert the invariant
    /// regardless.
    ///
    /// The `Coord::I` token under a float kind packs as the `Int` family
    /// at full `i64` range (its equality with the float spelling is the
    /// exact numeric rule, which widening would round away past 2^53).
    pub fn from_coords(
        coord: crate::value::VectorCoordKind,
        coords: impl Iterator<Item = Coord>,
    ) -> Option<VectorCoords> {
        use crate::value::VectorCoordKind;
        let tokens = coords.collect::<Vec<_>>();
        // One verdict per coordinate — `coord_conforms` owns the rule;
        // this policy adds only the family selection on top.
        for c in &tokens {
            coord_conforms(coord, *c).ok()?;
        }
        let mut ints: Option<Vec<i64>> = None;
        let mut floats: Option<Vec<f64>> = None;
        for c in tokens {
            match c {
                Coord::I(n) => ints.get_or_insert_with(Vec::new).push(n),
                Coord::F(f) => floats.get_or_insert_with(Vec::new).push(f),
            }
        }
        match (ints, floats) {
            // The empty vector takes its kind's own family.
            (None, None) if coord == VectorCoordKind::Float64 => {
                Some(VectorCoords::from_f64(Vec::new()))
            }
            (None, None) if coord.is_integer() => Some(VectorCoords::from_int(Vec::new())),
            (None, None) => Some(VectorCoords::from_f32(Vec::new())),
            (Some(xs), None) => Some(VectorCoords::from_int(xs)),
            (None, Some(xs)) if coord == VectorCoordKind::Float64 => {
                Some(VectorCoords::from_f64(xs))
            }
            (None, Some(xs)) => Some(VectorCoords::from_f32(
                xs.iter().map(|&f| f as f32).collect::<Vec<_>>(),
            )),
            // The engine writes homogeneous coordinates — a family mix is
            // a crafted document.
            _ => None,
        }
    }

    /// [`from_coords`](VectorCoords::from_coords) over already-coerced
    /// `CypherValue`s — the FFI parameter bridge's thin adapter. A
    /// non-number element rejects directly (`None`), never through a
    /// sentinel the policy would have to reject incidentally.
    pub fn from_cypher_values(
        coord: crate::value::VectorCoordKind,
        values: &[CypherValue],
    ) -> Option<VectorCoords> {
        let tokens = values
            .iter()
            .map(|v| match v {
                CypherValue::Int(n) => Some(Coord::I(*n)),
                CypherValue::Float(f) => Some(Coord::F(*f)),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        Self::from_coords(coord, tokens.into_iter())
    }

    /// The family/kind pairing and per-coordinate exactness invariant the
    /// canonical constructors assert (ADR-0706) — one spelling shared by
    /// the model and core constructors. `Err` carries the invariant
    /// message; user-input rejection stays typed at the coercion
    /// boundaries, so this firing is an engine bug, not a data condition.
    ///
    /// The `Int` family is legal under *every* kind, not only the integer
    /// kinds: a float-kind vector may carry integer coordinates (the
    /// decoded form of integers a persisted store held), and its equality
    /// with the float-family vector of the same numbers is the exact
    /// numeric elementwise rule (`eq_i64_f64`), which widening a large
    /// `i64` to `f64` would round away. Range is still the kind's own
    /// (`holds_int`).
    /// Assert this buffer conforms to `coord` — the constructors' one
    /// panicking invariant check (the data-side rejection shape is
    /// [`VectorCoords::from_coords`]'s `Option`, the only fallible form).
    pub fn assert_conforms(&self, coord: crate::value::VectorCoordKind) {
        if let Err(message) = self.invariant_violation(coord) {
            panic!("{message}");
        }
    }

    pub fn invariant_violation(&self, coord: crate::value::VectorCoordKind) -> Result<(), String> {
        // The buffer/kind pairing first — a float family under the wrong
        // float kind (an `F32` buffer under FLOAT, `F64` under FLOAT32)
        // is a family mismatch even when every coordinate survives the
        // kind's precision check. The `Int` family is legal under every
        // kind (its equality with the float spelling is the exact
        // numeric rule), so only float buffers are gated.
        if family_of(self) != kind_family(coord) && family_of(self) != "Int" {
            return Err(format!(
                "vector coordinate family does not match kind {}",
                coord.canonical_name()
            ));
        }
        for c in self.iter() {
            coord_conforms(coord, c)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_family_round_trips_losslessly() {
        let coords = VectorCoords::from_f32(vec![0.25f32, -1.5, 3.0]);
        assert_eq!(coords.len(), 3);
        assert_eq!(coords.coord_f64(0), 0.25);
        assert_eq!(coords.coord_f64(1), -1.5);
        assert_eq!(coords.coord_f64(2), 3.0);
    }

    #[test]
    fn int_family_reads_as_f64_like_the_value_walk_did() {
        let coords = VectorCoords::from_int(vec![7i64, -2]);
        assert_eq!(coords.coord_f64(0), 7.0);
        assert_eq!(coords.coord_f64(1), -2.0);
        assert_eq!(coords.coord_at(0).to_string(), "7");
    }

    #[test]
    fn display_keeps_the_int_float_distinction() {
        let ints = VectorCoords::from_int(vec![5]);
        let floats = VectorCoords::from_f32(vec![5.0]);
        assert_eq!(ints.coord_at(0).to_string(), "5");
        assert_eq!(floats.coord_at(0).to_string(), "5.0");
    }

    #[test]
    fn equality_is_the_numeric_policy_like_the_value_walk() {
        assert_eq!(
            VectorCoords::from_f32(vec![0.5f32, 1.0]),
            VectorCoords::from_f32(vec![0.5f32, 1.0])
        );
        // The `Int` family equals its float spelling — the elementwise
        // numeric rule, not a variant-strict comparison.
        assert_eq!(
            VectorCoords::from_int(vec![1]),
            VectorCoords::from_f64(vec![1.0])
        );
    }
}
