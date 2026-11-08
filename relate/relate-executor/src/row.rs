//! Compact query row: one shared name schema, one value per slot.
//!
//! Rows produced by the same clause share the schema `Arc`. Cloning a row
//! clones the value vector and the schema pointer, not a fresh copy of
//! every variable name.
//!
//! The crate's lowest tier: every consumer — both walk engines
//! (`pattern_walk`'s bind family among them), the exec layer's operators,
//! the workspace's accounting — imports these types downward from
//! `crate::row`, the one public path per item. Hoisted from
//! `pattern_walk/row.rs` (itself hoisted from `eval/row.rs`, LABS-668):
//! the row schema is not walk vocabulary — every clause product is a row,
//! pattern-walked or not — so its home is the crate root, not the
//! pattern-walk module that once carried it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use relate_core::Value;
use relate_names::SmallName;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowSchema {
    names: Vec<String>,
    index: HashMap<String, usize>,
}

impl RowSchema {
    fn empty() -> Arc<Self> {
        static EMPTY: OnceLock<Arc<RowSchema>> = OnceLock::new();
        Arc::clone(EMPTY.get_or_init(|| {
            Arc::new(RowSchema {
                names: Vec::new(),
                index: HashMap::new(),
            })
        }))
    }

    fn from_names(names: &[String]) -> Arc<Self> {
        let mut index = HashMap::with_capacity(names.len());
        for (slot, name) in names.iter().enumerate() {
            index.insert(name.clone(), slot);
        }
        Arc::new(Self {
            names: names.to_vec(),
            index,
        })
    }
}

/// The shared column names of one projected batch.
#[derive(Clone)]
pub struct RowNames(Arc<RowSchema>);

impl RowNames {
    pub fn new(names: &[String]) -> Self {
        Self(RowSchema::from_names(names))
    }

    /// The schema's column names, in slot order — the chunk layer's
    /// column projection reads them; every other consumer holds the names
    /// it built the schema from.
    pub(crate) fn names(&self) -> &[String] {
        &self.0.names
    }
}

/// The schema a row holds after a walk appends one pre-known set of
/// names — one rung of a walk's binding-schema ladder, [`Row::stamp`]'s
/// input. Built once per walk (the rung's names are the row's names plus
/// the appended set, in binding order); stamping any number of candidate
/// rows with it costs one `Arc` clone and the value pushes — the
/// new-name `insert` path (clone every name, intern the list in the
/// thread-local cache) never runs per candidate.
#[derive(Clone)]
pub(crate) struct RowRung(Arc<RowSchema>);

impl RowRung {
    /// The rung for a name list — walk-setup work, paid once per walk:
    /// the interned-schema cache serves repeats (a batch of walks over
    /// one entry schema asks for the same rungs).
    pub(crate) fn from_names(names: &[String]) -> Self {
        Self(interned_schema(names))
    }

    /// The rung for appending `bind_names` past `row`'s schema — the
    /// per-walk rung builds that read one row (the agg fold's hop rung,
    /// the existence probe's, the path-name bind's, the OPTIONAL-miss
    /// null binder's) instead of a pattern's whole element ladder. A
    /// name the row already binds appends nothing.
    pub(crate) fn past<'a>(
        row: &Row,
        bind_names: impl IntoIterator<Item = Option<&'a str>>,
    ) -> Self {
        let mut names: Vec<String> = row.keys().cloned().collect();
        for name in bind_names {
            push_bind_name(&mut names, name);
        }
        Self::from_names(&names)
    }
}

