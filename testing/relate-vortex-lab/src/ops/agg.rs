//! Aggregate kernels: O13 countDistinct, O14 histogram/quantiles, O15 order/top-k,
//! O6 group-by+count, O20 distinct over column pairs.
//!
//! The point of these kernels is which aggregate paths vortex 0.93 actually gives
//! you, so each one is measured against a decode-and-do-it-yourself baseline:
//! - what the compressor chose for the column (read from the layout tree and
//!   from the decoded chunk encodings),
//! - whether a dictionary-encoded column's distinct set / groups / code pairs are
//!   reachable without decoding values,
//! - whether per-zone Min/Max in the layout serves a histogram without reading
//!   the column, and what it costs to read the zone table,
//! - whether `IsSorted` exists in the file or only appears after a decode.
//!
//! Column census for the 150k-row synthetic "flat" table (attrN = i*(N+3) % 1000):
//! `attr1` (250 distinct) and `attr2` (200) compress to `vortex.dict`, `attr0`
//! (1000) and `attr3` (500) to `fastlanes.bitpacked.v2`; `id` is a `vortex.sequence`
//! (in-memory stat IsSorted=true), `key` is `vortex.onpair`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use vortex::array::arrays::chunked::{Chunked, ChunkedArrayExt};
use vortex::array::arrays::dict::{Dict, DictArrayExt, DictArraySlotsExt};
use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::arrays::struct_::{Struct, StructArrayExt};
use vortex::array::stream::ArrayStreamExt;
use vortex::array::{ArrayRef, VortexSessionExecute};
use vortex::expr::stats::{Stat, StatsProvider};
use vortex::expr::{get_item, root, select, BoundExpression, Expression};
use vortex::file::VortexFile;
use vortex::io::runtime::single::SingleThreadRuntime;
use vortex::io::runtime::BlockingRuntime;
use vortex::layout::LayoutReaderContext;
use vortex::session::VortexSession;

use crate::bench::{measure, Measured};
use crate::store;

const ROWS: usize = 150_000;
const ATTRS: usize = 4;
const BINS: usize = 10;

type Res = Box<dyn std::error::Error>;

// ---- shared plumbing ------------------------------------------------------

fn bind(vfile: &VortexFile, expr: Expression) -> Result<BoundExpression, Res> {
    Ok(expr.bind(vfile.dtype())?.optimize_recursive()?)
}

fn scan(
    rt: &SingleThreadRuntime,
    vfile: &VortexFile,
    projection: Option<BoundExpression>,
    limit: Option<u64>,
) -> Result<ArrayRef, Res> {
    Ok(rt.block_on(async {
        let mut b = vfile.scan()?;
        if let Some(p) = projection {
            b = b.with_projection(p);
        }
        if let Some(l) = limit {
            b = b.with_limit(l);
        }
        b.into_array_stream()?.read_all().await
    })?)
}

/// The scan result as per-chunk arrays (a chunked root is unwrapped).
fn chunks(arr: &ArrayRef) -> Vec<ArrayRef> {
    if arr.is::<Chunked>() {
        let c = arr.as_::<Chunked>();
        (0..c.nchunks()).map(|i| c.chunk(i).clone()).collect()
    } else {
        vec![arr.clone()]
    }
}

/// Decode a whole column (through the projected scan) into an `i64` vector.
fn decode_i64(session: &VortexSession, col: &ArrayRef) -> Result<Vec<i64>, Res> {
    let mut ctx = session.create_execution_ctx();
    let canonical = col.clone().execute::<PrimitiveArray>(&mut ctx)?;
    Ok(canonical.as_slice::<i64>().to_vec())
}

