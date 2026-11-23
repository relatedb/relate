//! The node-major property store: rows are nodes, row index == node id,
//! columns are properties — the shape the seek and filter kernels assume.
//!
//! The schema derivation mirrors the wide-column Parquet codec
//! (`relate/relate-storage/src/codec/parquet/wide.rs`, ADR-0534): a property
//! promoted to a column when its type is flat (bool/int/float/string); every
//! other value stays in the residual column. The lab re-derives it rather
//! than importing it (`pub(crate)` there) because the point of the lab is to
//! measure the same layout, not to share the engine's code path.

use std::error::Error;
use std::fs;
use std::path::Path;

use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::arrays::struct_::StructArray;
use vortex::array::arrays::varbin::VarBinArray;
use vortex::array::arrays::BoolArray;
use vortex::array::validity::Validity;
use vortex::array::{ArrayRef, IntoArray};
use vortex::buffer::{Buffer, ByteBuffer};
use vortex::error::VortexResult;
use vortex::file::{OpenOptionsSessionExt, WriteOptionsSessionExt};
use vortex::io::runtime::BlockingRuntime;
use vortex::io::runtime::single::SingleThreadRuntime;
use vortex::io::session::RuntimeSessionExt;
use vortex::session::VortexSession;
use vortex::VortexSessionDefault;

/// The flat value shapes a promoted column can carry (ADR-0534).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Bool,
    Int,
    Float,
    Str,
}

/// A column's values, in row (node-id) order.
#[derive(Debug, Clone)]
pub enum Col {
    Bool(Vec<bool>),
    Int(Vec<i64>),
    Float(Vec<f64>),
    Str(Vec<String>),
}

impl Col {
    pub fn len(&self) -> usize {
        match self {
            Col::Bool(v) => v.len(),
            Col::Int(v) => v.len(),
            Col::Float(v) => v.len(),
            Col::Str(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Uncompressed value bytes — the size table's denominator.
    pub fn raw_bytes(&self) -> u64 {
        match self {
            Col::Bool(v) => v.len() as u64,
            Col::Int(v) => 8 * v.len() as u64,
            Col::Float(v) => 8 * v.len() as u64,
            Col::Str(v) => v.iter().map(|s| s.len() as u64).sum(),
        }
    }

    fn to_vortex(&self) -> ArrayRef {
        match self {
            Col::Bool(v) => BoolArray::from_iter(v.iter().copied()).into_array(),
            Col::Int(v) => {
                PrimitiveArray::new(Buffer::from(v.clone()), Validity::NonNullable).into_array()
            }
            Col::Float(v) => {
                PrimitiveArray::new(Buffer::from(v.clone()), Validity::NonNullable).into_array()
            }
            Col::Str(v) => {
                let strs: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
                VarBinArray::from(strs).into_array()
            }
        }
    }
}

/// A table of node properties, one row per node.
#[derive(Debug, Clone)]
pub struct Table {
    pub name: String,
    pub columns: Vec<(String, Ty, Col)>,
}

impl Table {
    pub fn rows(&self) -> usize {
        self.columns.first().map(|(_, _, c)| c.len()).unwrap_or(0)
    }

    pub fn raw_bytes(&self) -> u64 {
        self.columns.iter().map(|(_, _, c)| c.raw_bytes()).sum()
    }

    pub fn column(&self, name: &str) -> Option<&Col> {
        self.columns
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, _, c)| c)
    }

    /// The table as one vortex struct array (row index == node id).
    pub fn to_vortex(&self) -> VortexResult<ArrayRef> {
        let names: Vec<&str> = self.columns.iter().map(|(n, _, _)| n.as_str()).collect();
        let children: Vec<ArrayRef> = self.columns.iter().map(|(_, _, c)| c.to_vortex()).collect();
        Ok(StructArray::try_new(
            names.into(),
            children,
            self.rows(),
            Validity::NonNullable,
        )?
        .into_array())
    }
}

/// Read selected columns of a pipe-delimited LDBC CSV (escaped quoting is
/// real, so the `csv` crate does the parsing).
pub fn load_csv(
    path: &Path,
    cols: &[(&str, Ty)],
) -> Result<Table, Box<dyn Error>> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(b'|')
        .has_headers(true)
        .from_path(path)?;
    let headers = rdr.headers()?.clone();
    let mut idx = Vec::new();
    for (name, ty) in cols {
        let i = headers
            .iter()
            .position(|h| h == *name)
            .ok_or_else(|| format!("{}: no column {name}", path.display()))?;
        idx.push((i, *name, *ty));
    }
    let mut out: Vec<(String, Ty, Col)> = cols
        .iter()
        .map(|(n, t)| {
            (
                n.to_string(),
                *t,
                match t {
                    Ty::Bool => Col::Bool(Vec::new()),
                    Ty::Int => Col::Int(Vec::new()),
                    Ty::Float => Col::Float(Vec::new()),
                    Ty::Str => Col::Str(Vec::new()),
                },
            )
        })
        .collect();
    for rec in rdr.records() {
        let rec = rec?;
        for (i, _, ty) in &idx {
            let field = rec.get(*i).unwrap_or("");
            match (&mut out[idx.iter().position(|(j, _, _)| j == i).unwrap()].2, ty) {
                (Col::Bool(v), Ty::Bool) => v.push(field == "true" || field == "1"),
                (Col::Int(v), Ty::Int) => v.push(field.parse().unwrap_or(0)),
                (Col::Float(v), Ty::Float) => v.push(field.parse().unwrap_or(0.0)),
                (Col::Str(v), Ty::Str) => v.push(field.to_string()),
                _ => unreachable!("column type and slot agree by construction"),
            }
        }
    }
    Ok(Table {
        name: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("table")
            .to_string(),
        columns: out,
    })
}

