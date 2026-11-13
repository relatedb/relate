//! ADR-0468's structure-of-arrays chunk: the substrate's type layer.
//!
//! A [`Chunk`] stores a batch of rows column-major — one typed buffer per
//! column plus a null bitmap per column — so clause interiors can run
//! tight loops over homogeneous buffers instead of re-dispatching on
//! `Value` variants row by row. `Row` stays the boundary representation:
//! conversion is lossless in both directions (every `CypherValue`, nulls
//! included, survives [`Chunk::from_rows`] followed by [`Chunk::to_rows`]),
//! which is the invariant pipeline breakers and not-yet-migrated operators
//! rely on while interiors migrate (ADR-0468).
//!
//! Scope of this layer: the types, the conversion, and the batch-size
//! constant, plus the operator-serving constructions ([`Column::from_values`],
//! [`Chunk::from_columns`]) and selections ([`Chunk::retain_rows`],
//! [`Chunk::select_columns`]) the scan/filter/project chain
//! (`exec/batch.rs`, ADR-0468's scan-filter-batch-operators node) reads —
//! and, since LABS-645 (ADR-0468 layer 2), each column's derived zone
//! metadata ([`ColumnZone`]): null-presence and the typed kinds'
//! min/max, folded at construction by the one [`zone_of`] spelling.
//!
//! Two facts of the layer sit ahead of their first engine consumer, each
//! marked where it is declared: [`Column::data`] and the `Row`-side
//! conversion [`Chunk::from_rows`] (an operator that *receives* boundary
//! rows reads it — the batch chain starts from the store and decodes at
//! the breaker). Their round-trip suites are the exercise until then,
//! item-scoped — never a blanket allow over the module the batch chain
//! already consumes.

use std::cmp::Ordering;
use std::sync::Arc;

use relate_core::Value;

use crate::row::{Row, RowNames};

/// Rows per chunk (ADR-0468): the one batch-size knob, mid way in the
/// ADR's 256–1024 band. The conversion entry points accept any length, because the batch a
/// pipeline breaker hands over is whatever the clause produced.
pub(crate) const CHUNK_ROWS: usize = 512;

/// One null bit per row per column, bit-packed: set means the slot is
/// null and the buffer's entry at that index is a placeholder, never a
/// value. Every read of a slot consults the bitmap first.
#[derive(Clone, Debug, Default)]
pub(crate) struct NullBitmap {
    words: Vec<u64>,
    len: usize,
}

impl NullBitmap {
    /// The empty bitmap over `len` slots — `pub(super)` for the type
    /// layer's sibling test module, which builds operator-shaped columns.
    pub(super) fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(u64::BITS as usize)],
            len,
        }
    }

    /// Whether `row`'s slot is null. Panics if `row` is outside the
    /// column: an out-of-range bit would otherwise read as a silent
    /// non-null.
    pub(crate) fn get(&self, row: usize) -> bool {
        assert!(
            row < self.len,
            "null bit read for row {row} of {len}",
            len = self.len
        );
        self.words[row / u64::BITS as usize] & (1 << (row % u64::BITS as usize)) != 0
    }

    /// Mark `row`'s slot null — `pub(super)` for the sibling test module,
    /// see [`NullBitmap::new`].
    pub(super) fn set(&mut self, row: usize) {
        assert!(
            row < self.len,
            "null bit set for row {row} of {len}",
            len = self.len
        );
        self.words[row / u64::BITS as usize] |= 1 << (row % u64::BITS as usize);
    }

    fn len(&self) -> usize {
        self.len
    }
}

/// One column's derived zone metadata (LABS-645, ADR-0468 layer 2): the
/// provable facts about a column's contents, folded once at construction
/// — null-presence for every column, and for the typed buffers
/// (u32/i64/f64/utf8/bool) the min/max over the non-null values.
/// Derived from the encoded buffer, never estimated: a zone fact that
/// could be wrong is a correctness bug, so the cases the ordering cannot
/// describe carry no min/max at all — a Float buffer holding NaN (which
/// orders against nothing) and the fallback buffer (no min/max over
/// heterogeneous values; null-presence only, the stated rule).
///
/// Consulted only where a predicate exists (the batch filter's skip
/// decision, `exec/batch.rs`); a predicate-free path never reads it.
/// Deliberately not a bloom filter: build+probe costs more than a plain
/// small-batch scan (the literature-anchored call, ADR-0468 layer 2).
#[derive(Clone, Debug)]
pub(crate) struct ColumnZone {
    /// Whether any slot in the column is null.
    null_present: bool,
    /// The non-null values' bounds, as [`Value`]s of the column's kind,
    /// so every consumer compares them through the engine's own
    /// comparison tables ([`Value::compare`], the shared `binary`) —
    /// never a second ordering. `None` for the fallback, a NaN-bearing
    /// Float buffer, or a typed column with no non-null value.
    min_max: Option<(Value, Value)>,
}

