//! The node-major property store: rows are nodes, row index == node id,
//! columns are properties — the shape the seek and filter kernels assume.
//!
//! The schema derivation mirrors the wide-column Parquet codec
//! (`relate/relate-storage/src/codec/parquet/wide.rs`, ADR-0534): a property
//! promoted to a column when its type is flat (bool/int/float/string); every
//! other value stays in the residual column. The lab re-derives it rather
//! than importing it (`pub(crate)` there) because the point of the lab is to
//! measure the same layout, not to share the engine's code path.
//!
//! This module is also the shared vortex read layer: [`scan`] is the one
//! scan-builder shape, [`projected`] the one projected single-column read,
//! [`field_of`] the one struct-or-field normalization, and [`open_mmap`] the
//! one mmap open — every kernel calls these instead of re-spelling them.

use std::error::Error;
use std::fs;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::{Path, PathBuf};

use vortex::array::arrays::chunked::{Chunked, ChunkedArrayExt};
use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::arrays::struct_::{Struct, StructArray, StructArrayExt};
use vortex::array::arrays::varbin::VarBinArray;
use vortex::array::arrays::BoolArray;
use vortex::array::stream::ArrayStreamExt;
use vortex::array::validity::Validity;
use vortex::array::{ArrayRef, IntoArray};
use vortex::buffer::{Buffer, ByteBuffer};
use vortex::error::VortexResult;
use vortex::expr::{col, BoundExpression};
use vortex::file::{OpenOptionsSessionExt, VortexFile, WriteOptionsSessionExt};
use vortex::io::runtime::single::SingleThreadRuntime;
use vortex::io::runtime::BlockingRuntime;
use vortex::io::session::RuntimeSessionExt;
use vortex::scan::strict_sorted_buffer::StrictSortedBuffer;
use vortex::session::VortexSession;
use vortex::VortexSessionDefault;