/// A per-chunk dictionary column: its shared values and the concatenated codes.
/// Returns `None` when any chunk of the column is not a `vortex.dict` array or
/// the chunks do not share one values array (then the codes are per-chunk and
/// not comparable across chunks).
fn dict_codes(
    session: &VortexSession,
    column: &ArrayRef,
) -> Result<Option<(ArrayRef, Vec<u16>)>, Res> {
    let mut ctx = session.create_execution_ctx();
    let mut values: Option<ArrayRef> = None;
    let mut codes: Vec<u16> = Vec::new();
    for chunk in chunks(column) {
        let Some(d) = chunk.as_opt::<Dict>() else {
            return Ok(None);
        };
        let v = d.values().clone();
        match &values {
            None => values = Some(v),
            Some(prev) => {
                if !ArrayRef::ptr_eq(prev, &v) {
                    return Ok(None); // per-chunk dictionaries: codes not global
                }
            }
        }
        let c = d.codes().clone().execute::<PrimitiveArray>(&mut ctx)?;
        codes.extend_from_slice(c.as_slice::<u16>());
    }
    Ok(values.map(|v| (v, codes)))
}

/// The dict values row count of a column layout, from the layout tree alone:
/// `zoned -> dict -> values`. `None` when the column was not written as a dict.
fn layout_dict_size(layout: &vortex::layout::LayoutRef) -> Option<u64> {
    let data = if layout.dyn_encoding_id().as_ref() == "vortex.zoned" {
        layout.dyn_slot(0).ok()??
    } else {
        layout.dyn_to_layout()
    };
    if data.dyn_encoding_id().as_ref() != "vortex.dict" {
        return None;
    }
    Some(data.dyn_slot(0).ok()??.dyn_row_count())
}

fn find_column_layout(
    root_layout: &vortex::layout::LayoutRef,
    field: &str,
) -> Result<Option<vortex::layout::LayoutRef>, Res> {
    for i in 0..root_layout.dyn_nslots() {
        if let Some(l) = root_layout.dyn_slot(i)? {
            if let Some(vortex::layout::LayoutChildType::Field(name)) = root_layout.dyn_slot_type(i)
            {
                if name.as_ref() == field {
                    return Ok(Some(l));
                }
            }
        }
    }
    Ok(None)
}

/// Read one column's zone table (Min/Max/NullCount per zone) straight from its
/// layout's stats child — no value decode.
fn zone_table(
    session: &VortexSession,
    rt: &SingleThreadRuntime,
    vfile: &VortexFile,
    col_layout: &vortex::layout::LayoutRef,
) -> Result<Option<(Vec<i64>, Vec<i64>)>, Res> {
    if col_layout.dyn_encoding_id().as_ref() != "vortex.zoned" {
        return Ok(None);
    }
    let Some(zones) = col_layout.dyn_slot(1)? else {
        return Ok(None);
    };
    if zones.dyn_row_count() == 0 {
        return Ok(None);
    }
    let reader = zones.dyn_new_reader(
        std::sync::Arc::from("zones"),
        vfile.segment_source(),
        session,
        &LayoutReaderContext::new(),
    )?;
    let n = reader.row_count();
    let expr = BoundExpression::new_root(reader.dtype().clone());
    let arr = rt.block_on(reader.projection_evaluation(
        &(0..n),
        &expr,
        vortex::array::MaskFuture::new_true(n as usize),
    )?)?;
    let mut ctx = session.create_execution_ctx();
    let sarr = arr.execute::<vortex::array::arrays::struct_::StructArray>(&mut ctx)?;
    let mins = sarr
        .unmasked_field_by_name("vortex.min()")?
        .clone()
        .execute::<PrimitiveArray>(&mut ctx)?;
    let maxes = sarr
        .unmasked_field_by_name("vortex.max()")?
        .clone()
        .execute::<PrimitiveArray>(&mut ctx)?;
    Ok(Some((
        mins.as_slice::<i64>().to_vec(),
        maxes.as_slice::<i64>().to_vec(),
    )))
}

// ---- kernels --------------------------------------------------------------

