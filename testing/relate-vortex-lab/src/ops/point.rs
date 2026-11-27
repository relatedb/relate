//! O1 point read by node-id, O2 multi-column read at the same rows.
//!
//! Vortex's read unit is the chunk/segment, not the value: a point read pays
//! one chunk decode, then value extraction is cheap. The bench therefore
//! reports the column decode cost once, and the per-value cost on the
//! decoded column — that pair is what a node-id lookup in the engine would
//! actually pay.

use std::path::Path;

use vortex::array::arrays::struct_::{Struct, StructArrayExt};
use vortex::array::VortexSessionExecute;

use crate::bench::{measure, spread_row, Measured};
use crate::store::{self, Ty};

/// The Person properties, the LDBC shape with the richest property mix.
const PERSON_COLS: &[(&str, Ty)] = &[
    ("id", Ty::Int),
    ("firstName", Ty::Str),
    ("lastName", Ty::Str),
    ("gender", Ty::Str),
    ("birthday", Ty::Str),
    ("browserUsed", Ty::Str),
];

/// The LDBC size run: the three largest tables, largest first — the memory
/// thesis lives in the string-heavy ones.
pub fn size_ldbc(base: &Path, out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let tables: Vec<(&str, Vec<(&str, Ty)>)> = vec![
        (
            "comment_0_0.csv",
            vec![
                ("id", Ty::Int),
                ("creationDate", Ty::Str),
                ("locationIP", Ty::Str),
                ("browserUsed", Ty::Str),
                ("content", Ty::Str),
                ("length", Ty::Int),
            ],
        ),
        (
            "post_0_0.csv",
            vec![
                ("id", Ty::Int),
                ("creationDate", Ty::Str),
                ("locationIP", Ty::Str),
                ("browserUsed", Ty::Str),
                ("language", Ty::Str),
                ("content", Ty::Str),
                ("length", Ty::Int),
            ],
        ),
        (
            "forum_0_0.csv",
            vec![
                ("id", Ty::Int),
                ("title", Ty::Str),
                ("creationDate", Ty::Str),
            ],
        ),
    ];
    for (file, cols) in tables {
        size(&base.join(file), &cols, out_dir)?;
    }
    Ok(())
}

/// The size bench alone, for any table: bytes/property vs the uncompressed
/// value bytes. The comment/post tables (151k/136k rows, content strings)
/// are where the memory thesis actually gets tested.
pub fn size(
    csv: &Path,
    cols: &[(&str, Ty)],
    out_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let table = store::load_csv(csv, cols)?;
    let path = out_dir.join(format!("{}.vortex", table.name));
    let (written, per_column) = store::write(&path, &table)?;
    store::print_sizes(&table, per_column.as_deref(), written);
    Ok(())
}

pub fn run(
    csv: &Path,
    out_dir: &Path,
    iters: u64,
) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();

    let table = store::load_csv(csv, PERSON_COLS)?;
    let path = out_dir.join("person.vortex");
    let (written, per_column) = store::write(&path, &table)?;
    store::print_sizes(&table, per_column.as_deref(), written);

    // Open through the mmap and decode the table once: the column decode a
    // point read pays per touched chunk.
    let (session, rt, vfile) = store::open_mmap(&path)?;
    let decode = measure("O1 read_all (COMPRESSED arrays, not decoded)", 1, |_| {
        let arr = store::scan(&vfile, &rt, store::ScanOpts::default()).unwrap();
        std::hint::black_box(arr);
    });
    decode.report();
    results.push(decode);

    let root = store::scan(&vfile, &rt, store::ScanOpts::default())?;
    let rows = root.len();
    let (gender, first, last, birthday) = {
        let view = root.as_opt::<Struct>().ok_or("root is not a struct")?;
        (
            view.unmasked_field_by_name("gender")?.clone(),
            view.unmasked_field_by_name("firstName")?.clone(),
            view.unmasked_field_by_name("lastName")?.clone(),
            view.unmasked_field_by_name("birthday")?.clone(),
        )
    };
    let mut ctx = session.create_execution_ctx();
    let o1 = measure("O1 point read (execute_scalar, warm column)", iters, |i| {
        let row = spread_row(i, rows as u64);
        let v = gender.execute_scalar(row, &mut ctx).unwrap();
        std::hint::black_box(v);
    });
    o1.report();
    results.push(o1);

    // O2: three columns at the same rows — the shape every query's RETURN has.

    let o2 = measure("O2 3-column read at the same rows", iters, |i| {
        let row = spread_row(i, rows as u64);
        let a = first.execute_scalar(row, &mut ctx).unwrap();
        let b = last.execute_scalar(row, &mut ctx).unwrap();
        let c = birthday.execute_scalar(row, &mut ctx).unwrap();
        std::hint::black_box((a, b, c));
    });
    o2.report();
    results.push(o2);

    Ok(results)
}
