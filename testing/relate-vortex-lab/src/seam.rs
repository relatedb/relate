//! The property-store seam: the trait an in-memory property representation
//! implements for the executor. Two implementations: `RowProps` here (the
//! row-oriented baseline, shaped like the engine's compact live records)
//! and `ColProps` in `colstore.rs` (the vortex columnar store). The A/B is
//! `ops::seam_bench`.
//!
//! Design notes (2026-11-23, ADR-0665):
//! - The method set is the measured workload: point (O1), morsel batch
//!   (O2/O10/O11), bulk expansion with a pushed predicate (O4/O5), sorted
//!   key seek (O3, arm A), aggregation (O6/O13), top-k/limit (O15).
//!   Presence (O16) is unit-tested, not bench-measured: no `ops` kernel
//!   drives `null_count`.
//! - `Pred` carries the whole predicate tree: filter pushdown is the
//!   store's job, not the caller's. Pruned splits under a conjunction are
//!   the union of the conjuncts' prunes (measured) — the store may prune
//!   on every leaf independently.
//! - Limit pushdown is `topk(k)`; the exists/no-rows-early-exit terminal
//!   is the upstream gap (vortex-data/vortex#11808).
//! - Keys are names in the prototype; the engine adapter maps interned
//!   key ids at the boundary (same table the compact records use).
//! - `member`/`seek` answer the business key — the table's `key` column,
//!   the sorted `(key,row)` arm — so neither takes a key argument.
//! - Unknown key policy, everywhere: a missing property reads `Val::Null`
//!   from `point`/`batch`, satisfies no comparison in `expand`, and yields
//!   no rows from `seek`/`member`/`distinct`/`topk`.
//! - shortcut: string values are served only by `seek` (the sorted-key
//!   arm); per-value string extraction costs are measured separately
//!   (point.rs O2). Ints cover the value-carrying benches.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::collections::HashMap;

use relate_core::Value;
use vortex::scalar::{PValue, Scalar, ScalarValue};

use crate::store;

#[derive(Clone, Copy, Debug)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A pushed predicate: leaves are column comparisons, and/or compose. The
/// key is any borrow, the same boundary the trait's key arguments take, so a
/// computed column name binds like a literal one.
#[derive(Debug)]
pub enum Pred<'a> {
    Cmp { key: &'a str, op: Cmp, val: i64 },
    And(Box<Pred<'a>>, Box<Pred<'a>>),
    Or(Box<Pred<'a>>, Box<Pred<'a>>),
}

impl<'a> Pred<'a> {
    pub fn and(self, other: Pred<'a>) -> Pred<'a> {
        Pred::And(Box::new(self), Box::new(other))
    }
}

pub type RowIds = Vec<u64>;

/// Interned label id — the engine's `CoreMap<u32, HashSet<NodeId>>` keys.
pub type Label = u32;

/// A promoted column's value shape: the two shapes the operator map needs
/// (ints and strings); other flat types join when a kernel asks for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int,
    Str,
}

/// One property value the store carries.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Val {
    Null,
    Int(i64),
    Str(String),
}