/// Push one bind name onto a walk's accumulated schema names unless the
/// walk already binds it — a row-bound variable appends nothing. The
/// one atom every binding-schema ladder and rung build reads, so the
/// precomputed schemas and the runtime binds agree by construction.
pub(crate) fn push_bind_name(names: &mut Vec<String>, name: Option<&str>) {
    if let Some(name) = name {
        if !names.iter().any(|bound| bound.as_str() == name) {
            names.push(name.to_string());
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    schema: Arc<RowSchema>,
    values: Vec<Value>,
}

impl Default for Row {
    fn default() -> Self {
        Self::new()
    }
}

impl Row {
    pub fn new() -> Self {
        Self {
            schema: RowSchema::empty(),
            values: Vec::new(),
        }
    }

    /// Every value sits in `names`'s order. Rows filled from the same
    /// `RowNames` share the schema allocation.
    pub fn from_names(names: &RowNames, values: Vec<Value>) -> Self {
        debug_assert_eq!(names.0.names.len(), values.len());
        Self {
            schema: Arc::clone(&names.0),
            values,
        }
    }

    /// Stamp `rung` onto this row: adopt the rung's precomputed schema —
    /// this row's names plus the rung's appended names — and push
    /// `values`, one per appended name, in the rung's order. The append
    /// path clones the schema `Arc` and pushes the values: no
    /// thread-local cache, no map, no per-name hashing. The debug check
    /// pins the prefix invariant — the rung's names must be this row's
    /// names plus the appended set — so a ladder/caller disagreement
    /// fails here instead of corrupting the row.
    pub(crate) fn stamp(&mut self, rung: &RowRung, values: impl IntoIterator<Item = Value>) {
        debug_assert!(
            self.values.len() <= rung.0.names.len()
                && rung.0.names[..self.values.len()] == self.schema.names[..],
            "the rung's schema must extend this row's schema"
        );
        self.schema = Arc::clone(&rung.0);
        self.values.extend(values);
        debug_assert_eq!(self.values.len(), self.schema.names.len());
    }

    /// Stamp `rung` onto this row with one null per appended name — the
    /// null-binding twin of [`Row::stamp`]: the rung's appended-name
    /// count (its schema's length past this row's own) is the null
    /// count, so a null-binding site (the OPTIONAL-miss binder) never
    /// re-derives the count from the row's key list beside the rung it
    /// just built. A rung that appends nothing stamps nothing.
    pub(crate) fn stamp_nulls(&mut self, rung: &RowRung) {
        let appended = rung.0.names.len() - self.values.len();
        if appended == 0 {
            return;
        }
        self.stamp(rung, std::iter::repeat_n(Value::Null, appended));
    }

    /// Remove `key` without preserving the order of the names after it.
    /// Matches `IndexMap::swap_remove`.
    pub fn swap_remove(&mut self, key: &str) -> Option<Value> {
        let slot = *self.schema.index.get(key)?;
        let last = self.values.len() - 1;
        self.schema_mut();
        let removed = self.values.swap_remove(slot);
        let name = self.schema_mut().names.swap_remove(slot);
        self.schema_mut().index.remove(&name);
        if slot != last {
            let swapped = self.schema.names[slot].clone();
            self.schema_mut().index.insert(swapped, slot);
        }
        Some(removed)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Capacity of the value buffer. The sink reserves exactly the column
    /// count; a test pins that so a size-hint-less collect cannot grow it
    /// back to the four-slot minimum.
    #[cfg(test)]
    pub(crate) fn values_capacity(&self) -> usize {
        self.values.capacity()
    }

    /// Identity of the shared schema allocation. Rows from one clause
    /// compare equal here, so a workspace walk charges that schema once.
    pub(crate) fn schema_ptr(&self) -> *const () {
        Arc::as_ptr(&self.schema) as *const ()
    }

    /// Bytes of this row's schema: the name vector, the index map, and
    /// both copies of each column name.
    pub(crate) fn schema_bytes(&self) -> u64 {
        use crate::memory::{reserved_len, OVERHEAD_MAP_ENTRY, VEC_HEADER};

        let names = self.schema.names.len();
        let mut bytes = if names == 0 {
            0
        } else {
            VEC_HEADER + reserved_len(names) as u64 * std::mem::size_of::<String>() as u64
        };
        bytes += self.schema.index.len() as u64 * OVERHEAD_MAP_ENTRY;
        for name in &self.schema.names {
            bytes = bytes.saturating_add(relate_core::str_logical_bytes(name).saturating_mul(2));
        }
        bytes
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.schema.index.get(key).map(|slot| &self.values[*slot])
    }

    /// Read a column through a [`SmallName`]: the one text bridge from
    /// the name representation to this row's String-keyed schema.
    pub fn get_name(&self, name: &SmallName) -> Option<&Value> {
        name.with_str(|text| self.get(text))
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        let slot = *self.schema.index.get(key)?;
        Some(&mut self.values[slot])
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.schema.index.contains_key(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.schema.names.iter()
    }

    /// Point every row with the same column order at one schema.
    ///
    /// A match binds each output by cloning a seed and inserting a column,
    /// and that insert copies the schema. The copies are identical, so one
    /// map serves the whole batch. A later insert still copies on write.
    pub(crate) fn share_schemas(rows: &mut [Row]) {
        let Some(first) = rows.first() else {
            return;
        };
        let names = first.schema.names.clone();
        if rows.iter().skip(1).any(|row| row.schema.names != names) {
            return;
        }
        let shared = Arc::clone(&first.schema);
        for row in rows.iter_mut().skip(1) {
            row.schema = Arc::clone(&shared);
        }
    }

    pub fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        if let Some(slot) = self.schema.index.get(&key).copied() {
            return Some(std::mem::replace(&mut self.values[slot], value));
        }
        let mut names = self.schema.names.clone();
        names.push(key);
        self.schema = interned_schema(&names);
        self.values.push(value);
        None
    }

    /// Remove `key`, keeping the order of the names that remain.
    pub fn shift_remove(&mut self, key: &str) -> Option<Value> {
        let slot = *self.schema.index.get(key)?;
        let removed = std::mem::replace(&mut self.values[slot], Value::Null);
        let keep: Vec<usize> = (0..self.values.len()).filter(|i| *i != slot).collect();
        self.rebuild(keep);
        Some(removed)
    }

    /// Move `key`'s value out, leaving null in the slot. The schema stays
    /// shared with every other row from the same clause.
    pub fn take_value(&mut self, key: &str) -> Option<Value> {
        let slot = *self.schema.index.get(key)?;
        Some(std::mem::replace(&mut self.values[slot], Value::Null))
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&String, &mut Value) -> bool) {
        let mut kept = Vec::with_capacity(self.values.len());
        for (slot, value) in self.values.iter_mut().enumerate() {
            if keep(&self.schema.names[slot], value) {
                kept.push(slot);
            }
        }
        if kept.len() != self.values.len() {
            self.rebuild(kept);
        }
    }

    pub fn entry(&mut self, key: String) -> RowEntry<'_> {
        let slot = self.schema.index.get(&key).copied();
        RowEntry {
            row: self,
            key,
            slot,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.schema.names.iter().zip(self.values.iter())
    }

    fn schema_mut(&mut self) -> &mut RowSchema {
        if Arc::get_mut(&mut self.schema).is_none() {
            self.schema = Arc::new((*self.schema).clone());
        }
        Arc::get_mut(&mut self.schema).expect("schema was just made unique")
    }

    fn rebuild(&mut self, keep: Vec<usize>) {
        let mut names = Vec::with_capacity(keep.len());
        let mut values = Vec::with_capacity(keep.len());
        for slot in keep {
            names.push(self.schema.names[slot].clone());
            values.push(std::mem::replace(&mut self.values[slot], Value::Null));
        }
        self.schema = RowSchema::from_names(&names);
        self.values = values;
    }
}

/// The row's bound-variable membership predicate — the row tier's one
/// spelling of "this name is bound in this row": a closure over the row,
/// no key cloning, read by every site that asks the question of the row
/// it walks from ([`crate::exec::pivot`]'s shared start estimates and
/// order-safety gate, [`crate::exec::expand`]'s scan-predicate compile).
/// It is row vocabulary, not start-selection vocabulary, so it lives
/// beside [`Row`] rather than in the first caller that needed it.
pub(crate) fn row_bound(row: &Row) -> impl Fn(&str) -> bool + '_ {
    |name: &str| row.get(name).is_some()
}

// Counts `interned_schema` calls: the per-row schema cost a hot path must
// not pay. Test-only, so a test can assert a path interns at setup and
// never per row.
#[cfg(test)]
thread_local! {
    static INTERNED_SCHEMA_CALLS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn interned_schema_calls() -> usize {
    INTERNED_SCHEMA_CALLS.with(std::cell::Cell::get)
}

/// One schema per column list on this thread. The remaining per-row
/// `insert` sites (comprehension bindings, the GPM walk's own binds,
/// the walks a binding-schema ladder cannot serve) build the same
/// shapes per row, and a private map per row was most of the create
/// family's resident bytes. The binding-schema ladders'
/// [`RowRung::from_names`] reads the same cache — once per walk, never
/// per candidate. The cache is cleared past 128 shapes so a long-lived
/// worker cannot keep every list it has seen.
fn interned_schema(names: &[String]) -> Arc<RowSchema> {
    #[cfg(test)]
    INTERNED_SCHEMA_CALLS.with(|calls| calls.set(calls.get() + 1));
    thread_local! {
        static CACHE: RefCell<HashMap<Vec<String>, Arc<RowSchema>>> =
            RefCell::new(HashMap::new());
    }
    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if let Some(hit) = cache.get(names) {
            return Arc::clone(hit);
        }
        let schema = RowSchema::from_names(names);
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(names.to_vec(), Arc::clone(&schema));
        schema
    })
}

