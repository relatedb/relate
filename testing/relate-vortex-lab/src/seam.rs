//! The property-store seam: the trait an in-memory property representation
//! implements for the executor. Two implementations: `RowProps` here (the
//! row-oriented baseline, shaped like the engine's compact live records)
//! and `ColProps` in `colstore.rs` (the vortex columnar store). The A/B is
//! `ops::seam_bench`.
//!
//! Design notes (2026-11-23, ADR-0665):
//! - The method set is the measured workload: point (O1), morsel batch
//!   (O2/O10/O11), bulk expansion with a pushed predicate (O4/O5), sorted
//!   key seek (O3, arm A), aggregation (O6/O13), top-k/limit (O15),
//!   presence (O16).
//! - `Pred` carries the whole predicate tree: filter pushdown is the
//!   store's job, not the caller's. Pruned splits under a conjunction are
//!   the union of the conjuncts' prunes (measured) — the store may prune
//!   on every leaf independently.
//! - Limit pushdown is `topk(k)`; the exists/no-rows-early-exit terminal
//!   is the upstream gap (vortex-data/vortex#11808).
//! - Keys are names in the prototype; the engine adapter maps interned
//!   key ids at the boundary (same table the compact records use).
//! - shortcut: string values are served only by `seek` (the sorted-key
//!   arm); per-value string extraction costs are measured separately
//!   (point.rs O2). Ints cover the value-carrying benches.

use std::collections::HashMap;

use crate::store;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Null,
    Int(i64),
    Str(String),
}

