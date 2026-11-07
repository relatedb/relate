//! Wide-column shard schema derivation (ADR-0534). A part's promoted
//! columns come from its newly encoded rows: a key present on at least
//! [`promotes`]'s share of them, with one consistent flat type, becomes a
//! column; every other key stays in the residual tagged-JSON column.
//! Columns carried by a previous part the unit copies rows from are added
//! by the copy path, not here — copied rows are re-read, never re-derived.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, BooleanBuilder, Float64Array, Float64Builder, Int64Array,
    Int64Builder, ListBuilder, StringArray, StringBuilder,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use relate_core::{Error, Node, PropertyMap, Relationship, Result, Value};

use crate::snapshot::Kind;

/// The flat value shapes a promoted column can carry (ADR-0534). Every
/// other `Value` variant has no flat column shape and always stays in the
/// residual column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColumnType {
    Bool,
    Int,
    Float,
    Str,
}

impl ColumnType {
    /// The one `Value`-to-column mapping. `None` = no flat column shape:
    /// either a complex value (list, map, temporal, …) or `Null`, which
    /// callers treat as absent rather than residual.
    fn of_value(value: &Value) -> Option<Self> {
        match value {
            Value::Bool(_) => Some(Self::Bool),
            Value::Int(_) => Some(Self::Int),
            Value::Float(_) => Some(Self::Float),
            Value::String(_) => Some(Self::Str),
            _ => None,
        }
    }
}

/// Whether `present` out of `rows` meets the promotion share — at least
/// half (ADR-0534). The one spelling of the threshold.
fn promotes(present: u64, rows: u64) -> bool {
    rows > 0 && present * 2 >= rows
}

