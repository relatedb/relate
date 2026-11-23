//! O4 range filter -> count/exists, O5 conjunctive filter, O12 prefix range
//! (STARTS WITH), O16 presence / IS NULL.
//!
//! Every kernel is a vortex-native scan (the predicate pushed into the layout
//! reader) against its in-memory equivalent. The scan builder has no count
//! terminal, so a "count" pays for materialising the projected column at the
//! surviving rows — that is the honest vortex path and is measured as such.
//!
//! Per-split zone-map pruning is measured directly through
//! `LayoutReader::pruning_evaluation`: a stats-only pass (no payload read),
//! called once per natural split, so the split count it reports is the actual
//! prune unit, not a file-level proxy.

use std::ops::Range;
use std::path::{Path, PathBuf};

use vortex::array::ArrayRef;
use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::arrays::struct_::{Struct, StructArrayExt};
use vortex::array::expr::{
    and, col, gt, gt_eq, is_not_null, is_null, lit, lt, lt_eq, BoundExpression, Expression,
};
use vortex::array::stream::ArrayStreamExt;
use vortex::array::VortexSessionExecute;
use vortex::buffer::Buffer;
use vortex::dtype::FieldPath;
use vortex::error::VortexResult;
use vortex::expr::stats::Stat;
use vortex::file::VortexFile;
use vortex::io::runtime::BlockingRuntime;
use vortex::io::runtime::single::SingleThreadRuntime;
use vortex::layout::LayoutReader;
use vortex::mask::Mask;
use vortex::scan::strict_sorted_buffer::StrictSortedBuffer;

use crate::bench::{measure, Measured};
use crate::store::{self, Ty};

/// The Person properties O16 needs — a low- and a high-cardinality column.
const PERSON_COLS: &[(&str, Ty)] = &[("id", Ty::Int), ("gender", Ty::Str), ("lastName", Ty::Str)];

