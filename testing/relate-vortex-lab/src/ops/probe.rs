//! O2 and O10b mechanism probes.
//!
//! O2 asked why three string reads at the same rows cost 18x one read. The
//! probe reports the decoded root's encoding tree, the per-column probe cost,
//! and two batched alternatives (a `take` gather on the decoded columns, and
//! one projected scan with a row selection).
//!
//! O10b asked what `with_row_indices` actually does. The probe measures a
//! fresh scan, a prepared scan, a row range, the same selection on contiguous
//! rows, the whole-file span, and a `take` kernel on already-decoded columns —
//! every number per row.

use std::path::Path;

use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::arrays::struct_::{Struct, StructArrayExt};
use vortex::array::stream::ArrayStreamExt;
use vortex::array::validity::Validity;
use vortex::array::VortexSessionExecute;
use vortex::array::{ArrayRef, Canonical, IntoArray};
use vortex::buffer::Buffer;
use vortex::expr::{get_item, root, select, Expression};
use vortex::io::runtime::BlockingRuntime;
use vortex::scalar::Scalar;
use vortex::scan::strict_sorted_buffer::StrictSortedBuffer;

use crate::bench::{measure, spread_row, Measured};
use crate::store::{self, Ty};

const PERSON_COLS: &[(&str, Ty)] = &[
    ("id", Ty::Int),
    ("firstName", Ty::Str),
    ("lastName", Ty::Str),
    ("gender", Ty::Str),
    ("birthday", Ty::Str),
    ("browserUsed", Ty::Str),
];

/// The O2 columns, in point.rs's order.
const PROPS: [&str; 4] = ["firstName", "lastName", "gender", "birthday"];

pub fn run(
    csv: &Path,
    out_dir: &Path,
    iters: u64,
) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut all = Vec::new();
    all.extend(o2(csv, out_dir, iters)?);
    all.extend(o10b(out_dir, iters)?);
    Ok(all)
}