impl ColumnZone {
    /// Whether the column holds at least one null slot.
    pub(crate) fn null_present(&self) -> bool {
        self.null_present
    }

    /// The non-null values' bounds (`min`, `max`), when the column's kind
    /// ordered them — `None` is the no-facts zone, never an estimate.
    pub(crate) fn min_max(&self) -> Option<(&Value, &Value)> {
        self.min_max.as_ref().map(|(min, max)| (min, max))
    }
}

/// One column's buffer. The first four are ADR-0468's typed buffers;
/// [`ColumnData::Value`] is the explicit degradation target.
#[derive(Clone, Debug)]
pub(crate) enum ColumnData {
    /// `Value::Int` slots.
    Int(Vec<i64>),
    /// `Value::Float` slots.
    Float(Vec<f64>),
    /// `Value::String` slots.
    Utf8(Vec<Arc<str>>),
    /// `Value::Bool` slots.
    Bool(Vec<bool>),
    /// The fallback: one `Value` per slot, null bitmap still consulted
    /// first. Holds any column whose values fit no single typed buffer
    /// (heterogeneous columns, and the structural variants — lists, maps,
    /// temporal, points, uuids, vectors, bytes, paths, node/relationship
    /// entities) and any column with no non-null value to claim a type.
    Value(Vec<Value>),
}

/// One column of a chunk: its buffer, its null bitmap, and its derived
/// zone (LABS-645). Invariant: buffer length == bitmap length == the
/// chunk's row count, and a set null bit means the buffer slot is a
/// placeholder, never a value.
#[derive(Clone, Debug)]
pub(crate) struct Column {
    nulls: NullBitmap,
    data: ColumnData,
    zone: ColumnZone,
}

impl Column {
    /// The one assembly of a column from its finished parts: every
    /// construction — the encoder ([`encode_column`]), the filter's
    /// compaction ([`Column::retain_rows`]) — goes through here, so the
    /// zone is derived at construction by the one [`zone_of`] spelling
    /// and can never drift from the buffer beside it. Panics if the
    /// parts disagree in length: the buffer-length invariant is the
    /// column's contract, not something a caller may fabricate around.
    /// `pub(super)` for the sibling test module's operator-shaped columns.
    pub(super) fn from_parts(nulls: NullBitmap, data: ColumnData) -> Column {
        assert_eq!(
            nulls.len(),
            match &data {
                ColumnData::Int(ints) => ints.len(),
                ColumnData::Float(floats) => floats.len(),
                ColumnData::Utf8(strings) => strings.len(),
                ColumnData::Bool(bools) => bools.len(),
                ColumnData::Value(values) => values.len(),
            },
            "a column's bitmap and buffer agree in length"
        );
        let zone = zone_of(&nulls, &data);
        Column { nulls, data, zone }
    }

    /// The column's derived zone metadata (LABS-645): null-presence, and
    /// the typed kinds' min/max — consulted only where a predicate
    /// exists, never on a predicate-free path.
    pub(crate) fn zone(&self) -> &ColumnZone {
        &self.zone
    }

    /// The decoded [`Value`] at `row` — the null bitmap first, then the
    /// buffer's payload.
    pub(crate) fn value_at(&self, row: usize) -> Value {
        if self.nulls.get(row) {
            return Value::Null;
        }
        match &self.data {
            ColumnData::Int(ints) => Value::Int(ints[row]),
            ColumnData::Float(floats) => Value::Float(floats[row]),
            ColumnData::Utf8(strings) => Value::String(Arc::clone(&strings[row])),
            ColumnData::Bool(bools) => Value::Bool(bools[row]),
            ColumnData::Value(values) => values[row].clone(),
        }
    }