pub struct RowEntry<'a> {
    row: &'a mut Row,
    key: String,
    slot: Option<usize>,
}

impl<'a> RowEntry<'a> {
    pub fn or_insert(self, value: Value) -> &'a mut Value {
        self.or_insert_with(|| value)
    }

    pub fn or_insert_with<F>(self, make: F) -> &'a mut Value
    where
        F: FnOnce() -> Value,
    {
        if let Some(slot) = self.slot {
            return &mut self.row.values[slot];
        }
        self.row.insert(self.key, make());
        let slot = self.row.values.len() - 1;
        &mut self.row.values[slot]
    }
}

impl FromIterator<(String, Value)> for Row {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        let mut row = Row::new();
        for (key, value) in iter {
            row.insert(key, value);
        }
        row
    }
}

impl<const N: usize> From<[(String, Value); N]> for Row {
    fn from(entries: [(String, Value); N]) -> Self {
        entries.into_iter().collect()
    }
}

impl std::ops::Index<&str> for Row {
    type Output = Value;

    fn index(&self, key: &str) -> &Value {
        self.get(key)
            .unwrap_or_else(|| panic!("no entry found for key {key:?}"))
    }
}

impl<'a> IntoIterator for &'a Row {
    type Item = (&'a String, &'a Value);
    type IntoIter = std::iter::Zip<std::slice::Iter<'a, String>, std::slice::Iter<'a, Value>>;