/// One key's presence and type agreement over a part's encoded rows: the
/// three states of a key's flat type — none seen yet, one consistent
/// type, or unpromotable (two flat types, or a non-flat value).
#[derive(Default)]
struct KeyStat {
    present: u64,
    ty: KeyTy,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum KeyTy {
    #[default]
    Unseen,
    Flat(ColumnType),
    Mixed,
}

/// The promoted `(key, type)` columns of the rows `props_of` yields, in
/// key order (a deterministic schema). `props_of` returns each visible
/// row's properties by id; an id with no visible row is skipped and does
/// not count toward the denominator.
pub(super) fn derive_columns(
    ids: &[i64],
    mut props_of: impl FnMut(i64) -> Option<Arc<PropertyMap>>,
) -> Vec<(String, ColumnType)> {
    let mut rows = 0u64;
    let mut stats: BTreeMap<String, KeyStat> = BTreeMap::new();
    for &id in ids {
        let Some(props) = props_of(id) else {
            continue;
        };
        rows += 1;
        for (key, value) in props.iter() {
            // Null is the column's own absent value: it neither counts
            // toward presence nor blocks promotion of the typed rows.
            if matches!(value, Value::Null) {
                continue;
            }
            let stat = stats.entry(key.clone()).or_default();
            stat.present += 1;
            stat.ty = match (stat.ty, ColumnType::of_value(value)) {
                (KeyTy::Unseen, Some(ty)) => KeyTy::Flat(ty),
                (KeyTy::Flat(ty), Some(seen)) if ty == seen => KeyTy::Flat(ty),
                (KeyTy::Mixed, _) | (_, None) | (KeyTy::Flat(_), Some(_)) => KeyTy::Mixed,
            };
        }
    }
    stats
        .into_iter()
        .filter_map(|(key, stat)| match stat.ty {
            // A key over the byte bound stays in the residual column: its
            // name would repeat through the file's metadata
            // ([`MAX_PROMOTED_KEY_BYTES`]).
            KeyTy::Flat(ty)
                if promotes(stat.present, rows) && key.len() <= MAX_PROMOTED_KEY_BYTES =>
            {
                Some((key, ty))
            }
            _ => None,
        })
        .collect()
}

/// Prefix every promoted column name carries, so a property key can never
/// collide with a reserved column (`id`, `labels`, `type`, `start_id`,
/// `end_id`, `properties`) — a key named `id` or `properties` is
/// representable (ADR-0534).
pub(super) const COLUMN_PREFIX: &str = "p_";

/// The most promoted columns one part carries (ADR-0534). A part's leaf
/// columns are its kind's reserved leaves plus at most this many plus the
/// residual, which keeps the ADR-0086 page envelope a bounded, plannable
/// row count; keys beyond the cap stay residual.
pub(crate) const MAX_PROMOTED_COLUMNS: usize = 32;

/// The most bytes a promoted column's key carries (ADR-0534). A key's
/// bytes become the column's name and repeat in every row group's
/// metadata, so an unbounded key could push a one-row shard's footer
/// past the cap every candidate is preflighted against — and a one-row
/// file cannot be halved, so its checkpoint would fail forever. Keys
/// over the bound stay in the residual column, and a part claiming one
/// is refused on read.
pub(crate) const MAX_PROMOTED_KEY_BYTES: usize = 64 * 1024;

/// One part's columns: the previous parts' carried columns — a copied row
/// keeps its values only through them — plus the keys the part's encoded
/// rows promote and the carried set does not already hold, at most
/// [`MAX_PROMOTED_COLUMNS`] together, in key order. A carried key meeting
/// new values of another type keeps its column: those values take the
/// residual column per row, the copied rows keep theirs.
pub(super) fn merge_columns(
    carried: &[(String, ColumnType)],
    derived: Vec<(String, ColumnType)>,
) -> Vec<(String, ColumnType)> {
    // Carried keys are mandatory: a copied row's values live only in
    // their columns. They are all kept — the caller never merges a
    // carried set over the cap, the invariant MAX_PROMOTED_COLUMNS exists
    // to keep enforceable — and derived keys fill what is left, in key
    // order.
    assert!(
        carried.len() <= MAX_PROMOTED_COLUMNS,
        "a carried column set over the cap breaks the page envelope's row bound"
    );
    let held: BTreeSet<&str> = carried.iter().map(|(key, _)| key.as_str()).collect();
    let mut merged: Vec<(String, ColumnType)> = carried.to_vec();
    for (key, ty) in derived {
        if merged.len() < MAX_PROMOTED_COLUMNS && !held.contains(key.as_str()) {
            merged.push((key, ty));
        }
    }
    merged.sort_by(|a, b| a.0.cmp(&b.0));
    merged
}

impl ColumnType {
    /// The Arrow type this column is written as.
    fn data_type(self) -> DataType {
        match self {
            Self::Bool => DataType::Boolean,
            Self::Int => DataType::Int64,
            Self::Float => DataType::Float64,
            Self::Str => DataType::Utf8,
        }
    }