    /// Whether `row`'s slot is null.
    pub(crate) fn is_null(&self, row: usize) -> bool {
        self.nulls.get(row)
    }

    /// The column's buffer, for typed loops over it. Still ahead of its
    /// first engine consumer (the module doc's set): the typed-buffer
    /// fast paths the later kernel nodes build read it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn data(&self) -> &ColumnData {
        &self.data
    }

    /// Rows in the column — always the chunk's row count.
    pub(crate) fn len(&self) -> usize {
        self.nulls.len()
    }

    /// Encode one column from owned values — the scan operator's direct
    /// construction, the owned spelling of the same single encoder
    /// [`Chunk::from_rows`] reads ([`encode_column`]). The values are in
    /// row order; a diverging type mix degrades to the fallback, never to
    /// a coercion.
    pub(crate) fn from_values(values: &[Value]) -> Column {
        let refs: Vec<&Value> = values.iter().collect();
        encode_column(&refs)
    }

    /// The column restricted to the kept rows (the filter operator's
    /// compaction half): each buffer keeps `keep`'s entries in its order,
    /// and the null bitmap moves with them. The zone is re-derived over
    /// the kept rows by the same construction every column takes.
    pub(crate) fn retain_rows(&self, keep: &[usize]) -> Column {
        let mut nulls = NullBitmap::new(keep.len());
        let data = match &self.data {
            ColumnData::Int(ints) => ColumnData::Int(keep.iter().map(|&row| ints[row]).collect()),
            ColumnData::Float(floats) => {
                ColumnData::Float(keep.iter().map(|&row| floats[row]).collect())
            }
            ColumnData::Utf8(strings) => {
                ColumnData::Utf8(keep.iter().map(|&row| Arc::clone(&strings[row])).collect())
            }
            ColumnData::Bool(bools) => {
                ColumnData::Bool(keep.iter().map(|&row| bools[row]).collect())
            }
            ColumnData::Value(values) => {
                ColumnData::Value(keep.iter().map(|&row| values[row].clone()).collect())
            }
        };
        for (kept, &row) in keep.iter().enumerate() {
            if self.nulls.get(row) {
                nulls.set(kept);
            }
        }
        Column::from_parts(nulls, data)
    }
}

/// Which typed buffer a non-null `Value` encodes into, payload included:
/// the encoder's half of the Value-variant → buffer mapping. The
/// classifier reads only the tag ([`typed_kind`]) and never pays this
/// constructor's per-cell payload cost; a tie test pins the two halves
/// together so they cannot drift.
#[derive(Debug)]
// `pub(super)` pair with the `pub(super)` tag/payload readers the
// sibling test module's tie test calls.
pub(super) enum TypedSlot {
    Bool(bool),
    Int(i64),
    Float(f64),
    Utf8(Arc<str>),
}

impl TypedSlot {
    /// `#[cfg(test)]`: production's classifier reads [`typed_kind`]
    /// directly and never routes through the payload-carrying
    /// constructor; the sibling test module's tie test calls this to pin
    /// the tag and payload halves together.
    #[cfg(test)]
    pub(super) fn kind(&self) -> TypedKind {
        match self {
            TypedSlot::Bool(_) => TypedKind::Bool,
            TypedSlot::Int(_) => TypedKind::Int,
            TypedSlot::Float(_) => TypedKind::Float,
            TypedSlot::Utf8(_) => TypedKind::Utf8,
        }
    }
}

/// The tag half of [`TypedSlot`]: which of the four typed buffers a
/// column claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TypedKind {
    Bool,
    Int,
    Float,
    Utf8,
}

/// `None` = the value fits no typed buffer and forces the fallback.
/// `Value::Null` is deliberately untyped: nullness lives in the bitmap,
/// not in any buffer.
/// `pub(super)` for the sibling test module's tag/payload tie test.
pub(super) fn typed_slot(value: &Value) -> Option<TypedSlot> {
    match value {
        Value::Bool(b) => Some(TypedSlot::Bool(*b)),
        Value::Int(i) => Some(TypedSlot::Int(*i)),
        Value::Float(f) => Some(TypedSlot::Float(*f)),
        Value::String(s) => Some(TypedSlot::Utf8(Arc::clone(s))),
        _ => None,
    }
}