impl Ord for Val {
    fn cmp(&self, other: &Self) -> Ordering {
        // Side-file entries are single-typed per column; the cross-type order
        // only keeps the sort total (Null first, then ints, then strings).
        match (self, other) {
            (Val::Null, Val::Null) => Ordering::Equal,
            (Val::Null, _) => Ordering::Less,
            (_, Val::Null) => Ordering::Greater,
            (Val::Int(a), Val::Int(b)) => a.cmp(b),
            (Val::Int(_), Val::Str(_)) => Ordering::Less,
            (Val::Str(_), Val::Int(_)) => Ordering::Greater,
            (Val::Str(a), Val::Str(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Val {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A property value in the store's promoted shape; `None` for a type the
/// prototype does not carry.
pub fn promoted(value: &Value) -> Option<(Kind, Val)> {
    match value {
        Value::Int(int) => Some((Kind::Int, Val::Int(*int))),
        Value::String(string) => Some((Kind::Str, Val::Str(string.to_string()))),
        _ => None,
    }
}

/// The engine value for a stored one (`Val::Null` = absent).
pub fn to_value(val: Val) -> Option<Value> {
    match val {
        Val::Null => None,
        Val::Int(int) => Some(Value::Int(int)),
        Val::Str(string) => Some(Value::String(std::sync::Arc::from(string))),
    }
}

/// A lookup value in the store's `Val` shape; `None` for a type the store
/// does not promote (the lookup then falls through to the index route).
pub fn promoted_val(value: &Value) -> Option<Val> {
    promoted(value).map(|(_, val)| val)
}

/// A decoded scalar in the store's `Val` shape.
pub fn scalar_to_val(scalar: &Scalar) -> Val {
    match scalar.value() {
        None => Val::Null,
        Some(ScalarValue::Primitive(PValue::I64(v))) => Val::Int(*v),
        Some(ScalarValue::Primitive(PValue::I32(v))) => Val::Int(*v as i64),
        Some(ScalarValue::Utf8(text)) => Val::Str(text.as_str().to_owned()),
        // shortcut: rows carry the prototype's int/string columns; any other
        // canonical shape is outside the seam (see `promoted`).
        Some(_) => Val::Null,
    }
}

pub trait PropStore {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// point read (O1): the value at one row of one key. Rows are the loaded
    /// ids `0..len()`; a row beyond that is absent — the same `Val::Null` a
    /// key the store does not carry answers, one answer for both.
    fn point(&self, row: u64, key: &str) -> Val;
    /// morsel gather (O2/O10): `rows` ascending, `keys` the wanted
    /// columns; out is key-major, len = rows.len() * keys.len(); a key the
    /// store does not carry, and a row beyond `len()`, each contribute
    /// `Val::Null`.
    fn batch(&self, rows: &[u64], keys: &[&str]) -> Vec<Val>;
    /// bulk expansion with filter pushdown (O4/O5/O16): rows of `labels`
    /// (empty = all labels, the engine's unlabeled-scan semantics)
    /// satisfying `pred`, stopping at `cap`+1 survivors — the engine's
    /// `lookup_nodes_upto(labels, props, cap)` shape
    fn expand(&self, labels: &[Label], pred: &Pred<'_>, cap: Option<usize>) -> RowIds;
    /// batched key check / IN-list (O10a/O18, the #9551 shape): rows whose
    /// business key (the table's `key` column) is in `sorted_vals` — one
    /// sorted-merge pass over the (key,row) arm, not one seek per value.
    /// `sorted_vals` must be ascending (the moving cursor relies on it; see
    /// [`member_rows`]).
    fn member(&self, sorted_vals: &[String]) -> RowIds;
    /// equality seek on the sorted key column — the (key,row) arm (O3)
    fn seek(&self, val: &str) -> RowIds;
    /// aggregation pushdown (O6/O13): (value, count) per distinct value
    fn distinct(&self, key: &str) -> Vec<(i64, u64)>;
    /// sort/limit pushdown (O15): the k rows with the smallest values
    fn topk(&self, key: &str, k: usize) -> Vec<(u64, i64)>;
    /// presence (O16): absent rows of a (nullable) column
    fn null_count(&self, key: &str) -> u64;
}

/// The row-space contract, the one place it is decided: `row` addresses a
/// loaded row (`0..rows.len()`); `None` is an absent id — the same answer an
/// unknown key gives — so neither store indexes a row unchecked. The
/// checked `u64 -> usize` conversion rejects a value that cannot name a row
/// on this target instead of truncating it onto a real one.
pub(crate) fn loaded<T>(rows: &[T], row: u64) -> Option<&T> {
    rows.get(usize::try_from(row).ok()?)
}

/// The contiguous `(start, len)` run of entries whose key equals `val`,
/// searched from `from` — the one run resolution every sorted arm reads (the
/// seam's [`SortedArm`] and the engine store's side-file). The whole run
/// answers — a single-entry result would silently drop a repeated key's
/// rows.
pub fn equal_range<K, Q, V>(entries: &[(K, V)], from: usize, val: &Q) -> (usize, usize)
where
    K: Borrow<Q>,
    Q: Ord + ?Sized,
{
    let start = from + entries[from..].partition_point(|(k, _)| k.borrow() < val);
    let len = entries[start..].partition_point(|(k, _)| k.borrow() <= val);
    (start, len)
}

/// Galloping sorted batch against a sorted arm: every row of every key in
/// `sorted_vals`, one pass — O(vals * log(arm/vals)), not O(arm + vals) and
/// not a seek per value.
///
/// Two sortedness preconditions the signature cannot express: `entries` is
/// ascending (every arm sorts at build — [`SortedArm::build`], the store's
/// `side_index`) and `sorted_vals` is ascending (the caller's contract, e.g.
/// the `IN` list sorted before the call). The moving `pos` starts each
/// lookup past the previous run, so an unsorted `sorted_vals` silently
/// misses rows; it is checked here, once, in debug builds only — the
/// release hot loop stays clean.
pub fn member_rows<K, Q, R>(entries: &[(K, R)], sorted_vals: &[Q]) -> Vec<R>
where
    K: Borrow<Q>,
    Q: Ord,
    R: Copy,
{
    debug_assert!(
        sorted_vals.is_sorted(),
        "member_rows: sorted_vals must be ascending"
    );
    let mut out = Vec::new();
    let mut pos = 0usize;
    for val in sorted_vals {
        let (start, len) = equal_range(entries, pos, val);
        out.extend(entries[start..start + len].iter().map(|(_, row)| *row));
        pos = start + len;
    }
    out
}

/// The sorted `(key,row)` arm over the table's `key` column. One home for
/// the seek and the sorted batch: both stores hold one, so the A/B compares
/// store shapes rather than two copies of the index code. The key column
/// carries no uniqueness contract (a loaded CSV need not be unique):
/// [`equal_range`] is the one place a value's rows are resolved, and both
/// readers answer every row under it.
pub struct SortedArm {
    entries: Vec<(String, u64)>,
}

impl SortedArm {
    pub fn build(table: &store::Table) -> Self {
        let mut entries: Vec<(String, u64)> = match table.column("key") {
            Some(store::Col::Str(v)) => v
                .iter()
                .enumerate()
                .map(|(i, s)| (s.clone(), i as u64))
                .collect(),
            _ => Vec::new(),
        };
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Self { entries }
    }

    pub fn seek(&self, val: &str) -> RowIds {
        let (start, len) = equal_range(&self.entries, 0, val);
        self.entries[start..start + len]
            .iter()
            .map(|(_, row)| *row)
            .collect()
    }

    /// Sorted batch against the sorted arm: the shared galloping member.
    pub fn member(&self, sorted_vals: &[String]) -> RowIds {
        member_rows(&self.entries, sorted_vals)
    }
}

/// The row-oriented representation, shaped like the engine's compact live
/// records (ADR-0033): per row one value per column, indexed by the column's
/// key id — the baseline the columnar store must beat.
pub struct RowProps {
    /// Per row, the value of every column, indexed by the column's kid.
    rows: Vec<Vec<Val>>,
    lookup: HashMap<String, u32>,
    /// per-row labels (the engine's `LiveNode.labels`, one per synthetic
    /// node; label 0 = the table carried no label column)
    labels: Vec<u32>,
    /// the (key,row) arm, here too, so `seek` compares index technique on
    /// both stores (the engine's postings serve this today, ADR-0614)
    key_index: SortedArm,
}

fn val_of(col: &store::Col, row: usize) -> Val {
    match col {
        store::Col::Int(v) => Val::Int(v[row]),
        store::Col::Str(v) => Val::Str(v[row].clone()),
        store::Col::Bool(v) => Val::Int(v[row] as i64),
        // shortcut: floats don't carry a seam bench (measured via size kernels)
        _ => Val::Null,
    }
}

impl RowProps {
    pub fn build(table: &store::Table) -> Result<Self, String> {
        let mut lookup = HashMap::new();
        for (name, _) in &table.columns {
            let next = lookup.len() as u32;
            lookup.entry(name.clone()).or_insert(next);
        }
        // The rows' slots and the lookup's ids come from the same map, so a
        // duplicated or skipped column name cannot silently reach another
        // column's value through the kid.
        let kids: Vec<usize> = table
            .columns
            .iter()
            .map(|(n, _)| lookup[n] as usize)
            .collect();
        let rows = (0..table.rows())
            .map(|r| {
                let mut vals = vec![Val::Null; table.columns.len()];
                for ((_, c), &kid) in table.columns.iter().zip(&kids) {
                    vals[kid] = val_of(c, r);
                }
                vals
            })
            .collect();
        let labels: Vec<u32> = match table.column("label") {
            // One default: the table carried no label column, so every row is
            // label 0. A present-but-unusable column is an error, never that
            // same default.
            None => vec![0; table.rows()],
            // A label code is a u32 interned id: checked, never a silent `as`
            // truncation that would fold two labels into one.
            Some(store::Col::Int(v)) => v
                .iter()
                .map(|&x| u32::try_from(x).map_err(|_| format!("label {x} is not a u32 label id")))
                .collect::<Result<Vec<u32>, String>>()?,
            Some(store::Col::Bool(v)) => v.iter().map(|&x| x as u32).collect(),
            Some(other) => {
                return Err(format!(
                    "label column is {:?}, not an integer column",
                    other.ty()
                ))
            }
        };
        Ok(Self {
            rows,
            lookup,
            labels,
            key_index: SortedArm::build(table),
        })
    }

    fn kid(&self, key: &str) -> Option<u32> {
        self.lookup.get(key).copied()
    }

    /// The one row read. `build` sizes every row to `columns.len()` and
    /// derives every kid from that same map, so a kid outside the row is a
    /// broken build — never a null cell to fold into a read or a count.
    fn cell(row: &[Val], kid: usize) -> &Val {
        row.get(kid)
            .expect("row-space invariant: every kid indexes a slot `build` sized")
    }

    fn row_val(&self, row: u64, kid: u32) -> Val {
        match loaded(&self.rows, row) {
            Some(cells) => Self::cell(cells, kid as usize).clone(),
            None => Val::Null,
        }
    }

    fn eval(&self, row: u64, pred: &Pred<'_>) -> bool {
        match pred {
            Pred::Cmp { key, op, val } => {
                // A missing property satisfies no comparison.
                let Some(kid) = self.kid(key) else {
                    return false;
                };
                // The bound is int-only by construction (`Pred::Cmp.val` is
                // an i64), so a string cell compares by `Val`'s typed order
                // (ints before strings) rather than answering "no
                // comparison" — the null cell alone satisfies no comparison.
                let cell = self.row_val(row, kid);
                if cell == Val::Null {
                    return false;
                }
                let ordering = cell.cmp(&Val::Int(*val));
                match op {
                    Cmp::Eq => ordering == Ordering::Equal,
                    Cmp::Ne => ordering != Ordering::Equal,
                    Cmp::Lt => ordering == Ordering::Less,
                    Cmp::Le => ordering != Ordering::Greater,
                    Cmp::Gt => ordering == Ordering::Greater,
                    Cmp::Ge => ordering != Ordering::Less,
                }
            }
            Pred::And(a, b) => self.eval(row, a) && self.eval(row, b),
            Pred::Or(a, b) => self.eval(row, a) || self.eval(row, b),
        }
    }
}

impl PropStore for RowProps {
    fn len(&self) -> usize {
        self.rows.len()
    }

    fn point(&self, row: u64, key: &str) -> Val {
        match self.kid(key) {
            Some(kid) => self.row_val(row, kid),
            None => Val::Null,
        }
    }

    fn batch(&self, rows: &[u64], keys: &[&str]) -> Vec<Val> {
        let kids: Vec<Option<u32>> = keys.iter().map(|k| self.kid(k)).collect();
        let mut out = Vec::with_capacity(rows.len() * keys.len());
        for kid in &kids {
            for row in rows {
                out.push(match kid {
                    Some(k) => self.row_val(*row, *k),
                    None => Val::Null,
                });
            }
        }
        out
    }

    fn expand(&self, labels: &[Label], pred: &Pred<'_>, cap: Option<usize>) -> RowIds {
        let label_ok = |r: usize| labels.is_empty() || labels.contains(&self.labels[r]);
        let take = cap.map_or(usize::MAX, |c| c.saturating_add(1));
        (0..self.rows.len() as u64)
            .filter(|&r| label_ok(r as usize) && self.eval(r, pred))
            .take(take)
            .collect()
    }

    fn member(&self, sorted_vals: &[String]) -> RowIds {
        self.key_index.member(sorted_vals)
    }

    fn seek(&self, val: &str) -> RowIds {
        self.key_index.seek(val)
    }

    fn distinct(&self, key: &str) -> Vec<(i64, u64)> {
        let Some(kid) = self.kid(key) else {
            return Vec::new();
        };
        let kid = kid as usize;
        let mut counts: HashMap<i64, u64> = HashMap::new();
        for row in &self.rows {
            if let Val::Int(v) = Self::cell(row, kid) {
                *counts.entry(*v).or_default() += 1;
            }
        }
        let mut out: Vec<(i64, u64)> = counts.into_iter().collect();
        out.sort_unstable();
        out
    }

    fn topk(&self, key: &str, k: usize) -> Vec<(u64, i64)> {
        let Some(kid) = self.kid(key) else {
            return Vec::new();
        };
        let kid = kid as usize;
        let mut pairs: Vec<(u64, i64)> = self
            .rows
            .iter()
            .enumerate()
            .map(|(r, row)| match Self::cell(row, kid) {
                Val::Int(v) => (r as u64, *v),
                _ => (r as u64, i64::MAX),
            })
            .collect();
        pairs.select_nth_unstable_by_key(k.saturating_sub(1), |p| p.1);
        pairs.truncate(k);
        pairs.sort_unstable_by_key(|p| p.1);
        pairs
    }

    fn null_count(&self, key: &str) -> u64 {
        // Presence answers from the same read `point` answers — its
        // unknown-key case included — so the two cannot disagree: a
        // carried-but-unpromoted column (`val_of` -> `Val::Null`, e.g. a
        // float) reads null on every row, exactly like an uncarried key.
        (0..self.rows.len() as u64)
            .filter(|&row| self.point(row, key) == Val::Null)
            .count() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared sorted arm answers a seek and the sorted batch the same way.
    #[test]
    fn sorted_arm_answers_seek_and_member() {
        let table = store::synthetic("flat", 1000, 1);
        let arm = SortedArm::build(&table);
        assert_eq!(arm.seek("node-00000999"), vec![999]);
        assert!(arm.seek("node-00001000").is_empty());
        let mut vals = vec![
            "node-00000007".to_string(),
            "node-00000100".to_string(),
            "node-00000999".to_string(),
        ];
        vals.sort();
        assert_eq!(arm.member(&vals), vec![7, 100, 999]);
        assert!(arm.member(&["absent".to_string()]).is_empty());
    }

    /// A key column may repeat a value: both readers answer every row under
    /// it, not just the one entry the search lands on first.
    #[test]
    fn sorted_arm_answers_every_row_under_a_repeated_key() {
        let table = store::Table {
            name: "dup".into(),
            columns: vec![(
                "key".into(),
                store::Col::Str(vec!["a".into(), "b".into(), "a".into()]),
            )],
        };
        let arm = SortedArm::build(&table);
        // The sort is unstable, so repeated keys arrive in no fixed order.
        let mut seek = arm.seek("a");
        seek.sort_unstable();
        assert_eq!(seek, vec![0, 2]);
        let mut member = arm.member(&["a".to_string(), "b".to_string()]);
        member.sort_unstable();
        assert_eq!(member, vec![0, 1, 2]);
        assert!(arm.seek("c").is_empty());
    }

    /// An unknown key has one policy across the trait: null reads, no rows,
    /// and a predicate over it satisfies no comparison.
    #[test]
    fn unknown_key_answers_empty_and_null() {
        let table = store::synthetic("flat", 10, 1);
        let row = RowProps::build(&table).unwrap();
        assert_eq!(row.point(0, "absent"), Val::Null);
        assert!(row.distinct("absent").is_empty());
        assert!(row.topk("absent", 3).is_empty());
        assert_eq!(row.null_count("absent"), 10);
        let pred = Pred::Cmp {
            key: "absent",
            op: Cmp::Eq,
            val: 1,
        };
        assert!(row.expand(&[], &pred, None).is_empty());
    }

    /// The two non-int cells the predicate evaluator used to answer alike: a
    /// string compares by the typed order (`Ne` against the int bound holds,
    /// `Eq` does not), while a null cell satisfies no comparison.
    #[test]
    fn a_string_cell_compares_typed_and_a_null_cell_does_not() {
        let mut table = store::synthetic("flat", 10, 1);
        table
            .columns
            .push(("measure".into(), store::Col::Float(vec![0.5; 10])));
        let row = RowProps::build(&table).unwrap();
        let ne_str = Pred::Cmp {
            key: "key",
            op: Cmp::Ne,
            val: 0,
        };
        assert_eq!(
            row.expand(&[], &ne_str, None),
            (0..10u64).collect::<Vec<_>>()
        );
        let eq_str = Pred::Cmp {
            key: "key",
            op: Cmp::Eq,
            val: 0,
        };
        assert!(row.expand(&[], &eq_str, None).is_empty());
        let ne_null = Pred::Cmp {
            key: "measure",
            op: Cmp::Ne,
            val: 0,
        };
        assert!(row.expand(&[], &ne_null, None).is_empty());
    }

    /// A row beyond the loaded count is absent — the same answer an unknown
    /// key gives — never a panic or a wrapped read; `batch` agrees.
    #[test]
    fn an_out_of_range_row_is_absent() {
        let table = store::synthetic("flat", 10, 1);
        let row = RowProps::build(&table).unwrap();
        assert_eq!(row.point(10, "attr0"), Val::Null);
        assert_eq!(row.point(u64::MAX, "attr0"), Val::Null);
        assert_eq!(
            row.batch(&[0, 10], &["attr0"]),
            vec![row.point(0, "attr0"), Val::Null]
        );
    }

    /// Presence answers from the same cells `point` reads: a carried column
    /// the prototype does not promote reads `Val::Null`, so it counts as null
    /// on every row rather than as carried with no nulls.
    #[test]
    fn presence_answers_from_the_stored_cell() {
        let mut table = store::synthetic("flat", 4, 1);
        table
            .columns
            .push(("measure".into(), store::Col::Float(vec![0.5; 4])));
        let row = RowProps::build(&table).unwrap();
        assert_eq!(row.point(0, "measure"), Val::Null);
        assert_eq!(row.null_count("measure"), 4);
    }
}
