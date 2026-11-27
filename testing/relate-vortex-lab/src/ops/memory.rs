//! Memory kernels: mmap resident accounting and the decoded-chunk cache.
//!
//! Three tables, the questions the round-1 discussion left open:
//! 1. per-shape **file bytes vs `UncompressedSizeInBytes` vs full-touch RSS vs
//!    partial-touch RSS** — the mmap page-accounting question (mmap pages count
//!    toward no engine cap today; needs its own decision record).
//! 2. **whole-column cache vs per-chunk `repeated_probe` cache** — hit and miss
//!    latency plus resident bytes. The design is per-chunk with a density hint
//!    (vortex-data/vortex#8694); the whole-column cache upper-bounds its cost.
//! 3. **density-hint cache size at three densities** — what a cache-the-touched-
//!    chunks policy retains when the caller says it will read d of the column,
//!    against the ideal `d × column` bytes.
//!
//! Resident bytes come from the counting allocator's live gauge (load-robust);
//! RSS comes from the process probe and is load-stamped like every other number.

use std::path::Path;

use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::arrays::struct_::{Struct, StructArrayExt};
use vortex::array::{ArrayRef, VortexSessionExecute};
use vortex::expr::stats::Stat;
use vortex::expr::{col, gt, lit};
use vortex::file::VortexFile;
use vortex::io::runtime::single::SingleThreadRuntime;
use vortex::session::VortexSession;

use crate::bench::{live_bytes, loadavg1, measure, rss_bytes, spread_row, Measured};
use crate::store;

/// The pokec shape's node count — the fixture's SMALL is 10k, MEDIUM 100k;
/// 100k keeps the labels' distributions meaningful at a lab cost.
const POKEC_NODES: usize = 100_000;
/// The fixture's MEDIUM seed (`testing/relate-test-support/src/pokec.rs`).
const POKEC_SEED: u64 = 0x706f_6b65_6303;

pub fn run(out_dir: &Path, iters: u64) -> Result<(), Box<dyn std::error::Error>> {
    println!("== memory: load {:.1} at start ==", loadavg1());
    table_shapes(out_dir)?;
    table_cache_and_density(out_dir, iters)?;
    Ok(())
}

/// The RSS delta, in KB, of one read's pages on a fresh mapping of `path`:
/// the mapping is opened, `read` runs, and the delta is what it faulted. The
/// session is returned for a caller that still needs it after the read.
fn touch_kb<T>(
    path: &Path,
    read: impl FnOnce(
        &VortexSession,
        &SingleThreadRuntime,
        &VortexFile,
    ) -> Result<T, Box<dyn std::error::Error>>,
) -> Result<(u64, VortexSession, T), Box<dyn std::error::Error>> {
    let (session, rt, vfile) = store::open_mmap(path)?;
    let before = rss_bytes()?;
    let value = read(&session, &rt, &vfile)?;
    let now = rss_bytes()?;
    Ok((now.saturating_sub(before) / 1024, session, value))
}

/// Table 1: the mmap page-accounting numbers, one row per shape.
fn table_shapes(out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let shapes: Vec<(&str, store::Table, &str)> = vec![
        (
            "synthetic-flat-150k",
            store::synthetic("flat", 150_000, 4),
            "id",
        ),
        (
            "synthetic-mono-150k",
            store::synthetic("mono", 150_000, 4),
            "id",
        ),
        ("pokec-100k", store::pokec(POKEC_NODES, POKEC_SEED), "id"),
    ];

    println!(
        "\n== table 1: mmap resident accounting (shape: file bytes vs UncompressedSizeInBytes \
         vs full-touch RSS vs partial-touch RSS; load {:.1}) ==",
        loadavg1()
    );
    println!(
        "{:<22} {:>8} {:>12} {:>26} {:>20} {:>23}",
        "shape",
        "rows",
        "file bytes",
        "UncompressedSizeInBytes",
        "full-touch RSS KB",
        "partial-touch RSS KB"
    );
    for (name, table, partial_col) in shapes {
        let path = out_dir.join(format!("memory-{name}.vortex"));
        let (file_bytes, _) = store::write(&path, &table)?;

        // Full touch on a fresh mapping first: every page faults, and the
        // logical byte count comes off the decoded root.
        let (full_kb, session, root) = touch_kb(&path, |_, rt, vfile| {
            Ok(store::scan(vfile, rt, store::ScanOpts::default())?)
        })?;
        let mut ctx = session.create_execution_ctx();
        let uncompressed = root
            .statistics()
            .compute_as::<usize>(Stat::UncompressedSizeInBytes, &mut ctx)
            .ok_or("UncompressedSizeInBytes unavailable for the table dtype")?;

        // Partial touch on a fresh mapping: only the projected column's pages
        // fault (an int column, so the delta stays page- not decode-dominated).
        let (partial_kb, _, _) = touch_kb(&path, |_, rt, vfile| {
            store::projected(rt, vfile, partial_col)
        })?;

        println!(
            "{:<22} {:>8} {:>12} {:>26} {:>20} {:>23}",
            name,
            table.rows(),
            file_bytes,
            uncompressed,
            full_kb,
            partial_kb
        );
    }
    Ok(())
}