pub fn run(out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Res> {
    // Smoke bounds: this box is shared and loaded; never run a campaign.
    let it = iters.clamp(1, 2000);
    let heavy = it.min(40);
    let mut results = Vec::new();

    let table = store::synthetic("flat", ROWS, ATTRS);
    let path = out_dir.join("agg.vortex");
    let (written, per_column) = store::write(&path, &table)?;
    println!(
        "size: {} rows, {} B raw -> {} B vortex, file {} B",
        table.rows(),
        table.raw_bytes(),
        per_column.iter().sum::<u64>(),
        written
    );

    let (session, rt, vfile) = store::open_mmap(&path)?;
    let root_layout = vfile.scan_file().layout;
    let root_arr = scan(&rt, &vfile, None, None)?;

    // ---- census: what the compressor produced, and where the stats live ----
    println!("column census (in-memory chunk encoding | layout | file stats):");
    let struct_chunk = chunks(&root_arr)
        .into_iter()
        .find(|c| c.is::<Struct>())
        .ok_or("no struct chunk")?;
    let struct_view = struct_chunk
        .as_opt::<Struct>()
        .ok_or("chunk is not a struct")?;
    for (i, name) in ["id", "key", "attr0", "attr1", "attr2", "attr3"]
        .iter()
        .enumerate()
    {
        let col = struct_view.unmasked_field_by_name(name)?.clone();
        let col_layout = find_column_layout(&root_layout, name)?;
        let layout_id = col_layout
            .as_ref()
            .map(|l| {
                let mut chain = vec![l.dyn_encoding_id().to_string()];
                if let Ok(Some(d)) = l.dyn_slot(0) {
                    chain.push(d.dyn_encoding_id().to_string());
                }
                chain.join(">")
            })
            .unwrap_or_default();
        let file_stat = vfile
            .file_stats()
            .and_then(|fs| {
                fs.paths()
                    .iter()
                    .position(|p| format!("{p:?}").contains(name))
            })
            .map(|idx| {
                let (agg, _) = vfile.file_stats().unwrap().get(idx);
                let s = agg.to_stats_set();
                format!("min={:?} max={:?}", s.get(Stat::Min), s.get(Stat::Max))
            })
            .unwrap_or_else(|| "none".into());
        println!(
            "  [{i}] {name:<6} chunk={:<24} layout={layout_id:<28} file_stats: {file_stat} | issorted(decoded)={:?}",
            col.encoding_id().to_string(),
            col.statistics().get(Stat::IsSorted)
        );
    }
    println!(
        "  (per-column sizes: {:?}; zone maps are per 8192 rows — 19 zones for {ROWS})",
        per_column
    );

    // ---- O13 countDistinct -------------------------------------------------
    let attr1_expr = bind(&vfile, get_item("attr1", root()))?;
    let attr0_expr = bind(&vfile, get_item("attr0", root()))?;

    let m = measure(
        "O13a countDistinct: decode + HashSet (attr1, 250)",
        heavy,
        |_| {
            let col = scan(&rt, &vfile, Some(attr1_expr.clone()), None).unwrap();
            let vals = decode_i64(&session, &col).unwrap();
            let set: HashSet<i64> = vals.into_iter().collect();
            std::hint::black_box(set.len());
        },
    );
    m.report();
    results.push(m);

    // attribution: how much of O13a is decode, how much the hash set.
    let m = measure("O13a' decode only (attr1 -> PrimitiveArray)", heavy, |_| {
        let col = scan(&rt, &vfile, Some(attr1_expr.clone()), None).unwrap();
        let vals = decode_i64(&session, &col).unwrap();
        std::hint::black_box(vals.len());
    });
    m.report();
    results.push(m);

    let attr1_vals = decode_i64(
        &session,
        &scan(&rt, &vfile, Some(attr1_expr.clone()), None)?,
    )?;
    let m = measure(
        "O13a'' HashSet over pre-decoded values (attr1)",
        heavy,
        |_| {
            let set: HashSet<i64> = attr1_vals.iter().copied().collect();
            std::hint::black_box(set.len());
        },
    );
    m.report();
    results.push(m);

    // (b) encoded form, footer only: a `vortex.dict` column layout carries one
    // dictionary for the whole column; its size is the distinct count.
    let m = measure("O13b countDistinct: dict layout size (footer)", it, |_| {
        let l = find_column_layout(&root_layout, "attr1").unwrap().unwrap();
        let n = layout_dict_size(&l);
        std::hint::black_box(n);
    });
    m.report();
    results.push(m);
    println!(
        "  layout dict size for attr1 = {:?} (values row count; exact iff all values referenced — the layout-level flag is not reachable in 0.93)",
        find_column_layout(&root_layout, "attr1")?.as_ref().and_then(layout_dict_size)
    );

    {
        let col = scan(&rt, &vfile, Some(attr1_expr.clone()), None)?;
        let first = chunks(&col).into_iter().next().ok_or("empty attr1 scan")?;
        if let Some(d) = first.as_opt::<Dict>() {
            println!(
                "  in-memory flag: attr1 chunk0 dict has_all_values_referenced={} (public on DictArray, not on the layout)",
                d.has_all_values_referenced()
            );
        }
    }

    // (b2) what the scan actually hands you: union the per-chunk dict values.
    let m = measure(
        "O13b2 countDistinct: union chunk dict values (attr1)",
        heavy,
        |_| {
            let col = scan(&rt, &vfile, Some(attr1_expr.clone()), None).unwrap();
            let mut set: HashSet<i64> = HashSet::new();
            for chunk in chunks(&col) {
                if let Some(d) = chunk.as_opt::<Dict>() {
                    let mut ctx = session.create_execution_ctx();
                    let v = d
                        .values()
                        .clone()
                        .execute::<PrimitiveArray>(&mut ctx)
                        .unwrap();
                    set.extend(v.as_slice::<i64>().iter().copied());
                }
            }
            std::hint::black_box(set.len());
        },
    );
    m.report();
    results.push(m);

    // (c) the column the compressor did NOT dictionary-encode: no cheap path.
    let m = measure(
        "O13c countDistinct: decode + HashSet (attr0, 1000)",
        heavy,
        |_| {
            let col = scan(&rt, &vfile, Some(attr0_expr.clone()), None).unwrap();
            let vals = decode_i64(&session, &col).unwrap();
            let set: HashSet<i64> = vals.into_iter().collect();
            std::hint::black_box(set.len());
        },
    );
    m.report();
    results.push(m);
    println!(
        "  attr0 in-memory encoding = {} — no dict, so no encoded distinct set (finding)",
        scan(&rt, &vfile, Some(attr0_expr.clone()), None)?
            .as_::<Chunked>()
            .chunk(0)
            .encoding_id()
    );

    // self-check: the footer's dict size is the true distinct count for attr1,
    // and attr0 (no dict) really has 1000 distinct values.
    {
        let vals = decode_i64(
            &session,
            &scan(&rt, &vfile, Some(attr1_expr.clone()), None)?,
        )?;
        let decoded: HashSet<i64> = vals.into_iter().collect();
        let layout_n = find_column_layout(&root_layout, "attr1")?
            .as_ref()
            .and_then(layout_dict_size);
        assert_eq!(
            layout_n,
            Some(decoded.len() as u64),
            "layout dict size must equal the decoded distinct count"
        );
        let distinct0 = decode_i64(
            &session,
            &scan(&rt, &vfile, Some(attr0_expr.clone()), None)?,
        )?
        .into_iter()
        .collect::<HashSet<i64>>()
        .len();
        println!(
            "  O13 self-check: attr1 layout dict size == decoded distinct ({}); attr0 decoded distinct = {distinct0}",
            decoded.len()
        );
    }

    // ---- O14 histograms / quantiles ---------------------------------------
    // (a) frequency histogram: per-code counts from the dict codes (values never
    // decoded), against a decode+HashMap histogram.
    let m = measure("O14a histogram: dict codes (attr1)", heavy, |_| {
        let col = scan(&rt, &vfile, Some(attr1_expr.clone()), None).unwrap();
        let (values, codes) = dict_codes(&session, &col).unwrap().unwrap();
        let mut counts = vec![0u64; values.len()];
        for &c in &codes {
            counts[c as usize] += 1;
        }
        std::hint::black_box(counts.len());
    });
    m.report();
    results.push(m);

    let m = measure("O14a histogram: decode + HashMap (attr1)", heavy, |_| {
        let col = scan(&rt, &vfile, Some(attr1_expr.clone()), None).unwrap();
        let vals = decode_i64(&session, &col).unwrap();
        let mut counts: HashMap<i64, u64> = HashMap::new();
        for v in vals {
            *counts.entry(v).or_insert(0) += 1;
        }
        std::hint::black_box(counts.len());
    });
    m.report();
    results.push(m);

    // self-check: both histograms must agree.
    {
        let col = scan(&rt, &vfile, Some(attr1_expr.clone()), None)?;
        let (values, codes) = dict_codes(&session, &col)?.ok_or("attr1 not dict")?;
        let val_vec = decode_i64(&session, &values)?;
        let mut from_codes: HashMap<i64, u64> = HashMap::new();
        for &c in &codes {
            *from_codes.entry(val_vec[c as usize]).or_insert(0) += 1;
        }
        let mut from_decode: HashMap<i64, u64> = HashMap::new();
        for v in decode_i64(&session, &col)? {
            *from_decode.entry(v).or_insert(0) += 1;
        }
        assert_eq!(
            from_codes, from_decode,
            "dict-code histogram must equal decode"
        );
        println!(
            "  O14a self-check: dict-code histogram == decode+HashMap ({} groups)",
            from_decode.len()
        );
    }

    // (b) equi-width binning from per-zone Min/Max, no value decode.
    let attr0_layout = find_column_layout(&root_layout, "attr0")?.ok_or("no attr0 layout")?;
    let m = measure(
        "O14b zone-table read (19 Min/Max pairs, attr0)",
        it.min(200),
        |_| {
            let z = zone_table(&session, &rt, &vfile, &attr0_layout).unwrap();
            std::hint::black_box(z);
        },
    );
    m.report();
    results.push(m);
    {
        let (mins, maxes) =
            zone_table(&session, &rt, &vfile, &attr0_layout)?.ok_or("attr0 has no zone table")?;
        let lo = *mins.iter().min().unwrap();
        let hi = *maxes.iter().max().unwrap();
        let width = ((hi - lo + 1) as f64 / BINS as f64).max(1.0);
        // A zone resolves a bin exactly only when the whole zone falls in it.
        let mut resolved = 0usize;
        for (mn, mx) in mins.iter().zip(&maxes) {
            let b0 = ((mn - lo) as f64 / width) as usize;
            let b1 = ((mx - lo) as f64 / width) as usize;
            if b0 == b1 {
                resolved += 1;
            }
        }
        println!(
            "  O14b attr0 zones={} range={lo}..={hi}: {resolved}/{} zones land in a single bin of {BINS} — zone stats bound the histogram, they do not build it",
            mins.len(),
            mins.len()
        );
        // The exact histogram is only available after decoding the column.
        let vals = decode_i64(
            &session,
            &scan(&rt, &vfile, Some(attr0_expr.clone()), None)?,
        )?;
        let mut bins = vec![0u64; BINS];
        for v in vals {
            let b = (((v - lo) as f64) / width) as usize;
            bins[b.min(BINS - 1)] += 1;
        }
        println!("  O14b exact bins from decode: {bins:?}");
    }

    // (c) exact quantile: the sorted column is `id` (sequence, IsSorted=true).
    let id_expr = bind(&vfile, get_item("id", root()))?;
    let id_col = scan(&rt, &vfile, Some(id_expr.clone()), None)?;
    let mut ctx = session.create_execution_ctx();
    let m = measure("O14c quantile: position read on sorted col (id)", it, |i| {
        let idx = ((i.wrapping_mul(2_654_435_761)) % ROWS as u64) as usize;
        let v = id_col.execute_scalar(idx, &mut ctx).unwrap();
        std::hint::black_box(v);
    });
    m.report();
    results.push(m);

    let id_vals = decode_i64(&session, &id_col)?;
    let m = measure("O14c quantile: decode + full sort", heavy.min(20), |_| {
        let mut v = id_vals.clone();
        v.sort_unstable();
        let median = v[v.len() / 2];
        std::hint::black_box(median);
    });
    m.report();
    results.push(m);
    println!(
        "  O14c id median by index = {} (expected {})",
        id_vals[ROWS / 2],
        ROWS / 2
    );

    // ---- O15 order / top-k ------------------------------------------------
    let k = 100u64;
    let m = measure("O15a with_limit(100) scan (whole row)", heavy, |_| {
        let a = scan(&rt, &vfile, None, Some(k)).unwrap();
        std::hint::black_box(a.len());
    });
    m.report();
    results.push(m);
    let first = scan(&rt, &vfile, None, Some(k))?;
    {
        let first_chunk = chunks(&first)
            .into_iter()
            .next()
            .ok_or("empty limited scan")?;
        let first_struct = first_chunk
            .as_opt::<Struct>()
            .ok_or("limited-scan chunk is not a struct")?;
        let mut ctx = session.create_execution_ctx();
        let id0 = first_struct
            .unmasked_field_by_name("id")?
            .clone()
            .execute_scalar(0, &mut ctx)?;
        println!(
            "  O15a limited scan rows={} first id={:?} (expected 0)",
            first.len(),
            id0
        );
    }

    let m = measure("O15a full scan (whole row)", heavy, |_| {
        let a = scan(&rt, &vfile, None, None).unwrap();
        std::hint::black_box(a.len());
    });
    m.report();
    results.push(m);

    // (b) top-k by value over the decoded column: full sort vs partial selection.
    let attr0_vals = decode_i64(
        &session,
        &scan(&rt, &vfile, Some(attr0_expr.clone()), None)?,
    )?;
    let m = measure(
        "O15b top-10: full sort of decoded column",
        heavy.min(20),
        |_| {
            let mut v = attr0_vals.clone();
            v.sort_unstable();
            std::hint::black_box(&v[v.len() - 10..]);
        },
    );
    m.report();
    results.push(m);
    let m = measure(
        "O15b top-10: select_nth_unstable (partial)",
        heavy.min(20),
        |_| {
            let mut v = attr0_vals.clone();
            let cut = v.len() - 10;
            v.select_nth_unstable(cut);
            std::hint::black_box(&v[cut..]);
        },
    );
    m.report();
    results.push(m);

    // ---- O6 group-by + count ---------------------------------------------
    // (a) decode attr0 + HashMap.
    let m = measure("O6a group-count: decode + HashMap (attr0)", heavy, |_| {
        let col = scan(&rt, &vfile, Some(attr0_expr.clone()), None).unwrap();
        let vals = decode_i64(&session, &col).unwrap();
        let mut counts: HashMap<i64, u64> = HashMap::new();
        for v in vals {
            *counts.entry(v).or_insert(0) += 1;
        }
        std::hint::black_box(counts.len());
    });
    m.report();
    results.push(m);

    // (b) derive groups from the encoding's runs/codes. attr0 has none; attr1 does.
    println!(
        "  O6b attr0 encoding = {}: no runend/dict form, groups cannot be derived from it",
        scan(&rt, &vfile, Some(attr0_expr.clone()), None)?
            .as_::<Chunked>()
            .chunk(0)
            .encoding_id()
    );
    let m = measure(
        "O6b group-count: dict codes, no value decode (attr1)",
        heavy,
        |_| {
            let col = scan(&rt, &vfile, Some(attr1_expr.clone()), None).unwrap();
            let (values, codes) = dict_codes(&session, &col).unwrap().unwrap();
            let mut counts = vec![0u64; values.len()];
            for &c in &codes {
                counts[c as usize] += 1;
            }
            std::hint::black_box(counts.len());
        },
    );
    m.report();
    results.push(m);
    println!(
        "  O6b attr1 top group: {} rows (groups = dict size, not row count)",
        {
            let col = scan(&rt, &vfile, Some(attr1_expr.clone()), None)?;
            let (values, codes) = dict_codes(&session, &col)?.ok_or("attr1 not dict")?;
            let mut counts = vec![0u64; values.len()];
            for &c in &codes {
                counts[c as usize] += 1;
            }
            counts.iter().copied().max().unwrap_or(0)
        }
    );

    // ---- O20 distinct over (attr1, attr2) pairs ---------------------------
    let pair_expr = bind(&vfile, select(["attr1", "attr2"], root()))?;

    // (a) decode both columns, tuple HashSet over values.
    let m = measure("O20a distinct pairs: decode + tuple HashSet", heavy, |_| {
        let col = scan(&rt, &vfile, Some(pair_expr.clone()), None).unwrap();
        let mut set: HashSet<(i64, i64)> = HashSet::new();
        for chunk in chunks(&col) {
            let view = chunk.as_opt::<Struct>().unwrap();
            let a = decode_i64(&session, view.unmasked_field_by_name("attr1").unwrap()).unwrap();
            let b = decode_i64(&session, view.unmasked_field_by_name("attr2").unwrap()).unwrap();
            for (x, y) in a.into_iter().zip(b) {
                set.insert((x, y));
            }
        }
        std::hint::black_box(set.len());
    });
    m.report();
    results.push(m);

    // (b) the same over the columns' dictionary codes (no value decode).
    let m = measure("O20b distinct pairs: dict code pairs", heavy, |_| {
        let col = scan(&rt, &vfile, Some(pair_expr.clone()), None).unwrap();
        // packed code pair: one u32 key, never a decoded string
        let mut set: HashSet<u32> = HashSet::new();
        for chunk in chunks(&col) {
            let view = chunk.as_opt::<Struct>().unwrap();
            let ca = dict_codes(&session, view.unmasked_field_by_name("attr1").unwrap())
                .unwrap()
                .unwrap()
                .1;
            let cb = dict_codes(&session, view.unmasked_field_by_name("attr2").unwrap())
                .unwrap()
                .unwrap()
                .1;
            for (x, y) in ca.into_iter().zip(cb) {
                set.insert(((x as u32) << 16) | y as u32);
            }
        }
        std::hint::black_box(set.len());
    });
    m.report();
    results.push(m);

    // self-check: the code path exists iff both columns are dict-encoded with a
    // shared dictionary — and then it must agree with the decoded pair count.
    {
        let col = scan(&rt, &vfile, Some(pair_expr.clone()), None)?;
        let mut code_set: HashSet<(u16, u16)> = HashSet::new();
        let mut dec_set: HashSet<(i64, i64)> = HashSet::new();
        let mut shared = true;
        for chunk in chunks(&col) {
            let view = chunk
                .as_opt::<Struct>()
                .ok_or("pair chunk is not a struct")?;
            let a = view.unmasked_field_by_name("attr1")?.clone();
            let b = view.unmasked_field_by_name("attr2")?.clone();
            let (Some((_, ca)), Some((_, cb))) =
                (dict_codes(&session, &a)?, dict_codes(&session, &b)?)
            else {
                shared = false;
                break;
            };
            for (x, y) in ca.into_iter().zip(cb) {
                code_set.insert((x, y));
            }
            let va = decode_i64(&session, &a)?;
            let vb = decode_i64(&session, &b)?;
            for (x, y) in va.into_iter().zip(vb) {
                dec_set.insert((x, y));
            }
        }
        if shared {
            assert_eq!(
                code_set.len(),
                dec_set.len(),
                "code-pair distinct must equal decoded distinct"
            );
            println!(
                "  O20b self-check: code pairs == decoded pairs ({} distinct); both columns dict, shared dictionary",
                dec_set.len()
            );
        } else {
            println!("  O20b: code path NOT available (columns not both dict / not shared)");
        }
    }

    Ok(results)
}
