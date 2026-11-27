//! The columnar property store behind the seam: the vortex file written
//! node-major, opened mmap, with a decoded-column cache. Each `PropStore`
//! method uses the technique the corresponding kernel measured:
//! - `expand` pushes the whole predicate into the layout reader (zone-map
//!   pruning; conjunction prunes are the union of the conjuncts')
//! - `point`/`batch`/`distinct`/`topk` read through the decoded-column
//!   cache — the real design is per-chunk with a density hint (the
//!   upstream Probe API, vortex-data/vortex#8694); the prototype caches
//!   whole columns, which upper-bounds the per-chunk cost
//! - `seek` binary-searches the sorted (key,row) arm (in memory at seal)
//! - shortcut: int columns carry the value benches; strings are served by
//!   `seek` only; `null_count` is 0 on the non-nullable synthetic shape
//!   (the NullCount-stat path is measured in filter.rs O16)
//!
//! Interior mutability is one `RefCell` for the cache — the engine-side
//! store would be the same shape behind the seam.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::rc::Rc;

use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::VortexSessionExecute;
use vortex::expr::{
    and, col, eq, gt, gt_eq, lit, lt, lt_eq, not_eq, or, BoundExpression, Expression,
};
use vortex::file::VortexFile;
use vortex::io::runtime::single::SingleThreadRuntime;
use vortex::session::VortexSession;

use crate::seam::{loaded, Cmp, Pred, PropStore, RowIds, SortedArm, Val};
use crate::store;

pub struct ColProps {
    n: usize,
    #[allow(dead_code)] // owns the mmap the vfile reads through
    session: VortexSession,
    rt: SingleThreadRuntime,
    vfile: VortexFile,
    /// The table's column names — the absent-key test `point`/`batch` need
    /// without decoding a projection that cannot bind.
    columns: Vec<String>,
    /// Whether the table carried a `label` column. When it did not, every row
    /// is label 0 (the row store's one default), so the label half of a scan
    /// is answered without binding a column that does not exist.
    has_label: bool,
    ints: RefCell<HashMap<String, Rc<Vec<i64>>>>,
    key_index: SortedArm,
}

fn to_expr(pred: &Pred<'_>) -> Expression {
    match pred {
        Pred::Cmp { key, op, val } => {
            let l = col(*key);
            let r = lit(*val);
            match op {
                Cmp::Eq => eq(l, r),
                Cmp::Ne => not_eq(l, r),
                Cmp::Lt => lt(l, r),
                Cmp::Le => lt_eq(l, r),
                Cmp::Gt => gt(l, r),
                Cmp::Ge => gt_eq(l, r),
            }
        }
        Pred::And(a, b) => and(to_expr(a), to_expr(b)),
        Pred::Or(a, b) => or(to_expr(a), to_expr(b)),
    }
}

impl ColProps {
    pub fn build(table: &store::Table, path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        store::write(path, table)?;
        let (session, rt, vfile) = store::open_mmap(path)?;
        Ok(Self {
            n: table.rows(),
            session,
            rt,
            vfile,
            columns: table.columns.iter().map(|(name, _)| name.clone()).collect(),
            has_label: table.column("label").is_some(),
            ints: RefCell::new(HashMap::new()),
            key_index: SortedArm::build(table),
        })
    }

    /// The decoded-column cache entry; one decode per column per process.
    /// `None` for a key the store does not carry.
    fn ints(&self, key: &str) -> Option<Rc<Vec<i64>>> {
        if let Some(v) = self.ints.borrow().get(key) {
            return Some(v.clone());
        }
        if !self.columns.iter().any(|name| name == key) {
            return None;
        }
        let v = self.decode_ints(key).expect("column decode failed");
        self.ints.borrow_mut().insert(key.to_string(), v.clone());
        Some(v)
    }

    fn decode_ints(&self, key: &str) -> Result<Rc<Vec<i64>>, Box<dyn std::error::Error>> {
        // the shared projected read: the multi-chunk file yields the column
        let field = store::projected(&self.rt, &self.vfile, key)?;
        let mut ctx = self.session.create_execution_ctx();
        let canonical = field.execute::<PrimitiveArray>(&mut ctx)?;
        Ok(Rc::new(canonical.as_slice::<i64>().to_vec()))
    }