/// Tables 2 and 3: the decoded-cache arms and the density-hint curve, on the
/// same 150k-row mono shape (one column, so chunk granularity is visible).
fn table_cache_and_density(out_dir: &Path, iters: u64) -> Result<(), Box<dyn std::error::Error>> {
    let table = store::synthetic("mono", 150_000, 4);
    let path = out_dir.join("memory-cache.vortex");
    store::write(&path, &table)?;
    let (session, rt, vfile) = store::open_mmap(&path)?;
    let column = store::projected(&rt, &vfile, "attr0")?;
    let chunks = store::chunks(&column);
    let rows = column.len() as u64;
    let lens: Vec<usize> = chunks.iter().map(|c| c.len()).collect();
    let mut ctx = session.create_execution_ctx();

    println!(
        "\n== table 2: whole-column cache vs per-chunk repeated_probe cache (attr0, {rows} rows, \
         {} chunks; load {:.1}) ==",
        chunks.len(),
        loadavg1()
    );
    println!(
        "{:<34} {:>12} {:>12} {:>14}",
        "arm", "miss ns/op", "hit ns/op", "resident B"
    );

    // ---- whole-column arm: decode the column once, then index the Vec ----
    let decode_iters = (iters / 20).max(2);
    let miss_whole = measure("whole-column miss (decode to Vec)", decode_iters, |_| {
        let decoded = column.clone().execute::<PrimitiveArray>(&mut ctx).unwrap();
        let cache: Vec<i64> = decoded.as_slice::<i64>().to_vec();
        std::hint::black_box(cache);
    });
    let before = live_bytes();
    let cache: Vec<i64> = {
        let decoded = column.clone().execute::<PrimitiveArray>(&mut ctx)?;
        decoded.as_slice::<i64>().to_vec()
    };
    let resident_whole = live_bytes().saturating_sub(before);
    let hit_whole = measure("whole-column hit (Vec index)", iters, |i| {
        std::hint::black_box(cache[spread_row(i, rows)]);
    });
    println!(
        "{:<34} {:>12.1} {:>12.1} {:>14}",
        "whole-column decode + Vec", miss_whole.ns_per_op, hit_whole.ns_per_op, resident_whole
    );

    // ---- per-chunk arm: one retained probe per chunk, primed once ----
    let sweep_iters = (iters / 5).max(2);
    let nchunks = chunks.len() as f64;
    let miss_chunk = measure("per-chunk probe miss (state build)", sweep_iters, |_| {
        for chunk in &chunks {
            let mut probe = chunk.repeated_probe();
            std::hint::black_box(probe.execute_scalar(0, &mut ctx).unwrap());
        }
    });

    let before = live_bytes();
    let mut probes: Vec<_> = chunks.iter().map(ArrayRef::repeated_probe).collect();
    for probe in &mut probes {
        probe.execute_scalar(0, &mut ctx)?;
    }
    let resident_chunk = live_bytes().saturating_sub(before);

    let hit_chunk = measure("per-chunk probe hit", sweep_iters, |i| {
        for (probe, len) in probes.iter_mut().zip(&lens) {
            let row = spread_row(i, *len as u64);
            std::hint::black_box(probe.execute_scalar(row, &mut ctx).unwrap());
        }
    });
    println!(
        "{:<34} {:>12.1} {:>12.1} {:>14}",
        "per-chunk repeated_probe (per chunk)",
        miss_chunk.ns_per_op / nchunks,
        hit_chunk.ns_per_op / nchunks,
        resident_chunk
    );
    println!(
        "  (full {}-chunk sweep: hit {:.1} ns, miss {:.1} ns; load {:.1}->{:.1} at the hits)",
        chunks.len(),
        hit_chunk.ns_per_op,
        miss_chunk.ns_per_op,
        hit_chunk.load_start,
        hit_chunk.load_end
    );

    // ---- table 3: cache-the-touched-chunks size at three densities ----
    let sizes: Vec<u64> = chunks
        .iter()
        .map(|c| {
            let n = c
                .statistics()
                .compute_as::<usize>(Stat::UncompressedSizeInBytes, &mut ctx)
                .ok_or("UncompressedSizeInBytes unavailable for a chunk")?;
            Ok(n as u64)
        })
        .collect::<Result<Vec<u64>, Box<dyn std::error::Error>>>()?;
    let column_bytes: u64 = sizes.iter().sum();
    let mut bounds = Vec::with_capacity(chunks.len());
    let mut acc = 0u64;
    for len in &lens {
        acc += *len as u64;
        bounds.push(acc);
    }

    println!(
        "\n== table 3: density-hint cache size (attr0, {rows} rows, column {} B; load {:.1}) ==",
        column_bytes,
        loadavg1()
    );
    println!(
        "{:>9} {:>9} {:>18} {:>14} {:>14}",
        "density", "rows", "chunks touched", "cache B", "ideal B (d*col)"
    );
    for density in [0.01f64, 0.10, 1.00] {
        let want = ((rows as f64 * density).round() as usize).max(1);
        let mut touched = vec![false; chunks.len()];
        for k in 0..want {
            // uniform spread: the density the hint is asked about
            let row = (k as u64 * rows) / want as u64;
            let chunk = bounds.partition_point(|&b| b <= row);
            touched[chunk.min(chunks.len() - 1)] = true;
        }
        let cache_bytes: u64 = sizes
            .iter()
            .zip(&touched)
            .filter(|(_, t)| **t)
            .map(|(s, _)| *s)
            .sum();
        let ideal = (rows as f64 * density) as u64 * (column_bytes / rows.max(1));
        println!(
            "{:>8.0}% {:>9} {:>13}/{:<4} {:>14} {:>14}",
            density * 100.0,
            want,
            touched.iter().filter(|t| **t).count(),
            chunks.len(),
            cache_bytes,
            ideal
        );
    }
    Ok(())
}

