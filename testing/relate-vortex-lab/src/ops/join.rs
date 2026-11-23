//! O7 property-key join, O8 intra-row / cross-row property compare, O11
//! gather-then-compare (spec: `workload-mapping-findings.md`).
//!
//! O7 join key = the `attr0` integer property (1000 distinct values) of the
//! synthetic table split into two row halves — the `a.birthday = b.birthday`
//! shape: an equi-join on a low-cardinality property, not a unique key. The
//! merge side assumes both sides arrive key-sorted (the "key-sorted index on
//! both sides" the spec names); the unsorted sort-then-merge variant measures
//! the no-index case.
//!
//! O8/O11 comparisons go through vortex's expression layer so the native
//! compare kernel runs: `vortex_array::scalar_fn::fns::binary::compare::
//! compare_primitive` (lane kernels in `vortex-compute::lane_kernels`,
//! bitmap output), reached via `lt(col, col)` on a struct root.

use std::collections::HashMap;
use std::path::Path;

use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::arrays::struct_::StructArray;
use vortex::array::expr::{col, lt, root, select, Expression};
use vortex::array::stream::ArrayStreamExt;
use vortex::array::validity::Validity;
use vortex::array::{ArrayRef, IntoArray, VortexSessionExecute};
use vortex::buffer::{BitBuffer, Buffer};
use vortex::io::runtime::BlockingRuntime;
use vortex::scan::strict_sorted_buffer::StrictSortedBuffer;
use vortex::session::VortexSession;
use vortex::VortexSessionDefault;

use crate::bench::{measure, Measured};
use crate::store::{self, Col, Table};