pub fn run(out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();

    let n = 150_000usize;
    let table = store::synthetic("flat", n, 4);
    let path = out_dir.join("filter-flat.vortex");
    let (written, per_column) = store::write(&path, &table)?;
    println!(
        "filter store: {} rows, {} B raw -> {} B vortex, file {} B",
        table.rows(),
        table.raw_bytes(),
        per_column.iter().sum::<u64>(),
        written
    );

    let (session, rt, vfile) = store::open_mmap(&path)?;
    let splits = vfile.splits()?;
    print_splits("filter", &splits);

    let attr0 = ints(&table, "attr0");
    let attr1 = ints(&table, "attr1");

    // Scan budgets: the box is shared and a scan is milliseconds, so cap hard.
    let scan_iters = iters.clamp(5, 40);
    let cheap_iters = iters.clamp(1, 2000);

    let p_attr0 = bind(&vfile, col("attr0"))?;
    let p_id = bind(&vfile, col("id"))?;

    // ---- O4a: range filter -> count ----
    let f_attr0_lt500 = bind(&vfile, lt(col("attr0"), lit(500i64)))?;
    let expect_lt500 = attr0.iter().filter(|&&v| v < 500).count();
    let got = run_scan(&vfile, &rt, Some(&f_attr0_lt500), Some(&p_attr0), None, None)?.len();
    assert_eq!(got, expect_lt500, "O4a count disagrees with in-memory truth");
    println!(
        "  O4a attr0<500 survives {got} of {} rows (in-memory agrees)",
        vfile.row_count()
    );
    let m = measure("O4a range scan+count attr0<500", scan_iters, |_| {
        let len = run_scan(&vfile, &rt, Some(&f_attr0_lt500), Some(&p_attr0), None, None)
            .unwrap()
            .len();
        std::hint::black_box(len);
    });
    m.report();
    results.push(m);

    // ---- O4b: stats-only short-circuit (file-level) ----
    let e_attr0_gt100k = gt(col("attr0"), lit(100_000i64));
    let file_max = attr0.iter().copied().max().unwrap_or(i64::MIN);
    let prunes = vfile.can_prune(&e_attr0_gt100k)?;
    println!(
        "  O4b can_prune(attr0 > 100000) = {prunes} (file attr0 max = {file_max})"
    );
    let m = measure("O4b can_prune attr0>100000", cheap_iters, |_| {
        let p = vfile.can_prune(&e_attr0_gt100k).unwrap();
        std::hint::black_box(p);
    });
    m.report();
    results.push(m);

    // A matching range still cannot be pruned at file level.
    let e_attr0_lt500 = lt(col("attr0"), lit(500i64));
    println!(
        "  O4b can_prune(attr0 < 500) = {} (a matching range, expected false)",
        vfile.can_prune(&e_attr0_lt500)?
    );

    // The whole-file prune through a real scan: nothing should be decoded.
    let f_attr0_gt100k = bind(&vfile, e_attr0_gt100k.clone())?;
    let n_pruned = run_scan(&vfile, &rt, Some(&f_attr0_gt100k), Some(&p_attr0), None, None)?.len();
    assert_eq!(n_pruned, 0);
    let m = measure("O4b scan attr0>100000 (file prunes)", scan_iters, |_| {
        let len = run_scan(&vfile, &rt, Some(&f_attr0_gt100k), Some(&p_attr0), None, None)
            .unwrap()
            .len();
        std::hint::black_box(len);
        assert_eq!(len, 0);
    });
    m.report();
    results.push(m);

    // ---- O4c: exists-style early exit ----
    let one = run_scan(&vfile, &rt, Some(&f_attr0_lt500), Some(&p_attr0), None, Some(1))?.len();
    assert_eq!(one, 1);
    let m = measure("O4c exists with_limit(1) attr0<500", scan_iters, |_| {
        let len = run_scan(&vfile, &rt, Some(&f_attr0_lt500), Some(&p_attr0), None, Some(1))
            .unwrap()
            .len();
        std::hint::black_box(len);
    });
    m.report();
    results.push(m);

    // The same exists probe with one worker: does the limit stop after the
    // first split, or does split-parallelism defeat it?
    let m = measure("O4c exists limit(1) concurrency=1", scan_iters, |_| {
        let arr = rt
            .block_on(async {
                vfile
                    .scan()?
                    .with_filter(f_attr0_lt500.clone())
                    .with_projection(p_attr0.clone())
                    .with_concurrency(1)
                    .with_limit(1)
                    .into_array_stream()?
                    .read_all()
                    .await
            })
            .unwrap();
        std::hint::black_box(arr.len());
    });
    m.report();
    results.push(m);

    // ---- O4d: per-split zone-map pruning (the real prune unit) ----
    let reader = vfile.layout_reader()?;
    let pruned_no_match = pruned_splits(reader.as_ref(), &rt, &splits, &f_attr0_gt100k)?;
    let pruned_lt500 = pruned_splits(reader.as_ref(), &rt, &splits, &f_attr0_lt500)?;
    println!(
        "  O4d splits pruned: attr0>100000 {pruned_no_match}/{}, attr0<500 {pruned_lt500}/{} \
         (attr0 zone = (i*3)%1000: every 8k-row zone spans 0..999, so min/max cannot prune the range)",
        splits.len(),
        splits.len()
    );
    let m = measure("O4d stats-only prune pass", scan_iters, |_| {
        let p = pruned_splits(reader.as_ref(), &rt, &splits, &f_attr0_gt100k).unwrap();
        std::hint::black_box(p);
    });
    m.report();
    results.push(m);

    // ---- O4e: a range on the monotone `id` column, where zone maps are informative ----
    let f_id_lt75k = bind(&vfile, lt(col("id"), lit(75_000i64)))?;
    let got_id = run_scan(&vfile, &rt, Some(&f_id_lt75k), Some(&p_id), None, None)?.len();
    assert_eq!(got_id, 75_000);
    let pruned_id = pruned_splits(reader.as_ref(), &rt, &splits, &f_id_lt75k)?;
    println!(
        "  O4e id<75000 survives {got_id}, splits pruned {pruned_id}/{}",
        splits.len()
    );
    let m = measure("O4e range scan+count id<75000", scan_iters, |_| {
        let len = run_scan(&vfile, &rt, Some(&f_id_lt75k), Some(&p_id), None, None)
            .unwrap()
            .len();
        std::hint::black_box(len);
    });
    m.report();
    results.push(m);

    // ---- O5: conjunctive filter vs sequential re-filter ----
    let f_attr1_gt100 = bind(&vfile, gt(col("attr1"), lit(100i64)))?;
    let f_conj = bind(
        &vfile,
        and(lt(col("attr0"), lit(500i64)), gt(col("attr1"), lit(100i64))),
    )?;
    let expect_conj = attr0
        .iter()
        .zip(attr1.iter())
        .filter(|(a, b)| **a < 500 && **b > 100)
        .count();

    let conj_n = run_scan(&vfile, &rt, Some(&f_conj), Some(&p_attr0), None, None)?.len();
    assert_eq!(conj_n, expect_conj, "O5 conjunction count wrong");
    println!("  O5a conjunction survives {conj_n} of {}", vfile.row_count());
    let m = measure("O5a conjunction scan+count", scan_iters, |_| {
        let len = run_scan(&vfile, &rt, Some(&f_conj), Some(&p_attr0), None, None)
            .unwrap()
            .len();
        std::hint::black_box(len);
    });
    m.report();
    results.push(m);

    let p_a = pruned_splits(reader.as_ref(), &rt, &splits, &f_attr0_lt500)?;
    let p_b = pruned_splits(reader.as_ref(), &rt, &splits, &f_attr1_gt100)?;
    let p_c = pruned_splits(reader.as_ref(), &rt, &splits, &f_conj)?;
    println!(
        "  O5 splits pruned: attr0<500 {p_a}, attr1>100 {p_b}, conjunction {p_c} of {} \
         (a zone survives iff BOTH conjuncts keep it)",
        splits.len()
    );

    // Sequential: scan attr0 only, take the survivor ids, then gather exactly
    // those rows from attr1 and re-filter. Counts must agree with O5a.
    let mut ctx = session.create_execution_ctx();
    let seq_check = {
        let ids = run_scan(&vfile, &rt, Some(&f_attr0_lt500), Some(&p_id), None, None)?;
        let ids = ids.execute::<PrimitiveArray>(&mut ctx)?.as_slice::<i64>().to_vec();
        assert!(
            ids.windows(2).all(|w| w[0] < w[1]),
            "survivor ids must be strictly ascending for with_row_indices"
        );
        let sel = index_buffer(&ids);
        run_scan(&vfile, &rt, Some(&f_attr1_gt100), Some(&p_id), Some(&sel), None)?.len()
    };
    assert_eq!(seq_check, expect_conj, "O5 sequential disagree with conjunction");
    let m = measure("O5b sequential: 2 scans (f0 then f1)", scan_iters, |_| {
        let ids = run_scan(&vfile, &rt, Some(&f_attr0_lt500), Some(&p_id), None, None)
            .unwrap();
        let ids = ids
            .execute::<PrimitiveArray>(&mut ctx)
            .unwrap()
            .as_slice::<i64>()
            .to_vec();
        let sel = index_buffer(&ids);
        let len = run_scan(&vfile, &rt, Some(&f_attr1_gt100), Some(&p_id), Some(&sel), None)
            .unwrap()
            .len();
        std::hint::black_box(len);
    });
    m.report();
    results.push(m);

    // A conjunction where one conjunct's column has informative zone maps:
    // the conjunction's pruning is the union of the two conjuncts' pruning.
    let f_conj_id = bind(
        &vfile,
        and(lt(col("id"), lit(75_000i64)), gt(col("attr1"), lit(100i64))),
    )?;
    let conj_id_pruned = pruned_splits(reader.as_ref(), &rt, &splits, &f_conj_id)?;
    let conj_id_n = run_scan(&vfile, &rt, Some(&f_conj_id), Some(&p_id), None, None)?.len();
    println!(
        "  O5c id<75000 AND attr1>100: survives {conj_id_n}, splits pruned {conj_id_pruned}/{}",
        splits.len()
    );
    let m = measure("O5c conjunction id<75000 AND attr1>100", scan_iters, |_| {
        let len = run_scan(&vfile, &rt, Some(&f_conj_id), Some(&p_id), None, None)
            .unwrap()
            .len();
        std::hint::black_box(len);
    });
    m.report();
    results.push(m);

    // ---- O12: STARTS WITH as a string range predicate ----
    // "flat" keys are `node-{i:08}`, so sort order == id order and the prefix
    // "node-0001" is exactly ids 10000..=19999.
    let lower = "node-0001".to_string();
    let upper = format!("node-0001\u{10FFFF}");
    let f_prefix = bind(
        &vfile,
        and(
            gt_eq(col("key"), lit(lower.clone())),
            lt_eq(col("key"), lit(upper.clone())),
        ),
    )?;
    let p_key = bind(&vfile, col("key"))?;
    let prefix_hits = run_scan(&vfile, &rt, Some(&f_prefix), Some(&p_key), None, None)?.len();
    assert_eq!(prefix_hits, 10_000, "prefix range should match ids 10000..19999");
    let pruned_prefix = pruned_splits(reader.as_ref(), &rt, &splits, &f_prefix)?;
    println!(
        "  O12a prefix \"node-0001\" matches {prefix_hits} rows, splits pruned {pruned_prefix}/{} \
         (a string range predicate IS expressible: gt_eq/lt_eq on Utf8)",
        splits.len()
    );
    let m = measure("O12a prefix range scan+count", scan_iters, |_| {
        let len = run_scan(&vfile, &rt, Some(&f_prefix), Some(&p_key), None, None)
            .unwrap()
            .len();
        std::hint::black_box(len);
    });
    m.report();
    results.push(m);

    // Full scan + compare baseline: decode the key column once, then compare
    // every value in memory (the no-pushdown equivalent).
    let keys: Vec<String> = match table.column("key") {
        Some(store::Col::Str(v)) => v.clone(),
        _ => return Err("key is not a string column".into()),
    };
    let m = measure("O12b key column decode (read_all)", 1, |_| {
        let a = run_scan(&vfile, &rt, None, Some(&p_key), None, None).unwrap();
        std::hint::black_box(a);
    });
    m.report();
    results.push(m);

    let m = measure("O12b full scan+starts_with (memory)", cheap_iters, |_| {
        let c = keys.iter().filter(|k| k.starts_with("node-0001")).count();
        std::hint::black_box(c);
        assert_eq!(c, 10_000);
    });
    m.report();
    results.push(m);

    // Binary search for the prefix bounds over the already-sorted key column:
    // log2(n) probes instead of n comparisons (what a sorted index serves).
    let m = measure("O12c prefix bounds binary search", cheap_iters, |_| {
        let lo = keys.partition_point(|k| k.as_str() < lower.as_str());
        let hi = keys.partition_point(|k| k.as_str() <= upper.as_str());
        std::hint::black_box(hi - lo);
        assert_eq!(hi - lo, 10_000);
    });
    m.report();
    results.push(m);

    // ---- O16: presence / IS NULL ----
    run_o16(out_dir, cheap_iters, &mut results)?;

    Ok(results)
}