fn o2(csv: &Path, out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();
    let table = store::load_csv(csv, PERSON_COLS)?;
    let path = out_dir.join("probe-person.vortex");
    store::write(&path, &table)?;
    let (session, rt, vfile) = store::open_mmap(&path)?;
    let decoded = rt.block_on(async { vfile.scan()?.into_array_stream()?.read_all().await })?;
    let rows = decoded.len() as u64;

    println!("\n== O2: {rows} rows; encoding tree of the decoded root ==");
    dump("root", &decoded, 0);

    let fields: Vec<ArrayRef> = {
        let view = decoded.as_opt::<Struct>().ok_or("root is not a struct")?;
        PROPS
            .iter()
            .map(|n| view.unmasked_field_by_name(n).cloned())
            .collect::<Result<_, _>>()?
    };
    let mut ctx = session.create_execution_ctx();

    // Per-column cost of the one-off probe path point.rs measured.
    for (name, f) in PROPS.iter().zip(&fields) {
        let m = measure(
            &format!("O2 old: 1 col execute_scalar ({name})"),
            iters,
            |i| {
                std::hint::black_box(f.execute_scalar(spread_row(i, rows), &mut ctx).unwrap());
            },
        );
        m.report();
        results.push(m);
    }

    let combined = measure("O2 old: 3 cols execute_scalar, same row", iters, |i| {
        let r = spread_row(i, rows);
        let a = fields[0].execute_scalar(r, &mut ctx).unwrap();
        let b = fields[1].execute_scalar(r, &mut ctx).unwrap();
        let c = fields[3].execute_scalar(r, &mut ctx).unwrap();
        std::hint::black_box((a, b, c));
    });
    combined.report();
    results.push(combined);

    // The same three reads through a retained probe (state kept between reads).
    let mut probes: Vec<_> = fields.iter().map(ArrayRef::repeated_probe).collect();
    let retained = measure("O2 new: 3 cols repeated_probe, same row", iters, |i| {
        let r = spread_row(i, rows);
        let a = probes[0].execute_scalar(r, &mut ctx).unwrap();
        let b = probes[1].execute_scalar(r, &mut ctx).unwrap();
        let c = probes[3].execute_scalar(r, &mut ctx).unwrap();
        std::hint::black_box((a, b, c));
    });
    retained.report();
    results.push(retained);

    // A morsel: 1024 rows, read across all four columns, three ways.
    let mut ids: Vec<u64> = (0..1024.min(rows)).map(|i| (i * 7919) % rows).collect();
    ids.sort_unstable();
    ids.dedup();
    let n = ids.len();
    let idx = || PrimitiveArray::new(Buffer::from(ids.clone()), Validity::NonNullable).into_array();

    let old_morsel = measure(
        "O2 old: per-value probes, 4 cols x rows",
        (iters / 20).max(2),
        |_| {
            let mut total = 0usize;
            for &r in &ids {
                for f in &fields {
                    total += utf8(&f.execute_scalar(r as usize, &mut ctx).unwrap()).len();
                }
            }
            std::hint::black_box(total);
        },
    );
    per_row("O2 old: per-value probes, 4 cols", &old_morsel, n as u64);
    results.push(old_morsel);

    let gather = measure(
        "O2 new: take 4 cols x rows (batched)",
        (iters / 20).max(2),
        |_| {
            let mut total = 0usize;
            for f in &fields {
                let canon = f
                    .take(idx())
                    .unwrap()
                    .execute::<Canonical>(&mut ctx)
                    .unwrap();
                let v = canon.as_varbinview();
                for j in 0..n {
                    total += v.bytes_at(j).len();
                }
            }
            std::hint::black_box(total);
        },
    );
    per_row("O2 new: take 4 cols (batched)", &gather, n as u64);
    results.push(gather);

    // Correctness: the gather must agree with the per-value probe.
    let at = n / 2;
    let want = utf8(&fields[0].execute_scalar(ids[at] as usize, &mut ctx)?);
    let canon = fields[0].take(idx())?.execute::<Canonical>(&mut ctx)?;
    let got = canon.as_varbinview().bytes_at(at);
    assert_eq!(
        String::from_utf8_lossy(got.as_slice()),
        want,
        "batched take disagrees with execute_scalar"
    );

    // Decoding the columns once is what makes a per-value read cheap: the
    // probes above pay a block decode inside the encoding on every call.
    let decode = measure(
        "O2 new: decode 4 cols to canonical, once",
        (iters / 100).max(2),
        |_| {
            let canon: Vec<Canonical> = fields
                .iter()
                .map(|f| f.clone().execute::<Canonical>(&mut ctx).unwrap())
                .collect();
            std::hint::black_box(canon);
        },
    );
    per_row("O2 new: decode 4 cols to canonical", &decode, rows);
    results.push(decode);

    let canon: Vec<Canonical> = fields
        .iter()
        .map(|f| f.clone().execute::<Canonical>(&mut ctx))
        .collect::<Result<_, _>>()?;
    let reads = measure(
        "O2 new: canonical per-value reads, 4 cols x rows",
        (iters / 20).max(2),
        |_| {
            let mut total = 0usize;
            for c in &canon {
                let v = c.as_varbinview();
                for &r in &ids {
                    total += v.bytes_at(r as usize).len();
                }
            }
            std::hint::black_box(total);
        },
    );
    per_row(
        "O2 new: canonical per-value reads, 4 cols",
        &reads,
        n as u64,
    );
    results.push(reads);

    // One projected scan with a row selection: the file reader's own path.
    let sel = StrictSortedBuffer::try_new(Buffer::from(ids))?;
    let proj = select(PROPS, root())
        .bind(vfile.dtype())?
        .optimize_recursive()?;
    let scan = measure(
        "O2 new: projection + row_indices scan",
        (iters / 100).max(2),
        |_| {
            let batch = store::scan(
                &vfile,
                &rt,
                store::ScanOpts {
                    projection: Some(&proj),
                    rows: Some(&sel),
                    ..store::ScanOpts::default()
                },
            )
            .unwrap();
            assert_eq!(batch.len(), n, "scan returned the wrong row count");
            std::hint::black_box(batch);
        },
    );
    per_row("O2 new: projection + row_indices scan", &scan, n as u64);
    results.push(scan);

    Ok(results)
}

/// A row selection for the O10b scan arms: a sorted row list or a
/// contiguous range — one argument, one type, so neither arm can pass the
/// wrong shape.
enum Slice {
    Rows(StrictSortedBuffer<u64>),
    Range(std::ops::Range<u64>),
}