/// The classifier's half of the same mapping: the tag only, borrowed —
/// no `TypedSlot` construction, so classifying a `Value::String` column
/// costs zero Arc refcount ops (the payload-carrying clone is the
/// encoder's cost, paid once per cell at `push`). This match and
/// [`typed_slot`]'s are the two halves of one mapping; the tie test in
/// `chunk_tests.rs` asserts they agree on every typed variant, so a new
/// variant added to one and not the other fails loudly.
/// `pub(super)` for the sibling test module's tag/payload tie test.
pub(super) fn typed_kind(value: &Value) -> Option<TypedKind> {
    match value {
        Value::Bool(_) => Some(TypedKind::Bool),
        Value::Int(_) => Some(TypedKind::Int),
        Value::Float(_) => Some(TypedKind::Float),
        Value::String(_) => Some(TypedKind::Utf8),
        _ => None,
    }
}

/// The typed buffer a column claims, or `None` for the fallback. The
/// first non-null value claims its kind; any later non-null value that is
/// untypeable or of another kind degrades the whole column to the
/// fallback — degradation is a stated rule, never a coercion. A column
/// with no non-null value has no type to claim and lands in the fallback
/// too.
fn classify(values: &[&Value]) -> Option<TypedKind> {
    let mut claimed = None;
    for value in values {
        if value.is_null() {
            continue;
        }
        let kind = typed_kind(value)?;
        match claimed {
            None => claimed = Some(kind),
            Some(claimed) if claimed == kind => {}
            Some(_) => return None,
        }
    }
    claimed
}

/// A typed column buffer under construction.
#[derive(Debug)]
enum TypedBuffer {
    Bool(Vec<bool>),
    Int(Vec<i64>),
    Float(Vec<f64>),
    Utf8(Vec<Arc<str>>),
}

impl TypedBuffer {
    fn of(kind: TypedKind, capacity: usize) -> Self {
        match kind {
            TypedKind::Bool => TypedBuffer::Bool(Vec::with_capacity(capacity)),
            TypedKind::Int => TypedBuffer::Int(Vec::with_capacity(capacity)),
            TypedKind::Float => TypedBuffer::Float(Vec::with_capacity(capacity)),
            TypedKind::Utf8 => TypedBuffer::Utf8(Vec::with_capacity(capacity)),
        }
    }

    /// Push one encoded value. A slot of another kind is an internal
    /// coding bug — the classifier's [`typed_kind`] and this encoder's
    /// [`typed_slot`] are pinned halves of one mapping — not a data
    /// condition; a column never both claims a type and holds a
    /// mismatched value.
    fn push(&mut self, slot: TypedSlot) {
        match (self, slot) {
            (TypedBuffer::Bool(buffer), TypedSlot::Bool(b)) => buffer.push(b),
            (TypedBuffer::Int(buffer), TypedSlot::Int(i)) => buffer.push(i),
            (TypedBuffer::Float(buffer), TypedSlot::Float(f)) => buffer.push(f),
            (TypedBuffer::Utf8(buffer), TypedSlot::Utf8(s)) => buffer.push(s),
            (buffer, slot) => panic!(
                "internal: {buffer:?} received a {slot:?} slot; classify and encode disagree"
            ),
        }
    }

    /// The placeholder written under a set null bit. Never read as a
    /// value: `value_at` consults the bitmap first.
    fn push_null(&mut self) {
        match self {
            TypedBuffer::Bool(buffer) => buffer.push(false),
            TypedBuffer::Int(buffer) => buffer.push(0),
            TypedBuffer::Float(buffer) => buffer.push(0.0),
            TypedBuffer::Utf8(buffer) => buffer.push(Arc::from("")),
        }
    }

    fn finish(self) -> ColumnData {
        match self {
            TypedBuffer::Bool(buffer) => ColumnData::Bool(buffer),
            TypedBuffer::Int(buffer) => ColumnData::Int(buffer),
            TypedBuffer::Float(buffer) => ColumnData::Float(buffer),
            TypedBuffer::Utf8(buffer) => ColumnData::Utf8(buffer),
        }
    }
}