    /// The inverse of [`Self::data_type`]: the flat type a column of
    /// `data_type` carries, `None` for anything that is not a promoted
    /// column shape.
    fn of_data_type(data_type: &DataType) -> Option<Self> {
        match data_type {
            DataType::Boolean => Some(Self::Bool),
            DataType::Int64 => Some(Self::Int),
            DataType::Float64 => Some(Self::Float),
            DataType::Utf8 => Some(Self::Str),
            _ => None,
        }
    }
}

/// The wide schema of one part: the kind's fixed columns, the promoted
/// property columns in `columns` order, then the residual tagged-JSON
/// column. Always carries the residual column — near-empty is the common
/// case, and a uniform schema keeps the reader one path (ADR-0534).
pub(super) fn wide_schema(kind: Kind, columns: &[(String, ColumnType)]) -> Arc<Schema> {
    let mut fields = match kind {
        Kind::Node => vec![
            Field::new("id", DataType::Int64, false),
            Field::new(
                "labels",
                DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
                true,
            ),
        ],
        Kind::Rel => vec![
            Field::new("id", DataType::Int64, false),
            Field::new("start_id", DataType::Int64, false),
            Field::new("end_id", DataType::Int64, false),
            Field::new("type", DataType::Utf8, true),
        ],
    };
    for (key, ty) in columns {
        fields.push(Field::new(
            format!("{COLUMN_PREFIX}{key}"),
            ty.data_type(),
            true,
        ));
    }
    fields.push(Field::new("properties", DataType::Utf8, true));
    Arc::new(Schema::new(fields))
}

/// The kind's reserved head width: the fields of [`Kind::schema`] minus
/// its trailing residual — the one derivation every wide-shape reader
/// consults, never restated as a literal.
pub(super) fn reserved_width(kind: Kind) -> usize {
    kind.schema().fields().len() - 1
}

/// Recover the promoted columns from a part's schema — the inverse of
/// [`wide_schema`], and the one place the wide shape is read back. `None`
/// when `schema` is not a valid wide schema for `kind`: the head must be
/// the kind's reserved columns exactly, every middle column must be a
/// prefixed flat column with a unique key, and the last must be the
/// residual column.
pub(super) fn recover_columns(kind: Kind, schema: &Schema) -> Option<Vec<(String, ColumnType)>> {
    let fixed = kind.schema();
    let reserved = reserved_width(kind);
    let fields = schema.fields();
    if fields.len() < reserved + 1 {
        return None;
    }
    let (head, tail) = fields.split_at(reserved);
    // One `Field` equality — the contract the copy path and the shard
    // writer already run — so a part whose reserved columns differ in
    // name, Arrow type or nullability does not pass as the kind's schema.
    if !head
        .iter()
        .zip(fixed.fields())
        .all(|(field, canonical)| field == canonical)
    {
        return None;
    }
    let (middle, residual) = tail.split_at(tail.len() - 1);
    let residual_of = fixed.fields().last()?;
    if residual[0] != *residual_of {
        return None;
    }
    // The writer's bounds, held on read too (ADR-0086): a part over
    // either is malformed, never a wider one to admit.
    if middle.len() > MAX_PROMOTED_COLUMNS {
        return None;
    }
    let mut columns: Vec<(String, ColumnType)> = Vec::with_capacity(middle.len());
    let mut seen = BTreeSet::new();
    for field in middle {
        // An empty key is representable — its column is the bare prefix —
        // because `SET n += $map` can set one and the checkpoint must
        // re-open.
        let key = field.name().strip_prefix(COLUMN_PREFIX)?;
        if key.len() > MAX_PROMOTED_KEY_BYTES {
            return None;
        }
        if !seen.insert(key.to_string()) {
            return None;
        }
        let ty = ColumnType::of_data_type(field.data_type())?;
        columns.push((key.to_string(), ty));
    }
    Some(columns)
}

/// Adapt a copied batch into the target part's schema (ADR-0534): a
/// column the source holds by name carries its rows over — a copied row
/// keeps its values, its promoted columns and its residual text alike —
/// and a target column the source lacks arrives null, the
/// carried-forward slots of rows the source part never held. A same-named
/// column of another type, or a source column the target lacks, is a
/// typed refusal: the copy path re-reads a value, never re-derives it.
pub(super) fn adapt_copied_batch(batch: &RecordBatch, schema: &Schema) -> Result<RecordBatch> {
    let mut columns = Vec::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let column = match batch.column_by_name(field.name()) {
            Some(column) if column.data_type() == field.data_type() => Arc::clone(column),
            // Only a promoted column may be absent from the source — the
            // carried-forward slots of rows the source part never held; a
            // missing reserved or residual column would null the row's
            // identity or its unpromoted values, and is the same typed
            // refusal as a type mismatch (`ShardRules::open` keeps it
            // unreachable for a valid part).
            None if field.name().strip_prefix(COLUMN_PREFIX).is_some() => {
                arrow::array::new_null_array(field.data_type(), batch.num_rows())
            }
            _ => {
                return Err(Error::storage(format!(
                    "copied column {} does not match the part's column",
                    field.name()
                )))
            }
        };
        columns.push(column);
    }
    // A source column the part's schema lacks has nowhere to carry over —
    // its values live only in that column — so the copy refuses it instead
    // of dropping it: in the wide path the carried set covers every copy
    // source, and a legacy target holds no promoted columns at all.
    if let Some(stray) = batch
        .schema()
        .fields()
        .iter()
        .find(|field| schema.field_with_name(field.name()).is_err())
    {
        return Err(Error::storage(format!(
            "copied column {} is not in the part's column set",
            stray.name()
        )));
    }
    RecordBatch::try_new(Arc::new(schema.clone()), columns)
        .map_err(|e| Error::storage(e.to_string()))
}