    /// scan with the predicate pushed into the layout reader, projecting
    /// `id` (row == node id) to recover the surviving rows; labels AND in
    /// as a label-code predicate so node+property filtering is ONE scan;
    /// `cap` mirrors the engine's `lookup_nodes_upto_at(…, cap)` —
    /// with_limit truncates but does not stop decoding (#11808)
    fn scan_ids(
        &self,
        labels: &[crate::seam::Label],
        pred: &Pred<'_>,
        cap: Option<usize>,
    ) -> Result<RowIds, Box<dyn std::error::Error>> {
        let mut expr = to_expr(pred);
        if !labels.is_empty() && !self.has_label {
            // A label-less table is the row store's label-0 default: label 0
            // selects every row, any other label selects none — never a bind
            // of a column the table does not carry.
            if !labels.contains(&0) {
                return Ok(RowIds::new());
            }
        } else if let Some(label_expr) = labels
            .iter()
            .map(|&l| eq(col("label"), lit(l as i64)))
            .reduce(or)
        {
            // label filter as or-of-eq on the label-code column, ANDed in
            expr = and(expr, label_expr);
        }
        let filter: BoundExpression = expr.bind(self.vfile.dtype())?.optimize_recursive()?;
        let projection: BoundExpression =
            col("id").bind(self.vfile.dtype())?.optimize_recursive()?;
        let mut opts = store::ScanOpts {
            filter: Some(&filter),
            projection: Some(&projection),
            ..store::ScanOpts::default()
        };
        if let Some(c) = cap {
            // the fork's bound: sequential split issue under a limit, so the
            // budget check (is_exhausted) stops remaining splits instead of
            // paying spawned-ahead IO for all of them (upstream #11808)
            opts.limit = Some(c as u64 + 1);
            opts.max_splits_in_flight = NonZeroUsize::new(1);
        }
        let arr = store::scan(&self.vfile, &self.rt, opts)?;
        let field = store::field_of(arr, "id")?;
        let mut ctx = self.session.create_execution_ctx();
        let canonical = field.execute::<PrimitiveArray>(&mut ctx)?;
        Ok(canonical
            .as_slice::<i64>()
            .iter()
            .map(|&v| v as u64)
            .collect())
    }
}

impl PropStore for ColProps {
    fn len(&self) -> usize {
        self.n
    }

    fn point(&self, row: u64, key: &str) -> Val {
        match self.ints(key) {
            // An out-of-range row is absent (`loaded` finds no slot), the
            // same answer an unknown key gives — never a raw index.
            Some(col) => loaded(&col, row).map_or(Val::Null, |v| Val::Int(*v)),
            None => Val::Null,
        }
    }

    fn batch(&self, rows: &[u64], keys: &[&str]) -> Vec<Val> {
        let cols: Vec<Option<Rc<Vec<i64>>>> = keys.iter().map(|k| self.ints(k)).collect();
        let mut out = Vec::with_capacity(rows.len() * keys.len());
        for col in &cols {
            for row in rows {
                out.push(match col {
                    Some(col) => loaded(col, *row).map_or(Val::Null, |v| Val::Int(*v)),
                    None => Val::Null,
                });
            }
        }
        out
    }

    fn expand(&self, labels: &[crate::seam::Label], pred: &Pred<'_>, cap: Option<usize>) -> RowIds {
        self.scan_ids(labels, pred, cap).expect("scan failed")
    }

    fn member(&self, sorted_vals: &[String]) -> RowIds {
        self.key_index.member(sorted_vals)
    }

    fn seek(&self, val: &str) -> RowIds {
        self.key_index.seek(val)
    }

    fn distinct(&self, key: &str) -> Vec<(i64, u64)> {
        let Some(col) = self.ints(key) else {
            return Vec::new();
        };
        let mut counts: HashMap<i64, u64> = HashMap::new();
        for &v in col.iter() {
            *counts.entry(v).or_default() += 1;
        }
        let mut out: Vec<(i64, u64)> = counts.into_iter().collect();
        out.sort_unstable();
        out
    }

    fn topk(&self, key: &str, k: usize) -> Vec<(u64, i64)> {
        let Some(col) = self.ints(key) else {
            return Vec::new();
        };
        let mut pairs: Vec<(u64, i64)> = col
            .iter()
            .enumerate()
            .map(|(r, &v)| (r as u64, v))
            .collect();
        pairs.select_nth_unstable_by_key(k.saturating_sub(1), |p| p.1);
        pairs.truncate(k);
        pairs.sort_unstable_by_key(|p| p.1);
        pairs
    }

    fn null_count(&self, _key: &str) -> u64 {
        0 // shortcut: non-nullable synthetic columns; NullCount path measured in O16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seam::{Cmp, Pred, PropStore, RowProps};

    /// A label-less table answers the label half the row store's way: label 0
    /// selects every row, any other label selects none — never a bind of a
    /// column the table does not carry.
    #[test]
    fn a_label_less_table_scans_as_label_zero() {
        let table = store::synthetic("mono", 16, 1);
        let dir = tempfile::tempdir().unwrap();
        let col = ColProps::build(&table, &dir.path().join("label-less.vortex")).unwrap();
        let row = RowProps::build(&table).unwrap();
        let pred = Pred::Cmp {
            key: "attr0",
            op: Cmp::Ge,
            val: i64::MIN,
        };
        let all: RowIds = (0..16).collect();
        assert_eq!(col.expand(&[0], &pred, None), all);
        assert_eq!(col.expand(&[0], &pred, None), row.expand(&[0], &pred, None));
        assert!(col.expand(&[7], &pred, None).is_empty());
    }
}