/// The pokec kernel command: the fixture's `:User` shape, a point read by node
/// id, and the `age > 50` filter class (the fixture's documented small-answer
/// query) both as a pushed scan and as the decoded-count oracle.
pub fn pokec(out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();
    let table = store::pokec(POKEC_NODES, POKEC_SEED);
    let path = out_dir.join("pokec.vortex");
    let (written, _) = store::write(&path, &table)?;
    println!(
        "\n== pokec kernel: {} rows, 4 columns, file {written} B, load {:.1} ==",
        table.rows(),
        loadavg1()
    );

    let (session, rt, vfile) = store::open_mmap(&path)?;
    let root = store::scan(&vfile, &rt, store::ScanOpts::default())?;
    let rows = root.len();
    let age = {
        let view = root
            .as_opt::<Struct>()
            .ok_or("pokec root is not a struct")?;
        view.unmasked_field_by_name("age")?.clone()
    };
    let mut ctx = session.create_execution_ctx();
    let decoded = age.execute::<PrimitiveArray>(&mut ctx)?;
    let ages = decoded.as_slice::<i64>();
    let old = ages.iter().filter(|&&a| a > 50).count();
    println!(
        "  age > 50 (the fixture's small-answer filter class): {old} of {rows} \
         ({:.2}%, the documented ~1/800 old arm)",
        100.0 * old as f64 / rows.max(1) as f64
    );

    let m = measure("P1 point read age by row (node id)", iters, |i| {
        let row = spread_row(i, rows as u64);
        std::hint::black_box(decoded.execute_scalar(row, &mut ctx).unwrap());
    });
    m.report();
    results.push(m);

    let filter = gt(col("age"), lit(50i64))
        .bind(vfile.dtype())?
        .optimize_recursive()?;
    let projection = col("age").bind(vfile.dtype())?.optimize_recursive()?;
    let m = measure(
        "P2 pushed filter scan count age>50",
        iters.clamp(1, 100),
        |_| {
            let arr = store::scan(
                &vfile,
                &rt,
                store::ScanOpts {
                    filter: Some(&filter),
                    projection: Some(&projection),
                    ..store::ScanOpts::default()
                },
            )
            .unwrap();
            assert_eq!(
                arr.len(),
                old,
                "pushed filter disagrees with the decoded count"
            );
            std::hint::black_box(arr.len());
        },
    );
    m.report();
    results.push(m);
    Ok(results)
}