/// The min/max fold every typed buffer's zone reads (LABS-645): bounds
/// over the non-null slots, in the buffer's own ordering — which is the
/// engine's [`Value::compare`] ordering for the kind, the invariant the
/// skip decision rests on. A value that does not order against itself (a
/// NaN) voids the whole fold: the zone stays a derived fact, so it
/// carries none rather than an estimate over values the ordering cannot
/// place. No non-null slot leaves `None`: a column with nothing to order
/// has no bounds to claim.
fn fold_bounds<T: Clone + PartialOrd<T>>(slots: impl Iterator<Item = Option<T>>) -> Option<(T, T)> {
    let mut bounds: Option<(T, T)> = None;
    for slot in slots {
        let Some(value) = slot else { continue };
        // The NaN rule above, as the one check: a value that does not
        // order against itself voids the fold.
        value.partial_cmp(&value)?;
        match &mut bounds {
            None => bounds = Some((value.clone(), value)),
            Some((min, max)) => {
                if value.partial_cmp(min) == Some(Ordering::Less) {
                    *min = value.clone();
                } else if value.partial_cmp(max) == Some(Ordering::Greater) {
                    *max = value;
                }
            }
        }
    }
    bounds
}

/// The buffer's slots with the null mask applied: `None` is a null slot,
/// whose placeholder value the zone never reads. Requires the
/// buffer-length invariant (buffer and bitmap agree in length), which
/// every construction through [`Column::from_parts`] asserts.
fn slots<'a, T: Clone>(
    nulls: &'a NullBitmap,
    buffer: &'a [T],
) -> impl Iterator<Item = Option<T>> + 'a {
    buffer
        .iter()
        .enumerate()
        .map(|(row, value)| (!nulls.get(row)).then(|| value.clone()))
}

/// The one zone derivation every column construction reads (LABS-645):
/// null-presence from the bitmap, bounds from the typed buffer's
/// non-null slots. The typed kinds (u32/i64/f64/utf8/bool) report their
/// bounds as the value-space image of the buffer, so a consumer compares
/// them through the engine's own tables; the fallback keeps
/// null-presence only — no min/max over heterogeneous values, the stated
/// rule.
fn zone_of(nulls: &NullBitmap, data: &ColumnData) -> ColumnZone {
    // `set` only ever sets in-range bits, so a nonzero word is a null
    // slot — one words walk, never a per-row probe.
    let null_present = nulls.words.iter().any(|&word| word != 0);
    let min_max = match data {
        ColumnData::Int(ints) => {
            fold_bounds(slots(nulls, ints)).map(|(min, max)| (Value::Int(min), Value::Int(max)))
        }
        ColumnData::Float(floats) => fold_bounds(slots(nulls, floats))
            .map(|(min, max)| (Value::Float(min), Value::Float(max))),
        ColumnData::Utf8(strings) => fold_bounds(slots(nulls, strings))
            .map(|(min, max)| (Value::String(min), Value::String(max))),
        ColumnData::Bool(bools) => {
            fold_bounds(slots(nulls, bools)).map(|(min, max)| (Value::Bool(min), Value::Bool(max)))
        }
        // The fallback (LABS-645's stated rule): null-presence only.
        ColumnData::Value(_) => None,
    };
    ColumnZone {
        null_present,
        min_max,
    }
}

/// Encode one column's values in row order. `classify` has already run,
/// so a non-null value the typed arm cannot encode is impossible — the
/// assert states that invariant instead of silently skipping or coercing.
/// The one encoder every column construction reads: [`Chunk::from_rows`]
/// and the scan operator's [`Column::from_values`] alike.
pub(crate) fn encode_column(values: &[&Value]) -> Column {
    let mut nulls = NullBitmap::new(values.len());
    let data = match classify(values) {
        Some(kind) => {
            let mut buffer = TypedBuffer::of(kind, values.len());
            for (row, value) in values.iter().enumerate() {
                let slot = match typed_slot(value) {
                    Some(slot) => slot,
                    None => {
                        assert!(
                            value.is_null(),
                            "internal: classify() proved every untyped value is null"
                        );
                        nulls.set(row);
                        buffer.push_null();
                        continue;
                    }
                };
                buffer.push(slot);
            }
            buffer.finish()
        }
        None => ColumnData::Value(
            values
                .iter()
                .enumerate()
                .map(|(row, value)| {
                    if value.is_null() {
                        nulls.set(row);
                    }
                    (*value).clone()
                })
                .collect(),
        ),
    };
    Column::from_parts(nulls, data)
}