fn o10b(out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();
    let n = 150_000usize;
    let table = store::synthetic("flat", n, 4);
    let path = out_dir.join("probe-synthetic-flat.vortex");
    let (written, _) = store::write(&path, &table)?;
    println!("\n== O10b: {n} rows synthetic flat, file {written} B ==");
    let (session, rt, vfile) = store::open_mmap(&path)?;
    let decoded = rt.block_on(async { vfile.scan()?.into_array_stream()?.read_all().await })?;
    let rows = decoded.len() as u64;
    dump("root", &decoded, 0);

    // 1024 ids spread over the file (seek.rs's shape) and 1024 consecutive rows (a morsel).
    let mut sparse: Vec<u64> = (0..1024u64).map(|i| (i * 7919) % rows).collect();
    sparse.sort_unstable();
    sparse.dedup();
    let block: Vec<u64> = (rows / 2..rows / 2 + 1024).collect();
    let vs = sparse.len() as u64;
    let vb = block.len() as u64;
    let sparse_sel = StrictSortedBuffer::try_new(Buffer::from(sparse.clone()))?;
    let block_sel = StrictSortedBuffer::try_new(Buffer::from(block.clone()))?;

    let scan_iters = (iters / 200).max(2);

    // Reference: a whole-file scan says what one decoded row costs at all.
    let m = measure("O10b full scan (all rows), reference", 2, |_| {
        let batch = store::scan(&vfile, &rt, store::ScanOpts::default()).unwrap();
        assert_eq!(batch.len() as u64, rows);
        std::hint::black_box(batch);
    });
    per_row("O10b full scan, reference", &m, rows);
    results.push(m);

    let fresh = |slice: Slice, expect: u64| {
        let opts = match &slice {
            Slice::Rows(sel) => store::ScanOpts {
                rows: Some(sel),
                ..store::ScanOpts::default()
            },
            Slice::Range(range) => store::ScanOpts {
                range: Some(range.clone()),
                ..store::ScanOpts::default()
            },
        };
        let batch = store::scan(&vfile, &rt, opts).unwrap();
        assert_eq!(
            batch.len(),
            expect as usize,
            "scan returned the wrong row count"
        );
        std::hint::black_box(batch);
    };

    let m = measure("O10b row_range 1 row, fresh scan", scan_iters, |_| {
        fresh(Slice::Range(rows / 2..rows / 2 + 1), 1)
    });
    per_row("O10b row_range 1 row", &m, 1);
    results.push(m);

    let m = measure("O10b row_indices sparse, fresh scan", scan_iters, |_| {
        fresh(Slice::Rows(sparse_sel.clone()), vs)
    });
    per_row("O10b row_indices sparse, fresh scan", &m, vs);
    results.push(m);

    let prepared = vfile
        .scan()?
        .with_row_indices(sparse_sel.clone())
        .prepare()?;
    let m = measure("O10b row_indices sparse, prepared scan", scan_iters, |_| {
        let batch = rt
            .block_on(async {
                prepared
                    .execute_array_stream(None::<std::ops::Range<u64>>)?
                    .read_all()
                    .await
            })
            .unwrap();
        assert_eq!(
            batch.len(),
            vs as usize,
            "prepared scan returned the wrong row count"
        );
        std::hint::black_box(batch);
    });
    per_row("O10b row_indices sparse, prepared scan", &m, vs);
    results.push(m);

    let m = measure(
        "O10b row_range contiguous (1024), fresh scan",
        scan_iters,
        |_| fresh(Slice::Range(rows / 2..rows / 2 + 1024), vb),
    );
    per_row("O10b row_range contiguous, fresh scan", &m, vb);
    results.push(m);

    let m = measure(
        "O10b row_indices contiguous (1024), fresh scan",
        scan_iters,
        |_| fresh(Slice::Rows(block_sel.clone()), vb),
    );
    per_row("O10b row_indices contiguous, fresh scan", &m, vb);
    results.push(m);

    // The sparse ids' whole span: what the reader is implicitly asked for.
    let span = sparse[0]..sparse[sparse.len() - 1] + 1;
    let span_rows = span.end - span.start;
    let m = measure(
        "O10b row_range over the sparse span, fresh scan",
        scan_iters,
        |_| fresh(Slice::Range(span.clone()), span_rows),
    );
    per_row("O10b row_range over the sparse span", &m, vs);
    results.push(m);

    let prepared_range = vfile
        .scan()?
        .with_row_range(rows / 2..rows / 2 + 1024)
        .prepare()?;
    let m = measure(
        "O10b row_range contiguous (1024), prepared scan",
        scan_iters,
        |_| {
            let batch = rt
                .block_on(async {
                    prepared_range
                        .execute_array_stream(None::<std::ops::Range<u64>>)?
                        .read_all()
                        .await
                })
                .unwrap();
            assert_eq!(batch.len(), vb as usize);
            std::hint::black_box(batch);
        },
    );
    per_row("O10b row_range contiguous, prepared scan", &m, vb);
    results.push(m);

    // Gather on already-decoded arrays: per-column takes and per-value probes.
    let key = projection_column(&rt, &vfile, get_item("key", root()))?;
    println!(
        "decoded key column: {:?}, {} rows, {} chunks",
        key.encoding_id(),
        key.len(),
        key.slots().len()
    );
    let four = projection_column(&rt, &vfile, select(["id", "key", "attr0", "attr1"], root()))?;
    let mut ctx = session.create_execution_ctx();
    let idx =
        || PrimitiveArray::new(Buffer::from(sparse.clone()), Validity::NonNullable).into_array();
    let gather_iters = (iters / 20).max(2);

    let m = measure(
        "O10b take kernel on decoded key, sparse",
        gather_iters,
        |_| {
            let canon = key
                .take(idx())
                .unwrap()
                .execute::<Canonical>(&mut ctx)
                .unwrap();
            let v = canon.as_varbinview();
            let mut total = 0usize;
            for j in 0..sparse.len() {
                total += v.bytes_at(j).len();
            }
            std::hint::black_box(total);
        },
    );
    per_row("O10b take kernel on decoded key", &m, vs);
    results.push(m);

    let m = measure(
        "O10b take on decoded 4-col struct, sparse",
        gather_iters,
        |_| {
            let canon = four
                .clone()
                .take(idx())
                .unwrap()
                .execute::<Canonical>(&mut ctx)
                .unwrap();
            let k = canon.as_struct().unmasked_field_by_name("key").unwrap();
            let mut total = 0usize;
            for j in 0..sparse.len() {
                total += utf8(&k.execute_scalar(j, &mut ctx).unwrap()).len();
            }
            std::hint::black_box(total);
        },
    );
    per_row("O10b take on decoded 4-col struct + read", &m, vs);
    results.push(m);

    let m = measure(
        "O10b per-value probes on decoded key, sparse",
        gather_iters,
        |_| {
            let mut total = 0usize;
            for &r in &sparse {
                total += utf8(&key.execute_scalar(r as usize, &mut ctx).unwrap()).len();
            }
            std::hint::black_box(total);
        },
    );
    per_row("O10b per-value probes on decoded key", &m, vs);
    results.push(m);

    let decode = measure(
        "O10b decode key to canonical, once",
        (iters / 100).max(2),
        |_| {
            let c = key.clone().execute::<Canonical>(&mut ctx).unwrap();
            std::hint::black_box(c);
        },
    );
    per_row("O10b decode key to canonical", &decode, rows);
    results.push(decode);

    let canon_key = key.clone().execute::<Canonical>(&mut ctx)?;
    let reads = measure("O10b canonical reads on key, sparse", gather_iters, |_| {
        let v = canon_key.as_varbinview();
        let mut total = 0usize;
        for &r in &sparse {
            total += v.bytes_at(r as usize).len();
        }
        std::hint::black_box(total);
    });
    per_row("O10b canonical reads on key", &reads, vs);
    results.push(reads);

    // Correctness: a projected contiguous range scan must agree with the decoded column.
    let projection = get_item("key", root())
        .bind(vfile.dtype())?
        .optimize_recursive()?;
    let batch = store::scan(
        &vfile,
        &rt,
        store::ScanOpts {
            projection: Some(&projection),
            range: Some(rows / 2..rows / 2 + 1024),
            ..store::ScanOpts::default()
        },
    )?;
    for j in [0usize, 1, 511, 1023] {
        let want = utf8(&key.execute_scalar((rows / 2) as usize + j, &mut ctx)?);
        let got = utf8(&batch.execute_scalar(j, &mut ctx)?);
        assert_eq!(
            got, want,
            "range scan row {j} disagrees with the decoded column"
        );
    }

    Ok(results)
}