/// O16 on the Person CSV: is a null-presence decision served by a stat, and
/// does a non-nullable column make IS NULL prune the whole file?
fn run_o16(
    out_dir: &Path,
    cheap_iters: u64,
    results: &mut Vec<Measured>,
) -> Result<(), Box<dyn std::error::Error>> {
    let csv = std::env::var("VORTEX_LAB_CSV")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(
                "target/query-latency/data/social_network-sf0.1-CsvBasic-StringDateFormatter/dynamic/person_0_0.csv",
            )
        });
    if !csv.exists() {
        println!("O16 skipped: no Person CSV at {} (set VORTEX_LAB_CSV)", csv.display());
        return Ok(());
    }

    let table = store::load_csv(&csv, PERSON_COLS)?;
    let path = out_dir.join("person-filter.vortex");
    let (written, _) = store::write(&path, &table)?;
    println!(
        "O16 store: {} rows from {} -> {} B vortex",
        table.rows(),
        csv.file_name().and_then(|s| s.to_str()).unwrap_or("person"),
        written
    );

    let (session, rt, vfile) = store::open_mmap(&path)?;

    // What the footer records per column — the store-level presence decision.
    for name in ["gender", "lastName"] {
        let path = FieldPath::from_name(name);
        match vfile.file_stats().and_then(|fs| fs.get_by_path(&path)) {
            Some((agg, dtype)) => {
                let set = agg.to_stats_set();
                println!(
                    "  O16 footer {name:<9} dtype={dtype:<28} NullCount={:?} Min={:?} Max={:?} IsSorted={:?}",
                    set.get(Stat::NullCount),
                    set.get(Stat::Min),
                    set.get(Stat::Max),
                    set.get(Stat::IsSorted),
                );
            }
            None => println!("  O16 footer {name}: no stats entry"),
        }
    }

    // The decoded columns: is NullCount present (cached) or computed on demand?
    let root = run_scan(&vfile, &rt, None, None, None, None)?;
    let view = root.as_opt::<Struct>().ok_or("person root is not a struct")?;
    let gender = view.unmasked_field_by_name("gender")?.clone();
    let last = view.unmasked_field_by_name("lastName")?.clone();
    let mut ctx = session.create_execution_ctx();
    for (name, arr) in [("gender", &gender), ("lastName", &last)] {
        let cached = arr.statistics().to_owned().get(Stat::NullCount);
        println!(
            "  O16 decoded {name:<9} NullCount stat={cached:?} (absent = computed on demand, \
             NonNullable column)"
        );
    }

    // Cost of the presence decision when it is computed over a decoded column.
    for (label, arr) in [("O16 NullCount compute gender", &gender), ("O16 NullCount compute lastName", &last)] {
        let m = measure(label, cheap_iters, |_| {
            let n = arr.statistics().compute_null_count(&mut ctx);
            std::hint::black_box(n);
        });
        m.report();
        results.push(m);
    }

    // Footer lookup: the same decision from the stored aggregate, no decode.
    let m = measure("O16 footer NullCount lookup gender", cheap_iters, |_| {
        let v = vfile
            .file_stats()
            .and_then(|fs| fs.get_by_path(&FieldPath::from_name("gender")))
            .map(|(agg, _)| agg.to_stats_set().get(Stat::NullCount));
        std::hint::black_box(v);
    });
    m.report();
    results.push(m);

    // IS NULL on a non-nullable column: nothing can be null, so the whole file
    // prunes; IS NOT NULL prunes nothing.
    println!(
        "  O16 can_prune(IS NULL gender) = {}, can_prune(IS NOT NULL gender) = {}",
        vfile.can_prune(&is_null(col("gender")))?,
        vfile.can_prune(&is_not_null(col("gender")))?,
    );

    let p_gender = bind(&vfile, col("gender"))?;
    let f_isnull = bind(&vfile, is_null(col("gender")))?;
    let n_null = run_scan(&vfile, &rt, Some(&f_isnull), Some(&p_gender), None, None)?.len();
    assert_eq!(n_null, 0, "a NonNullable column must have no nulls");
    let m = measure("O16 scan IS NULL gender (prunes all)", cheap_iters.max(5), |_| {
        let len = run_scan(&vfile, &rt, Some(&f_isnull), Some(&p_gender), None, None)
            .unwrap()
            .len();
        std::hint::black_box(len);
        assert_eq!(len, 0);
    });
    m.report();
    results.push(m);

    Ok(())
}