pub fn run(out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();
    // Join/frame-scale kernels cost ms per iteration; the compare kernels are
    // cheap. Both stay inside the smoke bound (<= 2000).
    let jit = iters.min(20).max(1);
    let cit = iters.min(1000).max(1);

    let n = 150_000usize;
    let table = store::synthetic("flat", n, 4);
    let attr0 = ints(&table, "attr0")?;
    let attr1 = ints(&table, "attr1")?;

    // ---- O7: equi-join on the property key -----------------------------
    let half = n / 2;
    let unsorted_left: Vec<(i64, u32)> = (0..half).map(|i| (attr0[i], i as u32)).collect();
    let unsorted_right: Vec<(i64, u32)> = (half..n).map(|i| (attr0[i], i as u32)).collect();
    let mut left = unsorted_left.clone();
    let mut right = unsorted_right.clone();
    left.sort_unstable();
    right.sort_unstable();

    let expected = merge_count(&left, &right);
    let mut hash_matches = 0u64;

    let o7a = measure("O7a sort-merge join (both sides key-sorted)", jit, |_| {
        let m = merge_count(&left, &right);
        std::hint::black_box(m);
    });
    o7a.report();
    results.push(o7a);

    let o7b = measure("O7b hash join (build both-side rows, probe)", jit, |_| {
        let mut build: HashMap<i64, Vec<u32>> = HashMap::with_capacity(left.len());
        for &(k, r) in &left {
            build.entry(k).or_default().push(r);
        }
        let mut matches = 0u64;
        for &(k, _) in &right {
            if let Some(rows) = build.get(&k) {
                matches += rows.len() as u64;
            }
        }
        hash_matches = matches;
        std::hint::black_box(matches);
    });
    o7b.report();
    results.push(o7b);

    let o7c = measure("O7c sort both sides + merge (no index)", jit, |_| {
        let mut l = unsorted_left.clone();
        let mut r = unsorted_right.clone();
        l.sort_unstable();
        r.sort_unstable();
        std::hint::black_box(merge_count(&l, &r));
    });
    o7c.report();
    results.push(o7c);

    assert_eq!(hash_matches, expected, "hash join must agree with sort-merge");
    println!(
        "  O7 join key: attr0, {} rows/side, {} distinct keys, {} matches (hash == merge)",
        half,
        left.iter().map(|(k, _)| *k).collect::<std::collections::BTreeSet<_>>().len(),
        expected
    );

    // ---- O8: intra-row and cross-row property compare -------------------
    let mem_session = VortexSession::default();
    let mut ctx = mem_session.create_execution_ctx();

    // (a) decoded arrays, per-row scalar reads.
    let o8a = measure("O8a intra-row compare, decoded vecs per row", cit, |_| {
        let mut c = 0usize;
        for i in 0..n {
            if attr0[i] < attr1[i] {
                c += 1;
            }
        }
        std::hint::black_box(c);
    });
    o8a.report();
    results.push(o8a);

    // (b) one vectorized compare pass: lt(col, col) over a two-field struct.
    let row_a = prim(&attr0);
    let row_b = prim(&attr1);
    let intra = StructArray::try_new(
        vec!["l", "r"].into(),
        [row_a, row_b],
        n,
        Validity::NonNullable,
    )?
    .into_array();
    let intra_bits = run_compare(intra.clone(), &lt(col("l"), col("r")), &mut ctx)?;
    let scalar_intra = (0..n).filter(|&i| attr0[i] < attr1[i]).count();
    assert_eq!(intra_bits.true_count(), scalar_intra, "O8b must match O8a");
    let intra_bound = lt(col("l"), col("r")).bind(intra.dtype())?;
    let o8b = measure("O8b intra-row compare, vectorized kernel", cit, |_| {
        let bits: BitBuffer = intra
            .clone()
            .apply_bound(&intra_bound)
            .unwrap()
            .execute(&mut ctx)
            .unwrap();
        std::hint::black_box(bits.true_count());
    });
    o8b.report();
    results.push(o8b);

    // (c) row i vs row i+1 of the same column (the `ti.date < tj.date` shape):
    // two constant-time slices of one column.
    let full = prim(&attr0);
    let shifted = StructArray::try_new(
        vec!["l", "r"].into(),
        [full.slice(0..n - 1)?, full.slice(1..n)?],
        n - 1,
        Validity::NonNullable,
    )?
    .into_array();
    let scalar_shift = (0..n - 1).filter(|&i| attr0[i] < attr0[i + 1]).count();
    let shift_bits = run_compare(shifted.clone(), &lt(col("l"), col("r")), &mut ctx)?;
    assert_eq!(shift_bits.true_count(), scalar_shift, "O8c must match scalar");
    let shift_bound = lt(col("l"), col("r")).bind(shifted.dtype())?;
    let o8c = measure("O8c cross-row (i vs i+1) compare, vectorized", cit, |_| {
        let bits: BitBuffer = shifted
            .clone()
            .apply_bound(&shift_bound)
            .unwrap()
            .execute(&mut ctx)
            .unwrap();
        std::hint::black_box(bits.true_count());
    });
    o8c.report();
    results.push(o8c);
    println!(
        "  O8 matches: intra {} , cross-row {} (rows {})",
        scalar_intra, scalar_shift, n
    );

    // ---- O11: gather a row subset then compare, vs compare all ----------
    let path = out_dir.join("join-o11.vortex");
    store::write(&path, &table)?;
    let (_file_session, rt, vfile) = store::open_mmap(&path)?;
    let positions: Vec<u64> = (0..1024u64).map(|i| i * 137).collect();
    let selection = StrictSortedBuffer::try_new(Buffer::from(positions.clone()))?;
    let projection = select(["attr0", "attr1"], root()).bind(vfile.dtype())?;

    let mut gather_matches = 0usize;
    let o11a = measure("O11a gather 1024 rows + compare", jit, |_| {
        let rows = rt
            .block_on(async {
                vfile
                    .scan()?
                    .with_projection(projection.clone())
                    .with_row_indices(selection.clone())
                    .into_array_stream()?
                    .read_all()
                    .await
            })
            .unwrap();
        let bits = run_compare(rows, &lt(col("attr0"), col("attr1")), &mut ctx).unwrap();
        gather_matches = bits.true_count();
        std::hint::black_box(gather_matches);
    });
    o11a.report();
    results.push(o11a);

    let mut scan_matches = 0usize;
    let o11b = measure("O11b compare all rows, filter at 1024 positions", jit, |_| {
        let rows = rt
            .block_on(async {
                vfile
                    .scan()?
                    .with_projection(projection.clone())
                    .into_array_stream()?
                    .read_all()
                    .await
            })
            .unwrap();
        let bits = run_compare(rows, &lt(col("attr0"), col("attr1")), &mut ctx).unwrap();
        scan_matches = positions
            .iter()
            .filter(|&&i| bits.value(i as usize))
            .count();
        std::hint::black_box(scan_matches);
    });
    o11b.report();
    results.push(o11b);

    assert_eq!(
        gather_matches, scan_matches,
        "gather-then-compare must agree with compare-all-then-filter"
    );
    println!(
        "  O11 {} of {} gathered positions match, both paths agree",
        gather_matches,
        positions.len()
    );

    Ok(results)
}

/// Pairs matching on the key, counted in one pass over two key-sorted lists:
/// duplicate runs multiply instead of materializing row pairs.
fn merge_count(left: &[(i64, u32)], right: &[(i64, u32)]) -> u64 {
    let (mut i, mut j, mut matches) = (0usize, 0usize, 0u64);
    while i < left.len() && j < right.len() {
        match left[i].0.cmp(&right[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                let key = left[i].0;
                let li = i;
                while i < left.len() && left[i].0 == key {
                    i += 1;
                }
                let rj = j;
                while j < right.len() && right[j].0 == key {
                    j += 1;
                }
                matches += ((i - li) as u64) * ((j - rj) as u64);
            }
        }
    }
    matches
}

fn ints(table: &Table, name: &str) -> Result<Vec<i64>, Box<dyn std::error::Error>> {
    match table.column(name) {
        Some(Col::Int(v)) => Ok(v.clone()),
        _ => Err(format!("{name} is not an int column").into()),
    }
}

fn prim(values: &[i64]) -> ArrayRef {
    PrimitiveArray::new(Buffer::from(values.to_vec()), Validity::NonNullable).into_array()
}

/// Apply `expr` to `array` and execute the compare kernel to a bitmap.
fn run_compare(
    array: ArrayRef,
    expr: &Expression,
    ctx: &mut vortex::array::ExecutionCtx,
) -> Result<BitBuffer, Box<dyn std::error::Error>> {
    let bound = expr.bind(array.dtype())?;
    Ok(array.apply_bound(&bound)?.execute(ctx)?)
}