/// Read one projected column (or struct) of the whole file through a scan.
fn projection_column(
    rt: &vortex::io::runtime::single::SingleThreadRuntime,
    vfile: &vortex::file::VortexFile,
    expr: Expression,
) -> Result<ArrayRef, Box<dyn std::error::Error>> {
    let proj = expr.bind(vfile.dtype())?.optimize_recursive()?;
    Ok(store::scan(
        vfile,
        rt,
        store::ScanOpts {
            projection: Some(&proj),
            ..store::ScanOpts::default()
        },
    )?)
}

/// One report line for a kernel that handles `rows` rows per op.
fn per_row(name: &str, m: &Measured, rows: u64) {
    println!(
        "{:<44} {:>9.1} ns/row  {:>8.2} allocs/row  ({} rows/op, {:>9.1} ns/op, load {:.1}->{:.1})",
        name,
        m.ns_per_op / rows.max(1) as f64,
        m.allocs_per_op / rows.max(1) as f64,
        rows,
        m.ns_per_op,
        m.load_start,
        m.load_end
    );
}

fn utf8(s: &Scalar) -> String {
    s.value()
        .map(|v| v.as_utf8().as_str().to_string())
        .unwrap_or_default()
}

/// Print the encoding tree — the column encodings decide what a probe costs.
fn dump(name: &str, arr: &ArrayRef, depth: usize) {
    println!(
        "{}{} id={:?} dtype={:?} len={} canonical={}",
        "  ".repeat(depth),
        name,
        arr.encoding_id(),
        arr.dtype(),
        arr.len(),
        arr.is_canonical()
    );
    if depth < 4 {
        for (i, slot) in arr.slots().iter().enumerate() {
            if let Some(child) = slot {
                dump(&format!("[{i}] {}", arr.slot_name(i)), child, depth + 1);
            }
        }
    }
}