/// One promoted column's builder: the append-and-finish half of the
/// `ColumnType` mapping, so the write path never matches on the type twice.
enum ColumnBuilder {
    Bool(BooleanBuilder),
    Int(Int64Builder),
    Float(Float64Builder),
    Str(StringBuilder),
}

impl ColumnBuilder {
    fn new(ty: ColumnType, rows: usize) -> Self {
        match ty {
            ColumnType::Bool => Self::Bool(BooleanBuilder::with_capacity(rows)),
            ColumnType::Int => Self::Int(Int64Builder::with_capacity(rows)),
            ColumnType::Float => Self::Float(Float64Builder::with_capacity(rows)),
            ColumnType::Str => Self::Str(StringBuilder::with_capacity(rows, rows * 16)),
        }
    }

    /// A null slot for the row, the one spelling of the four-arm null
    /// append.
    fn append_null(&mut self) {
        match self {
            Self::Bool(b) => b.append_null(),
            Self::Int(b) => b.append_null(),
            Self::Float(b) => b.append_null(),
            Self::Str(b) => b.append_null(),
        }
    }

    /// Append `value`'s slot for a row and report whether the column
    /// carries the key: a null for an absent (or null) property, the
    /// value itself when it matches the column's type, and — a value of
    /// another shape — a null slot with the value left for the residual
    /// column. A carried column can meet new data of another type
    /// (ADR-0534); the residual column is where such a value belongs,
    /// never a coercion.
    fn append(&mut self, value: Option<&Value>) -> bool {
        let Some(value) = value.filter(|value| !matches!(value, Value::Null)) else {
            self.append_null();
            return true;
        };
        let carried = match (&mut *self, value) {
            (Self::Bool(b), Value::Bool(v)) => {
                b.append_value(*v);
                true
            }
            (Self::Int(b), Value::Int(v)) => {
                b.append_value(*v);
                true
            }
            (Self::Float(b), Value::Float(v)) => {
                b.append_value(*v);
                true
            }
            (Self::Str(b), Value::String(v)) => {
                b.append_value(v.as_ref());
                true
            }
            // Not the column's type: the value takes the residual column.
            _ => false,
        };
        if !carried {
            // Not the column's type: the value takes the residual column.
            self.append_null();
        }
        carried
    }

    fn finish(self) -> ArrayRef {
        match self {
            Self::Bool(mut b) => Arc::new(b.finish()),
            Self::Int(mut b) => Arc::new(b.finish()),
            Self::Float(mut b) => Arc::new(b.finish()),
            Self::Str(mut b) => Arc::new(b.finish()),
        }
    }
}

