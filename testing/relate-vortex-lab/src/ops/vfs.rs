//! VFS prototype: vortex property files mapped directly to memory.
//!
//! The same Person table in two layouts:
//! - **struct file**: one vortex file per table (columns = struct fields)
//! - **column files**: one vortex file per property key
//!
//! The measurements are the ones the layout decision turns on:
//! - V1 open cost: 1 file vs N files (footer parse per file)
//! - V2/V3 single-column read: projected read of one struct field vs one
//!   column file — time, allocs, and the RSS delta (only touched segments
//!   fault: the page-level skipping claim, measured not assumed)
//! - V4 3-column read: three projected scans of the struct file vs three
//!   column-file reads
//! - V5 whole-file decode: the all-pages RSS number the skipping claim
//!   is measured against
//! - V6 compaction remap: rewrite one column file, drop the old mapping,
//!   mmap the new one — the cost a fold/compact pass pays per touched
//!   column (new inode = cold pages, reported as such)

use std::path::Path;

use vortex::array::arrays::struct_::{Struct, StructArrayExt};
use vortex::array::ArrayRef;
use vortex::file::VortexFile;
use vortex::io::runtime::single::SingleThreadRuntime;

use crate::bench::{loadavg1, measure, rss_bytes, Measured};
use crate::store::{self, Ty};

/// The Person properties — the LDBC shape with the richest property mix.
const PERSON_COLS: &[(&str, Ty)] = &[
    ("id", Ty::Int),
    ("firstName", Ty::Str),
    ("lastName", Ty::Str),
    ("gender", Ty::Str),
    ("birthday", Ty::Str),
    ("browserUsed", Ty::Str),
];

fn print_rss(label: &str, before: u64) -> Result<(), Box<dyn std::error::Error>> {
    let now = rss_bytes()?;
    println!(
        "  {label}: RSS {} -> {} KB (delta {} KB, load {:.1})",
        before / 1024,
        now / 1024,
        now.saturating_sub(before) / 1024,
        loadavg1()
    );
    Ok(())
}

/// A whole-file read, unwrapped to the named column (single-column files
/// are one-field structs at the root).
fn file_read(
    vfile: &VortexFile,
    rt: &SingleThreadRuntime,
    name: &str,
) -> Result<ArrayRef, Box<dyn std::error::Error>> {
    let arr = store::scan(vfile, rt, store::ScanOpts::default())?;
    let view = arr
        .as_opt::<Struct>()
        .ok_or("column file root is not a struct")?;
    Ok(view.unmasked_field_by_name(name)?.clone())
}