// ---- helpers ----

fn bind(vfile: &VortexFile, expr: Expression) -> VortexResult<BoundExpression> {
    expr.bind(vfile.dtype())?.optimize_recursive()
}

#[allow(clippy::too_many_arguments)]
fn run_scan(
    vfile: &VortexFile,
    rt: &SingleThreadRuntime,
    filter: Option<&BoundExpression>,
    projection: Option<&BoundExpression>,
    rows: Option<&StrictSortedBuffer<u64>>,
    limit: Option<u64>,
) -> VortexResult<ArrayRef> {
    let mut b = vfile.scan()?;
    if let Some(f) = filter {
        b = b.with_filter(f.clone());
    }
    if let Some(p) = projection {
        b = b.with_projection(p.clone());
    }
    if let Some(r) = rows {
        b = b.with_row_indices(r.clone());
    }
    if let Some(l) = limit {
        b = b.with_limit(l);
    }
    rt.block_on(async { b.into_array_stream()?.read_all().await })
}

/// How many natural splits `filter` proves empty — a stats-only pass, no
/// payload read. The split count is the store's real prune granularity.
fn pruned_splits(
    reader: &dyn LayoutReader,
    rt: &SingleThreadRuntime,
    splits: &[Range<u64>],
    filter: &BoundExpression,
) -> VortexResult<usize> {
    let mut pruned = 0usize;
    for s in splits {
        let len = usize::try_from(s.end - s.start)?;
        let mask_future = reader.pruning_evaluation(s, filter, Mask::new_true(len))?;
        let mask = rt.block_on(mask_future)?;
        if mask.all_false() {
            pruned += 1;
        }
    }
    Ok(pruned)
}