/// One promoted column's decoded values (ADR-0534): the read-side half of
/// the `ColumnType` mapping, downcast once per batch. A null slot is the
/// row's absence of the key — or its rerouted value, which the residual
/// column carries.
pub(super) enum ColumnValues<'a> {
    Bool(&'a BooleanArray),
    Int(&'a Int64Array),
    Float(&'a Float64Array),
    Str(&'a StringArray),
}

impl<'a> ColumnValues<'a> {
    /// The column at `index` of `batch`, which carries `ty`'s values.
    pub(super) fn column(batch: &'a RecordBatch, index: usize, ty: ColumnType) -> Result<Self> {
        let column: &'a dyn Array = batch.column(index).as_ref();
        let wrong = || Error::storage(format!("wide column {index} does not carry {ty:?} values"));
        Ok(match ty {
            ColumnType::Bool => Self::Bool(column.as_any().downcast_ref().ok_or_else(wrong)?),
            ColumnType::Int => Self::Int(column.as_any().downcast_ref().ok_or_else(wrong)?),
            ColumnType::Float => Self::Float(column.as_any().downcast_ref().ok_or_else(wrong)?),
            ColumnType::Str => Self::Str(column.as_any().downcast_ref().ok_or_else(wrong)?),
        })
    }

    /// Row `i`'s value, or `None` for its null slot.
    pub(super) fn value(&self, i: usize) -> Option<Value> {
        match self {
            Self::Bool(a) => (!a.is_null(i)).then(|| Value::Bool(a.value(i))),
            Self::Int(a) => (!a.is_null(i)).then(|| Value::Int(a.value(i))),
            Self::Float(a) => (!a.is_null(i)).then(|| Value::Float(a.value(i))),
            Self::Str(a) => (!a.is_null(i)).then(|| Value::String(a.value(i).into())),
        }
    }

    /// Whether row `i`'s slot carries a value — the key is retained only
    /// then, so only then does the row envelope count the key's bytes.
    pub(super) fn carries(&self, i: usize) -> bool {
        match self {
            Self::Bool(a) => !a.is_null(i),
            Self::Int(a) => !a.is_null(i),
            Self::Float(a) => !a.is_null(i),
            Self::Str(a) => !a.is_null(i),
        }
    }

    /// Row `i`'s decoded value bytes, for the ADR-0086 row envelope: the
    /// flat shapes are fixed-width beside `Str`, which carries its text.
    pub(super) fn bytes(&self, i: usize) -> u64 {
        match self {
            Self::Str(a) if !a.is_null(i) => a.value(i).len() as u64,
            Self::Str(_) | Self::Bool(_) | Self::Int(_) | Self::Float(_) => 8,
        }
    }
}

/// Overlay row `i`'s column values onto its decoded residual map — the
/// read-side counterpart of [`place_columns`]: a column speaks for its
/// key on the rows it carries, a null slot leaves the residual's entry,
/// which is where a rerouted or unpromoted value lives.
pub(super) fn overlay_values(
    values: &[ColumnValues<'_>],
    properties: &mut BTreeMap<String, Value>,
    columns: &[(String, ColumnType)],
    i: usize,
) {
    for (j, (key, _)) in columns.iter().enumerate() {
        if let Some(value) = values[j].value(i) {
            properties.insert(key.clone(), value);
        }
    }
}

/// The residual tagged-JSON text of one row: the row's properties minus
/// the keys its columns speak for — `placed` holds a column's key when the
/// column took the value, held it null, or the row does not hold the key
/// at all — in the same representation the legacy column holds
/// (ADR-0534). `None` when nothing is left, which is the common case: no
/// per-row map, no per-row serialization.
fn residual_json(props: &PropertyMap, placed: &[&str]) -> Result<Option<String>> {
    // The legacy shape — no column took a value — is the whole map,
    // serialized exactly as the legacy encoder did, bytes included, and
    // without copying it per row.
    if placed.is_empty() {
        return serde_json::to_string(props)
            .map(Some)
            .map_err(|e| Error::storage(e.to_string()));
    }
    // One pass: the keys the row's columns speak for leave, everything
    // else stays, in the legacy column's representation. `None` — no
    // per-row map, no serialization — when a column took a value and
    // nothing is left.
    let mut rest = PropertyMap::new();
    for (key, value) in props.iter() {
        if !placed.contains(&key.as_str()) {
            rest.insert(key.clone(), value.clone());
        }
    }
    if rest.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(&rest)
        .map(Some)
        .map_err(|e| Error::storage(e.to_string()))
}

/// The label-list builder both kinds share: one row per node, its labels
/// in stored order.
fn labels_column(nodes: &[Node]) -> ArrayRef {
    let mut list = ListBuilder::new(StringBuilder::new());
    for node in nodes {
        for label in node.labels.iter() {
            list.values().append_value(label);
        }
        list.append(true);
    }
    Arc::new(list.finish())
}

/// The shared row loop of the wide batch builders (ADR-0534): one
/// builder per column, each row's value into its column or — a value of
/// another shape — the residual text, and the residual column itself.
fn place_columns<'p>(
    columns: &[(String, ColumnType)],
    rows: impl ExactSizeIterator<Item = &'p PropertyMap>,
) -> Result<(Vec<ColumnBuilder>, Vec<Option<String>>)> {
    let mut builders: Vec<ColumnBuilder> = columns
        .iter()
        .map(|(_, ty)| ColumnBuilder::new(*ty, rows.len()))
        .collect();
    let mut residual = Vec::with_capacity(rows.len());
    // One reusable buffer for every row's placed keys — at most the column
    // cap of them, where a linear scan beats building a set per row.
    let mut placed = Vec::with_capacity(columns.len());
    for props in rows {
        // A value the row's column cannot carry — a carried column meeting
        // new data of another type — stays in the residual text, its slot
        // null; the key is `placed` only when the column took the value.
        placed.clear();
        for ((key, _), builder) in columns.iter().zip(builders.iter_mut()) {
            if builder.append(props.get(key)) {
                placed.push(key.as_str());
            }
        }
        residual.push(residual_json(props, &placed)?);
    }
    Ok((builders, residual))
}

/// One node row group's batch under `columns` (ADR-0534): the reserved
/// head, the placed columns, the residual tail.
pub(super) fn node_wide_batch(
    columns: &[(String, ColumnType)],
    nodes: &[Node],
) -> Result<RecordBatch> {
    wide_batch(
        Kind::Node,
        columns,
        vec![
            Arc::new(Int64Array::from(
                nodes.iter().map(|n| n.id).collect::<Vec<_>>(),
            )),
            labels_column(nodes),
        ],
        nodes.iter().map(|node| node.properties.as_ref()),
    )
}

/// One relationship row group's batch under `columns` (ADR-0534).
pub(super) fn rel_wide_batch(
    columns: &[(String, ColumnType)],
    rels: &[Relationship],
) -> Result<RecordBatch> {
    wide_batch(
        Kind::Rel,
        columns,
        vec![
            Arc::new(Int64Array::from(
                rels.iter().map(|r| r.id).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rels.iter().map(|r| r.start).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rels.iter().map(|r| r.end).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rels.iter().map(|r| r.rel_type.as_str()).collect::<Vec<_>>(),
            )),
        ],
        rels.iter().map(|rel| rel.properties.as_ref()),
    )
}

/// The shared tail of the wide batch builders (ADR-0534): each row's
/// values placed under `columns` beside the kind's reserved `head`
/// arrays, the residual text column last, one batch under the kind's
/// wide schema.
fn wide_batch<'p>(
    kind: Kind,
    columns: &[(String, ColumnType)],
    head: Vec<ArrayRef>,
    rows: impl ExactSizeIterator<Item = &'p PropertyMap>,
) -> Result<RecordBatch> {
    let (builders, residual) = place_columns(columns, rows)?;
    let mut arrays = head;
    arrays.extend(builders.into_iter().map(ColumnBuilder::finish));
    arrays.push(Arc::new(StringArray::from(residual)));
    RecordBatch::try_new(wide_schema(kind, columns), arrays)
        .map_err(|e| Error::storage(e.to_string()))
}

#[cfg(test)]
mod tests;
