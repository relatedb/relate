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
use std::path::Path;
use std::rc::Rc;

use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::arrays::struct_::{Struct, StructArrayExt};
use vortex::array::expr::{
    and, col, eq, gt, gt_eq, lit, lt, lt_eq, not_eq, or, BoundExpression, Expression,
};
use vortex::array::stream::ArrayStreamExt;
use vortex::array::{ArrayRef, VortexSessionExecute};
use vortex::file::VortexFile;
use vortex::io::runtime::single::SingleThreadRuntime;
use vortex::io::runtime::BlockingRuntime;
use vortex::session::VortexSession;

use crate::seam::{Cmp, Pred, PropStore, RowIds, Val};
use crate::store;

pub struct ColProps {
    n: usize,
    #[allow(dead_code)] // owns the mmap the vfile reads through
    session: VortexSession,
    rt: SingleThreadRuntime,
    vfile: VortexFile,
    ints: RefCell<HashMap<String, Rc<Vec<i64>>>>,
    key_index: Vec<(String, u64)>,
}

fn to_expr(pred: &Pred) -> Expression {
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
        let mut key_index: Vec<(String, u64)> = table
            .column("key")
            .map(|c| match c {
                store::Col::Str(v) => v
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (s.clone(), i as u64))
                    .collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        key_index.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Ok(Self {
            n: table.rows(),
            session,
            rt,
            vfile,
            ints: RefCell::new(HashMap::new()),
            key_index,
        })
    }

    /// The decoded-column cache entry; one decode per column per process.
    fn ints(&self, key: &str) -> Rc<Vec<i64>> {
        if let Some(v) = self.ints.borrow().get(key) {
            return v.clone();
        }
        let v = self.decode_ints(key).expect("column decode failed");
        self.ints.borrow_mut().insert(key.to_string(), v.clone());
        v
    }

    fn decode_ints(&self, key: &str) -> Result<Rc<Vec<i64>>, Box<dyn std::error::Error>> {
        // projected scan: the multi-chunk file yields the column directly
        // (the unprojected root is a chunked struct — same as agg.rs's scan)
        let proj: BoundExpression = col(key).bind(self.vfile.dtype())?.optimize_recursive()?;
        let arr = self.rt.block_on(async {
            self.vfile
                .scan()?
                .with_projection(proj)
                .into_array_stream()?
                .read_all()
                .await
        })?;
        let field = self.unwrap_field(arr, key)?;
        let mut ctx = self.session.create_execution_ctx();
        let canonical = field.execute::<PrimitiveArray>(&mut ctx)?;
        Ok(Rc::new(canonical.as_slice::<i64>().to_vec()))
    }

    /// a projected read yields the field, or a struct holding it, possibly
    /// chunked — normalize to the one field array
    fn unwrap_field(
        &self,
        arr: vortex::array::ArrayRef,
        name: &str,
    ) -> Result<vortex::array::ArrayRef, Box<dyn std::error::Error>> {
        match arr.as_opt::<Struct>() {
            Some(view) => Ok(view.unmasked_field_by_name(name)?.clone()),
            None => Ok(arr),
        }
    }

    /// scan with the predicate pushed into the layout reader, projecting
    /// `id` (row == node id) to recover the surviving rows; labels AND in
    /// as a label-code predicate so node+property filtering is ONE scan;
    /// `cap` mirrors the engine's `lookup_nodes_upto_at(…, cap)` —
    /// with_limit truncates but does not stop decoding (#11808)
    fn scan_ids(
        &self,
        labels: &[crate::seam::Label],
        pred: &Pred,
        cap: Option<usize>,
    ) -> Result<RowIds, Box<dyn std::error::Error>> {
        let mut expr = to_expr(pred);
        if !labels.is_empty() {
            // label filter as or-of-eq on the label-code column, ANDed in
            let label_expr = labels
                .iter()
                .map(|&l| eq(col("label"), lit(l as i64)))
                .reduce(or)
                .expect("non-empty labels reduce");
            expr = and(expr, label_expr);
        }
        let bound: BoundExpression = expr.bind(self.vfile.dtype())?.optimize_recursive()?;
        let proj: BoundExpression = col("id").bind(self.vfile.dtype())?.optimize_recursive()?;
        let mut b = self.vfile.scan()?;
        b = b.with_filter(bound).with_projection(proj);
        if let Some(c) = cap {
            // the fork's bound: sequential split issue under a limit, so the
            // budget check (is_exhausted) stops remaining splits instead of
            // paying spawned-ahead IO for all of them (upstream #11808)
            b = b
                .with_limit(c as u64 + 1)
                .with_some_max_splits_in_flight(std::num::NonZeroUsize::new(1));
        }
        let arr = self
            .rt
            .block_on(async { b.into_array_stream()?.read_all().await })?;
        // projection yields the field (or a struct holding it)
        let field = self.unwrap_field(arr, "id")?;
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
        Val::Int(self.ints(key)[row as usize])
    }

    fn batch(&self, rows: &[u64], keys: &[&str]) -> Vec<Val> {
        let cols: Vec<Rc<Vec<i64>>> = keys.iter().map(|k| self.ints(k)).collect();
        let mut out = Vec::with_capacity(rows.len() * keys.len());
        for col in &cols {
            for row in rows {
                out.push(Val::Int(col[*row as usize]));
            }
        }
        out
    }

    fn expand(&self, labels: &[crate::seam::Label], pred: &Pred, cap: Option<usize>) -> RowIds {
        self.scan_ids(labels, pred, cap).expect("scan failed")
    }

    fn member(&self, _key: &str, sorted_vals: &[String]) -> RowIds {
        // sorted batch against the sorted arm: galloping suffix search —
        // O(vals * log(arm/vals)), the #9551 shape
        let arm = &self.key_index;
        let mut out = Vec::new();
        let mut pos = 0usize;
        for val in sorted_vals {
            let idx = pos + arm[pos..].partition_point(|(k, _)| k.as_str() < val.as_str());
            if idx < arm.len() && arm[idx].0 == *val {
                out.push(arm[idx].1);
                pos = idx + 1;
            } else {
                pos = idx;
            }
        }
        out
    }

    fn seek(&self, _key: &str, val: &str) -> RowIds {
        match self
            .key_index
            .binary_search_by(|(k, _)| k.as_str().cmp(val))
        {
            Ok(i) => vec![self.key_index[i].1],
            Err(_) => Vec::new(),
        }
    }

    fn distinct(&self, key: &str) -> Vec<(i64, u64)> {
        let col = self.ints(key);
        let mut counts: HashMap<i64, u64> = HashMap::new();
        for &v in col.iter() {
            *counts.entry(v).or_default() += 1;
        }
        let mut out: Vec<(i64, u64)> = counts.into_iter().collect();
        out.sort_unstable();
        out
    }

    fn topk(&self, key: &str, k: usize) -> Vec<(u64, i64)> {
        let col = self.ints(key);
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