    fn into_iter(self) -> Self::IntoIter {
        self.schema.names.iter().zip(self.values.iter())
    }
}

impl IntoIterator for Row {
    type Item = (String, Value);
    type IntoIter = std::vec::IntoIter<(String, Value)>;

    fn into_iter(self) -> Self::IntoIter {
        self.schema
            .names
            .iter()
            .cloned()
            .zip(self.values)
            .collect::<Vec<_>>()
            .into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rows_inserted_with_the_same_columns_share_a_schema() {
        let mut rows = Vec::new();
        for i in 0..32 {
            let mut row = Row::new();
            row.insert("s".into(), Value::Int(i));
            rows.push(row);
        }
        let ptr = rows[0].schema_ptr();
        assert!(rows.iter().all(|row| row.schema_ptr() == ptr));
        assert_eq!(rows[7].get("s"), Some(&Value::Int(7)));
    }

    proptest! {
        #![proptest_config(crate::test_support::config_with_floor(100))]

        /// The stamping primitive is the insert path's equal, generated:
        /// over generated seed names and binding sequences, rows built by
        /// `Row::insert` and rows stamped up one binding-schema ladder
        /// hold the same names in the same order, answer the same `get()`
        /// lookups, and carry equal values — and every row stamped up one
        /// ladder shares that ladder's final schema `Arc` (`schema_ptr`).
        /// The binding names are fresh by construction (index-suffixed):
        /// the walk binds a row-bound name never — it appends nothing —
        /// so both paths apply the same append-only sequence.
        #[test]
        fn stamped_rows_match_insert_built_rows(
            seed in proptest::collection::vec(("[a-z]{1,5}", any::<i64>()), 0..8),
            binds in proptest::collection::vec(("[a-z]{1,5}", any::<i64>()), 0..12),
        ) {
            let seed: Vec<(String, Value)> = seed
                .into_iter()
                .enumerate()
                .map(|(i, (name, v))| (format!("s{name}{i}"), Value::Int(v)))
                .collect();
            let binds: Vec<(String, Value)> = binds
                .into_iter()
                .enumerate()
                .map(|(i, (name, v))| (format!("b{name}{i}"), Value::Int(v)))
                .collect();

            // the insert-built baseline
            let mut inserted = Row::new();
            for (name, value) in seed.iter().chain(binds.iter()) {
                inserted.insert(name.clone(), value.clone());
            }

            // the ladder: rung i is the schema after seed + binds[..=i]
            let mut names: Vec<String> = seed.iter().map(|(name, _)| name.clone()).collect();
            let rungs: Vec<RowRung> = binds
                .iter()
                .map(|(name, _)| {
                    push_bind_name(&mut names, Some(name));
                    RowRung::from_names(&names)
                })
                .collect();

            // several rows stamped up the one ladder
            let stamped: Vec<Row> = (0..3)
                .map(|_| {
                    let mut row = Row::new();
                    for (name, value) in &seed {
                        row.insert(name.clone(), value.clone());
                    }
                    for ((_, value), rung) in binds.iter().zip(&rungs) {
                        row.stamp(rung, [value.clone()]);
                    }
                    row
                })
                .collect();

            for row in &stamped {
                prop_assert_eq!(
                    row.keys().cloned().collect::<Vec<_>>(),
                    inserted.keys().cloned().collect::<Vec<_>>()
                );
                prop_assert_eq!(row.values(), inserted.values());
                for (name, value) in inserted.iter() {
                    prop_assert_eq!(row.get(name), Some(value));
                }
            }
            let shared = stamped[0].schema_ptr();
            prop_assert!(stamped.iter().all(|row| row.schema_ptr() == shared));
        }
    }
}