use relate_test_support::rng::Rng;

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
    fn empty(ty: Ty) -> Self {
        match ty {
            Ty::Bool => Col::Bool(Vec::new()),
            Ty::Int => Col::Int(Vec::new()),
            Ty::Float => Col::Float(Vec::new()),
            Ty::Str => Col::Str(Vec::new()),
        }
    }

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

    /// The column's type — derived from the values, never carried beside them.
    pub fn ty(&self) -> Ty {
        match self {
            Col::Bool(_) => Ty::Bool,
            Col::Int(_) => Ty::Int,
            Col::Float(_) => Ty::Float,
            Col::Str(_) => Ty::Str,
        }
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

    /// Append one CSV field. The `Col` variant is the type, so there is no
    /// second type match to disagree with it, and an unparseable field is a
    /// loader error, never a silent zero.
    fn push_field(&mut self, field: &str, row: usize, name: &str) -> Result<(), String> {
        match self {
            Col::Bool(v) => v.push(field == "true" || field == "1"),
            Col::Int(v) => v.push(field.parse().map_err(|e: std::num::ParseIntError| {
                format!("row {row} column {name}: {e} on {field:?}")
            })?),
            Col::Float(v) => v.push(field.parse().map_err(|e: std::num::ParseFloatError| {
                format!("row {row} column {name}: {e} on {field:?}")
            })?),
            Col::Str(v) => v.push(field.to_string()),
        }
        Ok(())
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
    pub columns: Vec<(String, Col)>,
}

impl Table {
    pub fn rows(&self) -> usize {
        self.columns.first().map(|(_, c)| c.len()).unwrap_or(0)
    }

    pub fn raw_bytes(&self) -> u64 {
        self.columns.iter().map(|(_, c)| c.raw_bytes()).sum()
    }

    pub fn column(&self, name: &str) -> Option<&Col> {
        self.columns.iter().find(|(n, _)| n == name).map(|(_, c)| c)
    }

    /// An i64 column's values — the kernels' one in-memory-truth accessor.
    pub fn ints(&self, name: &str) -> Result<&[i64], String> {
        match self.column(name) {
            Some(Col::Int(v)) => Ok(v),
            Some(other) => Err(format!(
                "column {name} is {:?}, not an i64 column",
                other.ty()
            )),
            None => Err(format!("no column {name}")),
        }
    }

    /// The table as one vortex struct array (row index == node id).
    pub fn to_vortex(&self) -> VortexResult<ArrayRef> {
        let names: Vec<&str> = self.columns.iter().map(|(n, _)| n.as_str()).collect();
        let children: Vec<ArrayRef> = self.columns.iter().map(|(_, c)| c.to_vortex()).collect();
        Ok(
            StructArray::try_new(names.into(), children, self.rows(), Validity::NonNullable)?
                .into_array(),
        )
    }
}

/// The run's output directory (`VORTEX_LAB_OUT`, default `/tmp/relate-vortex-lab`).
pub fn lab_out() -> PathBuf {
    PathBuf::from(
        std::env::var("VORTEX_LAB_OUT").unwrap_or_else(|_| "/tmp/relate-vortex-lab".to_string()),
    )
}

/// The LDBC SF0.1 Person CSV the query-latency suite already has on disk
/// (`VORTEX_LAB_CSV` overrides) — the one home of that default path.
pub fn lab_csv() -> PathBuf {
    PathBuf::from(
        std::env::var("VORTEX_LAB_CSV").unwrap_or_else(|_| {
            "target/query-latency/data/social_network-sf0.1-CsvBasic-StringDateFormatter/dynamic/person_0_0.csv"
                .to_string()
        }),
    )
}

/// Read selected columns of a pipe-delimited LDBC CSV (escaped quoting is
/// real, so the `csv` crate does the parsing).
pub fn load_csv(path: &Path, cols: &[(&str, Ty)]) -> Result<Table, Box<dyn Error>> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(b'|')
        .has_headers(true)
        .from_path(path)?;
    let headers = rdr.headers()?.clone();
    // Resolve each destination column's CSV index once, at header time; the
    // record loop below walks this list and the parallel output columns by
    // construction, so no field ever re-searches its slot.
    let mut slots: Vec<(usize, &str)> = Vec::with_capacity(cols.len());
    let mut out: Vec<(String, Col)> = Vec::with_capacity(cols.len());
    for (name, ty) in cols {
        let csv_index = headers
            .iter()
            .position(|h| h == *name)
            .ok_or_else(|| format!("{}: no column {name}", path.display()))?;
        slots.push((csv_index, name));
        out.push((name.to_string(), Col::empty(*ty)));
    }
    for (row, rec) in rdr.records().enumerate() {
        let rec = rec?;
        for ((csv_index, name), (_, column)) in slots.iter().zip(out.iter_mut()) {
            let field = rec
                .get(*csv_index)
                .ok_or_else(|| format!("{}: row {row} has no column {name}", path.display()))?;
            column
                .push_field(field, row, name)
                .map_err(|e| format!("{}: {e}", path.display()))?;
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

/// One synthetic shape's value policies: the business key of row `i` in an
/// `n`-row table, and attr `a`'s value at row `i`. One `match` decides both,
/// so an unknown shape fails once instead of falling into a silent default.
struct Shape {
    key: fn(usize, usize) -> String,
    attr: fn(usize, usize) -> i64,
}

fn shape(kind: &str) -> Shape {
    match kind {
        // a key that sorts in the same order as the node id (cheapest case)
        "flat" => Shape {
            key: |i, _n| format!("node-{i:08}"),
            attr: |i, a| ((i * (a + 3)) % 1000) as i64,
        },
        // clustered, values grow with position plus sub-stride jitter
        // (date-like) — the shape under which range-pruning numbers are quotable
        "mono" => Shape {
            key: |i, _n| format!("node-{i:08}"),
            attr: |i, a| (i * (a + 3) + (i % 7)) as i64,
        },
        // a key whose sort order is a permutation of the node order
        "ring" => Shape {
            key: |i, n| format!("node-{:08}", (i * 7919) % n),
            attr: |i, a| ((i * (a + 3)) % 1000) as i64,
        },
        other => panic!("unknown synthetic shape {other}"),
    }
}

/// Synthetic shapes for the size and seek benches: a unique `id` column
/// (dense node id), a unique string `key` (the business key a seek targets),
/// and `attrs` integer property columns. Two attr distributions:
/// - `flat`: repeating (`(i*(a+3))%1000`) — every zone spans the full value
///   range, so zone-map pruning can never fire (the anti-shape; kept for the
///   dict-encoded aggregate/join kernels that want low cardinality)
/// - `mono`: clustered (see [`shape`]) — the range-pruning shape
pub fn synthetic(kind: &str, n: usize, attrs: usize) -> Table {
    let shape = shape(kind);
    let ids: Vec<i64> = (0..n as i64).collect();
    let keys: Vec<String> = (0..n).map(|i| (shape.key)(i, n)).collect();
    let mut columns: Vec<(String, Col)> =
        vec![("id".into(), Col::Int(ids)), ("key".into(), Col::Str(keys))];
    for a in 0..attrs {
        let v: Vec<i64> = (0..n).map(|i| (shape.attr)(i, a)).collect();
        columns.push((format!("attr{a}"), Col::Int(v)));
    }
    Table {
        name: format!("synthetic-{kind}-{n}-{attrs}"),
        columns,
    }
}

/// The pokec shape the fixture builds (`testing/relate-test-support/src/pokec.rs`):
/// `:User {id, completion_percentage, gender, age}`, ids `1..=nodes`, drawn from
/// the same xorshift stream in the fixture's order, so the same seed pins the
/// same rows (the association/edge stream is out of scope here — the lab
/// measures property columns, not traversal).
pub fn pokec(nodes: usize, seed: u64) -> Table {
    let mut rng = Rng(seed);
    let mut id = Vec::with_capacity(nodes);
    let mut completion = Vec::with_capacity(nodes);
    let mut gender: Vec<String> = Vec::with_capacity(nodes);
    let mut age = Vec::with_capacity(nodes);
    for i in 1..=nodes as u64 {
        // The fixture's documented young-age skew: one coin puts ~1/800 users
        // in the old arm, so `age > 50` stays the small-answer filter class.
        let old = rng.below(800) == 0;
        let a = 1 + rng.below(50) + if old { 50 } else { 0 };
        id.push(i as i64);
        completion.push(rng.below(101) as i64);
        gender.push(if rng.below(2) == 0 { "man" } else { "woman" }.to_string());
        age.push(a as i64);
    }
    Table {
        name: format!("pokec-{nodes}"),
        columns: vec![
            ("id".into(), Col::Int(id)),
            ("completion_percentage".into(), Col::Int(completion)),
            ("gender".into(), Col::Str(gender)),
            ("age".into(), Col::Int(age)),
        ],
    }
}

/// The scan result as per-chunk arrays (a chunked root is unwrapped).
pub fn chunks(arr: &ArrayRef) -> Vec<ArrayRef> {
    match arr.as_opt::<Chunked>() {
        Some(c) => (0..c.nchunks()).map(|i| c.chunk(i).clone()).collect(),
        None => vec![arr.clone()],
    }
}

/// One vortex scan's knobs — every kernel's read is one of these shapes.
#[derive(Default)]
pub struct ScanOpts<'a> {
    pub filter: Option<&'a BoundExpression>,
    pub projection: Option<&'a BoundExpression>,
    pub rows: Option<&'a StrictSortedBuffer<u64>>,
    pub range: Option<Range<u64>>,
    pub limit: Option<u64>,
    pub concurrency: Option<usize>,
    pub max_splits_in_flight: Option<NonZeroUsize>,
}

/// Run one scan with `opts`; the caller unwraps/normalizes the result shape.
/// Generic over the blocking runtime so the engine store's `Send` current-
/// thread runtime drives the same one scan shape a kernel's single-thread
/// runtime does.
pub fn scan<R: BlockingRuntime>(
    vfile: &VortexFile,
    rt: &R,
    opts: ScanOpts<'_>,
) -> VortexResult<ArrayRef> {
    rt.block_on(async {
        let mut b = vfile.scan()?;
        if let Some(f) = opts.filter {
            b = b.with_filter(f.clone());
        }
        if let Some(p) = opts.projection {
            b = b.with_projection(p.clone());
        }
        if let Some(r) = opts.rows {
            b = b.with_row_indices(r.clone());
        }
        if let Some(r) = opts.range {
            b = b.with_row_range(r);
        }
        if let Some(l) = opts.limit {
            b = b.with_limit(l);
        }
        if let Some(c) = opts.concurrency {
            b = b.with_concurrency(c);
        }
        if let Some(m) = opts.max_splits_in_flight {
            b = b.with_some_max_splits_in_flight(Some(m));
        }
        b.into_array_stream()?.read_all().await
    })
}

/// Normalize a projected read to the field array: a read may yield the field
/// itself or a struct holding it (the scan's two documented shapes).
pub fn field_of(array: ArrayRef, name: &str) -> VortexResult<ArrayRef> {
    match array.as_opt::<Struct>() {
        Some(view) => Ok(view.unmasked_field_by_name(name)?.clone()),
        None => Ok(array),
    }
}

/// Read one projected column of a file — the crate's one single-column read.
/// The unprojected root is a chunked struct; a single-field projection yields
/// the field (or a struct holding it). Any other shape is an error, never a
/// silently different column.
pub fn projected<R: BlockingRuntime>(
    rt: &R,
    vfile: &VortexFile,
    name: &str,
) -> Result<ArrayRef, Box<dyn Error>> {
    let projection: BoundExpression = col(name).bind(vfile.dtype())?.optimize_recursive()?;
    let want = projection.dtype().clone();
    let arr = scan(
        vfile,
        rt,
        ScanOpts {
            projection: Some(&projection),
            ..ScanOpts::default()
        },
    )?;
    if arr.as_opt::<Struct>().is_none() && *arr.dtype() != want {
        return Err(format!(
            "projected scan of {name} yielded {} instead of the column",
            arr.dtype()
        )
        .into());
    }
    Ok(field_of(arr, name)?)
}

/// The write step's report: file bytes, and the writer's per-column
/// compressed sizes when it reports them.
pub type Written = (u64, Option<Vec<u64>>);

/// Write a table as a vortex file.
pub fn write(path: &Path, table: &Table) -> Result<Written, Box<dyn Error>> {
    let rt = SingleThreadRuntime::default();
    let session = VortexSession::default().with_handle(rt.handle());
    let array = table.to_vortex()?;
    let out = fs::File::create(path)?;
    let summary = session
        .write_options()
        .blocking(&rt)
        .write(out, array.to_array_iterator())?;
    Ok((summary.size(), summary.compressed_column_sizes().ok()))
}

/// Print the write step's size report: the one header + per-column table the
/// kernels' size and store lines share. A column the writer did not report
/// prints "sizes unavailable" instead of vanishing from the table.
pub fn print_sizes(table: &Table, per_column: Option<&[u64]>, written: u64) {
    let vortex_bytes: u64 = per_column.map_or(0, |sizes| sizes.iter().sum());
    println!(
        "size: {} — {} rows, {} columns, {} B raw -> {} B vortex ({:.1}%), file {} B",
        table.name,
        table.rows(),
        table.columns.len(),
        table.raw_bytes(),
        vortex_bytes,
        100.0 * vortex_bytes as f64 / table.raw_bytes().max(1) as f64,
        written
    );
    for (index, (name, col)) in table.columns.iter().enumerate() {
        match per_column.and_then(|sizes| sizes.get(index)) {
            Some(bytes) => println!(
                "  {:<14} {:<5} {:>8} values {:>10} B raw -> {:>9} B ({:>6.2} B/value)",
                name,
                format!("{:?}", col.ty()),
                col.len(),
                col.raw_bytes(),
                bytes,
                *bytes as f64 / col.len().max(1) as f64
            ),
            None => println!(
                "  {:<14} {:<5} {:>8} values {:>10} B raw -> sizes unavailable",
                name,
                format!("{:?}", col.ty()),
                col.len(),
                col.raw_bytes()
            ),
        }
    }
}

/// Open a vortex file through an mmap: zero decode, pages fault in on use.
///
/// # Safety
///
/// `Mmap::map` requires the file's contents not to change for the mapping's
/// life. The lab writes each file into its own run directory and opens it
/// read-only; no handle writes an opened file, and the directory is private
/// to this process, so nothing else truncates or rewrites it while mapped.
pub fn open_mmap(
    path: &Path,
) -> Result<(VortexSession, SingleThreadRuntime, vortex::file::VortexFile), Box<dyn Error>> {
    let rt = SingleThreadRuntime::default();
    let session = VortexSession::default().with_handle(rt.handle());
    let f = fs::File::open(path)?;
    let mmap = unsafe { memmap2::Mmap::map(&f)? };
    let vfile = session.open_options().open_buffer(ByteBuffer::from(mmap))?;
    Ok((session, rt, vfile))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vortex::array::VortexSessionExecute;

    /// The shared projected read returns exactly the named column, and an
    /// absent column is an error rather than a silently different array.
    #[test]
    fn projected_reads_the_named_column() {
        let dir = tempfile::tempdir().unwrap();
        let table = synthetic("flat", 100, 2);
        let path = dir.path().join("projected.vortex");
        write(&path, &table).unwrap();
        let (session, rt, vfile) = open_mmap(&path).unwrap();

        let column = projected(&rt, &vfile, "attr1").unwrap();
        assert_eq!(column.len(), 100);
        let mut ctx = session.create_execution_ctx();
        let decoded = column.execute::<PrimitiveArray>(&mut ctx).unwrap();
        assert_eq!(decoded.as_slice::<i64>(), table.ints("attr1").unwrap());

        assert!(projected(&rt, &vfile, "absent").is_err());
    }

    /// The loader's typed accessor rejects a missing column and a non-int one.
    #[test]
    fn ints_rejects_missing_and_non_int_columns() {
        let table = synthetic("flat", 4, 1);
        assert_eq!(table.ints("attr0").unwrap(), &[0, 3, 6, 9]);
        assert!(table.ints("key").is_err());
        assert!(table.ints("absent").is_err());
    }
}