/// Synthetic shapes for the size and seek benches: a unique `id` column
/// (dense node id), a unique string `key` (the business key a seek targets),
/// and `attrs` integer property columns with a repeating distribution.
pub fn synthetic(kind: &str, n: usize, attrs: usize) -> Table {
    let ids: Vec<i64> = (0..n as i64).collect();
    let keys: Vec<String> = match kind {
        // a key that sorts in the same order as the node id (cheapest case)
        "flat" => (0..n).map(|i| format!("node-{i:08}")).collect(),
        // a key whose sort order is a permutation of the node order
        "ring" => (0..n).map(|i| format!("node-{:08}", (i * 7919) % n)).collect(),
        other => panic!("unknown synthetic shape {other}"),
    };
    let mut columns: Vec<(String, Ty, Col)> = vec![
        ("id".into(), Ty::Int, Col::Int(ids)),
        ("key".into(), Ty::Str, Col::Str(keys)),
    ];
    for a in 0..attrs {
        let v: Vec<i64> = (0..n).map(|i| ((i * (a + 3)) % 1000) as i64).collect();
        columns.push((format!("attr{a}"), Ty::Int, Col::Int(v)));
    }
    Table {
        name: format!("synthetic-{kind}-{n}-{attrs}"),
        columns,
    }
}

/// Write a table as a vortex file; returns (bytes written, per-column
/// compressed sizes when the writer reports them).
pub fn write(path: &Path, table: &Table) -> Result<(u64, Vec<u64>), Box<dyn Error>> {
    let rt = SingleThreadRuntime::default();
    let session = VortexSession::default().with_handle(rt.handle());
    let array = table.to_vortex()?;
    let out = fs::File::create(path)?;
    let summary = session
        .write_options()
        .blocking(&rt)
        .write(out, array.to_array_iterator())?;
    let per_column = summary.compressed_column_sizes().unwrap_or_default();
    Ok((summary.size(), per_column))
}

/// Open a vortex file through an mmap: zero decode, pages fault in on use.
pub fn open_mmap(path: &Path) -> Result<(VortexSession, SingleThreadRuntime, vortex::file::VortexFile), Box<dyn Error>> {
    let rt = SingleThreadRuntime::default();
    let session = VortexSession::default().with_handle(rt.handle());
    let f = fs::File::open(path)?;
    let mmap = unsafe { memmap2::Mmap::map(&f)? };
    let vfile = session.open_options().open_buffer(ByteBuffer::from(mmap))?;
    Ok((session, rt, vfile))
}