#[derive(Clone, Copy, Debug)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A pushed predicate: leaves are column comparisons, and/or compose.
#[derive(Debug)]
pub enum Pred {
    Cmp { key: &'static str, op: Cmp, val: i64 },
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
}

impl Pred {
    pub fn and(self, other: Pred) -> Pred {
        Pred::And(Box::new(self), Box::new(other))
    }
}

pub type RowIds = Vec<u64>;

pub trait PropStore {
    fn len(&self) -> usize;
    /// point read (O1): the value at one row of one key
    fn point(&self, row: u64, key: &str) -> Val;
    /// morsel gather (O2/O10): `rows` ascending, `keys` the wanted
    /// columns; out is key-major, len = rows.len() * keys.len()
    fn batch(&self, rows: &[u64], keys: &[&str]) -> Vec<Val>;
    /// bulk expansion with filter pushdown (O4/O5): rows satisfying `pred`,
    /// stopping at `cap`+1 survivors — the engine's cap chain shape
    /// (`lookup_nodes_upto_at(…, cap)`, whose `RecheckWalk` stops the scan
    /// at the first cap+1 matches)
    fn expand(&self, pred: &Pred, cap: Option<usize>) -> RowIds;
    /// equality seek on the sorted key column — the (key,row) arm (O3)
    fn seek(&self, key: &str, val: &str) -> RowIds;
    /// aggregation pushdown (O6/O13): (value, count) per distinct value
    fn distinct(&self, key: &str) -> Vec<(i64, u64)>;
    /// sort/limit pushdown (O15): the k rows with the smallest values
    fn topk(&self, key: &str, k: usize) -> Vec<(u64, i64)>;
    /// presence (O16): absent rows of a (nullable) column
    fn null_count(&self, key: &str) -> u64;
}

/// The row-oriented representation, shaped like the engine's compact live
/// records (ADR-0033): per row an interned key list + value list, small
/// and linear-scanned — the baseline the columnar store must beat.
pub struct RowProps {
    rows: Vec<(Vec<u32>, Vec<Val>)>,
    names: Vec<String>,
    lookup: HashMap<String, u32>,
    /// the (key,row) arm, here too, so `seek` compares index technique on
    /// both stores (the engine's postings serve this today, ADR-0614)
    key_index: Vec<(String, u64)>,
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
    pub fn build(table: &store::Table) -> Self {
        let mut names = Vec::new();
        let mut lookup = HashMap::new();
        for (name, _, _) in &table.columns {
            lookup.insert(name.clone(), names.len() as u32);
            names.push(name.clone());
        }
        let mut rows = Vec::with_capacity(table.rows());
        for r in 0..table.rows() {
            let kids: Vec<u32> = (0..table.columns.len() as u32).collect();
            let vals: Vec<Val> = table.columns.iter().map(|(_, _, c)| val_of(c, r)).collect();
            rows.push((kids, vals));
        }
        // the sorted (key,row) arm over the `key` column
        let mut key_index: Vec<(String, u64)> = table
            .column("key")
            .map(|c| match c {
                store::Col::Str(v) => v.iter().enumerate().map(|(i, s)| (s.clone(), i as u64)).collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        key_index.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Self { rows, names, lookup, key_index }
    }

    fn kid(&self, key: &str) -> Option<u32> {
        self.lookup.get(key).copied()
    }

    fn row_val(&self, row: u64, kid: u32) -> Val {
        let (keys, vals) = &self.rows[row as usize];
        for (i, k) in keys.iter().enumerate() {
            if *k == kid {
                return vals[i].clone();
            }
        }
        Val::Null
    }

    fn eval(&self, row: u64, pred: &Pred) -> bool {
        match pred {
            Pred::Cmp { key, op, val } => {
                let kid = self.kid(key).expect("unknown key");
                match self.row_val(row, kid) {
                    Val::Int(v) => match op {
                        Cmp::Eq => v == *val,
                        Cmp::Ne => v != *val,
                        Cmp::Lt => v < *val,
                        Cmp::Le => v <= *val,
                        Cmp::Gt => v > *val,
                        Cmp::Ge => v >= *val,
                    },
                    _ => false,
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

    fn expand(&self, pred: &Pred, cap: Option<usize>) -> RowIds {
        match cap {
            None => (0..self.rows.len() as u64).filter(|&r| self.eval(r, pred)).collect(),
            Some(cap) => {
                let mut out = Vec::with_capacity(cap + 1);
                for r in 0..self.rows.len() as u64 {
                    if self.eval(r, pred) {
                        out.push(r);
                        if out.len() > cap {
                            break;
                        }
                    }
                }
                out
            }
        }
    }

    fn seek(&self, _key: &str, val: &str) -> RowIds {
        // unique keys in the synthetic shapes: 0 or 1 hit
        match self.key_index.binary_search_by(|(k, _)| k.as_str().cmp(val)) {
            Ok(i) => vec![self.key_index[i].1],
            Err(_) => Vec::new(),
        }
    }

    fn distinct(&self, key: &str) -> Vec<(i64, u64)> {
        let kid = self.kid(key).expect("unknown key");
        let mut counts: HashMap<i64, u64> = HashMap::new();
        for (keys, vals) in &self.rows {
            for (i, k) in keys.iter().enumerate() {
                if *k == kid {
                    if let Val::Int(v) = vals[i] {
                        *counts.entry(v).or_default() += 1;
                    }
                }
            }
        }
        let mut out: Vec<(i64, u64)> = counts.into_iter().collect();
        out.sort_unstable();
        out
    }

    fn topk(&self, key: &str, k: usize) -> Vec<(u64, i64)> {
        let kid = self.kid(key).expect("unknown key");
        let mut pairs: Vec<(u64, i64)> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(r, (keys, vals))| {
                keys.iter().position(|&x| x == kid).map(|i| match vals[i] {
                    Val::Int(v) => (r as u64, v),
                    _ => (r as u64, i64::MAX),
                })
            })
            .collect();
        pairs.select_nth_unstable_by_key(k.saturating_sub(1), |p| p.1);
        pairs.truncate(k);
        pairs.sort_unstable_by_key(|p| p.1);
        pairs
    }

    fn null_count(&self, key: &str) -> u64 {
        match self.kid(key) {
            Some(kid) => self
                .rows
                .iter()
                .filter(|(keys, _)| !keys.contains(&kid))
                .count() as u64,
            None => self.rows.len() as u64,
        }
    }
}