/// A column-major batch of rows (ADR-0468): one [`Column`] per name, all
/// buffers the same length, plus the name schema the decoded rows hand
/// back.
pub(crate) struct Chunk {
    names: RowNames,
    columns: Vec<Column>,
    rows: usize,
}

impl Chunk {
    /// Encode one projected batch. Every row must carry the same column
    /// names in the same order — rows from one clause do — so a diverging
    /// row is a caller bug and panics naming its index.
    ///
    /// Still ahead of its first engine consumer (the module doc's set):
    /// an operator that receives boundary rows reads it — this node's
    /// chain encodes from the store and decodes at the breaker.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_rows(rows: &[Row]) -> Self {
        let Some(first) = rows.first() else {
            return Self {
                names: RowNames::new(&[]),
                columns: Vec::new(),
                rows: 0,
            };
        };
        let names: Vec<String> = first.keys().cloned().collect();
        for (index, row) in rows.iter().enumerate().skip(1) {
            assert!(
                row.len() == names.len()
                    && row
                        .keys()
                        .zip(&names)
                        .all(|(name, expected)| name == expected),
                "chunk row {index} diverges from the batch's schema"
            );
        }
        let columns = (0..names.len())
            .map(|slot| {
                let values: Vec<&Value> = rows.iter().map(|row| &row.values()[slot]).collect();
                encode_column(&values)
            })
            .collect();
        Self {
            names: RowNames::new(&names),
            columns,
            rows: rows.len(),
        }
    }

    /// Decode the chunk back to boundary rows. Every row shares the
    /// chunk's schema, the way the rows of one clause do.
    pub(crate) fn to_rows(&self) -> Vec<Row> {
        (0..self.rows)
            .map(|row| {
                let values = self
                    .columns
                    .iter()
                    .map(|column| column.value_at(row))
                    .collect();
                Row::from_names(&self.names, values)
            })
            .collect()
    }

    /// Rows in the chunk.
    pub(crate) fn len(&self) -> usize {
        self.rows
    }

    /// Still ahead of its first engine consumer (the module doc's set).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// The column at `slot`. Panics if `slot` is out of range: a chunk
    /// has exactly one column per name.
    pub(crate) fn column(&self, slot: usize) -> &Column {
        &self.columns[slot]
    }

    /// Assemble a chunk from already-encoded columns — the scan
    /// operator's construction (it builds each column straight from the
    /// store instead of decoding rows first). Panics if the names and
    /// columns disagree in count, or if the columns disagree in length:
    /// the buffer-length invariant is the chunk's contract, not something
    /// a caller may fabricate around.
    pub(crate) fn from_columns(names: &[String], columns: Vec<Column>) -> Self {
        assert!(
            names.len() == columns.len()
                && columns
                    .iter()
                    .all(|c| c.len() == columns.first().map_or(0, Column::len)),
            "a chunk's names and columns agree in count and every column in length"
        );
        Self {
            names: RowNames::new(names),
            rows: columns.first().map_or(0, Column::len),
            columns,
        }
    }

    /// The chunk restricted to the kept rows (the filter operator's
    /// compaction): every column keeps `keep`'s entries, in its order.
    pub(crate) fn retain_rows(&self, keep: &[usize]) -> Self {
        Self {
            names: self.names.clone(),
            columns: self.columns.iter().map(|c| c.retain_rows(keep)).collect(),
            rows: keep.len(),
        }
    }

    /// The chunk restricted to the named-by-position columns, consumed —
    /// the projection at the pipeline breaker: the scratch columns a
    /// filter read stay behind, and no buffer is copied to drop them.
    /// Panics if a slot is out of range: a chunk has exactly one column
    /// per name.
    pub(crate) fn select_columns(self, slots: &[usize]) -> Self {
        let names: Vec<String> = slots
            .iter()
            .map(|&slot| self.names.names()[slot].clone())
            .collect();
        let mut by_slot: Vec<Option<Column>> = self.columns.into_iter().map(Some).collect();
        let columns = slots
            .iter()
            .map(|&slot| {
                by_slot[slot]
                    .take()
                    .expect("select_columns slots are in range")
            })
            .collect();
        Self {
            names: RowNames::new(&names),
            columns,
            rows: self.rows,
        }
    }
}