pub fn run(
    csv: &Path,
    out_dir: &Path,
    iters: u64,
) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();

    // ---- build both layouts of the same table ----
    let table = store::load_csv(csv, PERSON_COLS)?;
    let n_cols = table.columns.len();

    let struct_path = out_dir.join("vfs-person-struct.vortex");
    let (struct_written, _) = store::write(&struct_path, &table)?;

    let mut col_paths = Vec::new();
    for (name, colv) in &table.columns {
        let one = store::Table {
            name: format!("person-{name}"),
            columns: vec![(name.clone(), colv.clone())],
        };
        let p = out_dir.join(format!("vfs-person-col-{name}.vortex"));
        let (w, _) = store::write(&p, &one)?;
        col_paths.push((name.clone(), p, w));
    }
    let col_bytes: u64 = col_paths.iter().map(|(_, _, w)| *w).sum();
    println!(
        "vfs layouts: {} rows x {n_cols} columns — struct file {} B, {n_cols} column files {} B ({} B/column avg)",
        table.rows(),
        struct_written,
        col_bytes,
        col_bytes / n_cols.max(1) as u64
    );

    // ---- V1: open cost, 1 file vs N files ----
    let m = measure(
        "V1 open struct file (1 mmap + footer)",
        iters.clamp(1, 200),
        |_| {
            let f = store::open_mmap(&struct_path).unwrap();
            std::hint::black_box(&f);
            drop(f);
        },
    );
    m.report();
    results.push(m);

    let m = measure(
        &format!("V1 open {n_cols} column files ({n_cols} mmap + footers)"),
        iters.clamp(1, 200),
        |_| {
            for (_, p, _) in &col_paths {
                let f = store::open_mmap(p).unwrap();
                std::hint::black_box(&f);
                drop(f);
            }
        },
    );
    m.report();
    results.push(m);

    // ---- open both layouts, leave them resident for the read measurements ----
    let (session, rt, sfile) = store::open_mmap(&struct_path)?;
    let mut cfiles = Vec::new();
    for (name, p, _) in &col_paths {
        cfiles.push((name.clone(), store::open_mmap(p)?));
    }
    let rss0 = rss_bytes()?;
    println!(
        "after opening both layouts (no reads): RSS {} KB",
        rss0 / 1024
    );

    // ---- V3: single-column read, both layouts ----
    // struct file, projected read of lastName
    let before = rss_bytes()?;
    let last_struct = store::projected(&rt, &sfile, "lastName")?;
    print_rss("V3a struct projected read lastName (fresh open)", before)?;
    let last_struct_warm = last_struct.clone();
    let m = measure(
        "V3a struct file, projected read lastName",
        iters.clamp(5, 100),
        |_| {
            let a = store::projected(&rt, &sfile, "lastName").unwrap();
            std::hint::black_box(a.len());
        },
    );
    m.report();
    results.push(m);

    // column file for lastName
    let before = rss_bytes()?;
    let (_, (_, crt, cfile)) = cfiles
        .iter()
        .find(|(n, _)| n == "lastName")
        .ok_or("no lastName column file")?;
    let last_col = file_read(cfile, crt, "lastName")?;
    print_rss("V3b column file read lastName (fresh open)", before)?;
    assert_eq!(
        last_col.len(),
        last_struct_warm.len(),
        "both layouts must agree on length"
    );
    let m = measure(
        "V3b column file, read lastName",
        iters.clamp(5, 100),
        |_| {
            let a = file_read(cfile, crt, "lastName").unwrap();
            std::hint::black_box(a.len());
        },
    );
    m.report();
    results.push(m);

    // ---- V4: three-column read (the RETURN shape) ----
    let want = ["firstName", "lastName", "birthday"];
    let m = measure(
        "V4a struct file, 3 projected scans",
        iters.clamp(5, 100),
        |_| {
            let mut total = 0usize;
            for w in want {
                total += store::projected(&rt, &sfile, w).unwrap().len();
            }
            std::hint::black_box(total);
        },
    );
    m.report();
    results.push(m);

    let m = measure("V4b 3 column files, read each", iters.clamp(5, 100), |_| {
        let mut total = 0usize;
        for (name, (_, r, f)) in &cfiles {
            if !want.contains(&name.as_str()) {
                continue;
            }
            total += file_read(f, r, name).unwrap().len();
        }
        std::hint::black_box(total);
    });
    m.report();
    results.push(m);

    // ---- V5: whole-file decode — the all-pages number ----
    let before = rss_bytes()?;
    let all = store::scan(&sfile, &rt, store::ScanOpts::default())?;
    std::hint::black_box(&all);
    print_rss("V5 struct file read_all (all columns)", before)?;

    // ---- V7: page-level skipping on a big file ----
    // The Person file (47 KB) is too small for RSS to resolve; the Comment
    // table (~6 MB vortex, 151k rows) is where "untouched segments never
    // fault" becomes measurable: one projected column vs the whole file.
    let base = csv.parent().ok_or("csv has no parent dir")?;
    let comment_csv = base.join("comment_0_0.csv");
    if comment_csv.exists() {
        const COMMENT_COLS: &[(&str, Ty)] = &[
            ("id", Ty::Int),
            ("creationDate", Ty::Str),
            ("locationIP", Ty::Str),
            ("browserUsed", Ty::Str),
            ("content", Ty::Str),
            ("length", Ty::Int),
        ];
        let big = store::load_csv(&comment_csv, COMMENT_COLS)?;
        let big_path = out_dir.join("vfs-comment.vortex");
        let (big_written, _) = store::write(&big_path, &big)?;
        let (_bs, brt, bfile) = store::open_mmap(&big_path)?;
        println!(
            "V7 comment store: {} rows, file {} B",
            big.rows(),
            big_written
        );

        // drop decoded arrays immediately so RSS deltas are page-dominated
        let before = rss_bytes()?;
        drop(store::projected(&brt, &bfile, "length")?);
        print_rss("V7a projected read `length` only (1 of 6 columns)", before)?;

        let before = rss_bytes()?;
        let all = store::scan(&bfile, &brt, store::ScanOpts::default())?;
        let all_len = all.len();
        drop(all);
        print_rss("V7b read_all (all 6 columns)", before)?;
        println!("  (both reads cover {all_len} rows; delta ratio = the skipping factor)");
    }

    // ---- V6: compaction remap — rewrite one column file, remap ----
    // New inode: the page cache cannot warm it from the old file, so the
    // read cost is the honest cold compaction cost.
    let one = store::Table {
        name: "person-lastName".into(),
        columns: vec![(
            "lastName".into(),
            table
                .column("lastName")
                .cloned()
                .ok_or("no lastName column")?,
        )],
    };
    let v2_path = out_dir.join("vfs-person-col-lastName-v2.vortex");
    store::write(&v2_path, &one)?;
    let m = measure(
        "V6a remap: open rewritten column file",
        iters.clamp(1, 200),
        |_| {
            let f = store::open_mmap(&v2_path).unwrap();
            std::hint::black_box(&f);
            drop(f);
        },
    );
    m.report();
    results.push(m);

    let (_s6, rt6, f6) = store::open_mmap(&v2_path)?;
    let before = rss_bytes()?;
    let v6 = file_read(&f6, &rt6, "lastName")?;
    print_rss("V6b remapped file, first read (cold pages)", before)?;
    let m = measure(
        "V6b remapped column file, read lastName",
        iters.clamp(5, 100),
        |_| {
            let a = file_read(&f6, &rt6, "lastName").unwrap();
            std::hint::black_box(a.len());
        },
    );
    m.report();
    results.push(m);
    assert_eq!(v6.len(), last_struct_warm.len());
    let _ = session;

    Ok(results)
}