fn index_buffer(ids: &[i64]) -> StrictSortedBuffer<u64> {
    let rows: Vec<u64> = ids.iter().map(|&i| i as u64).collect();
    StrictSortedBuffer::try_new(Buffer::from(rows)).expect("survivor ids are strictly ascending")
}

fn print_splits(label: &str, splits: &[Range<u64>]) {
    let sizes: Vec<u64> = splits.iter().map(|r| r.end - r.start).collect();
    let total: u64 = sizes.iter().sum();
    let min = sizes.iter().copied().min().unwrap_or(0);
    let max = sizes.iter().copied().max().unwrap_or(0);
    let ranges: Vec<String> = splits.iter().map(|r| format!("{}..{}", r.start, r.end)).collect();
    println!(
        "{label} splits: {} ranges covering {} rows, {min}..{max} rows each (mean {:.0}) — the zone-map prune unit",
        splits.len(),
        total,
        total as f64 / splits.len().max(1) as f64
    );
    println!("  ranges: [{}]", ranges.join(", "));
}

/// The in-memory truth for a synthetic i64 column.
fn ints(table: &store::Table, name: &str) -> Vec<i64> {
    match table.column(name) {
        Some(store::Col::Int(v)) => v.clone(),
        _ => panic!("column {name} is not an i64 column"),
    }
}
