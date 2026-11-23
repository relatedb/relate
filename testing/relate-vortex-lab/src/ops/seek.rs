//! O3 seek by business key, and the morsel gather (O10b's file primitive).
//!
//! Three mechanisms, cheapest to dearest to implement:
//! - scan + compare: the no-index baseline (read the whole column)
//! - binary search over the decoded sorted column: log2(n) value probes
//! - `with_row_indices`: read exactly the given rows — the batched gather a
//!   morsel-shaped operator needs (order-preserving, sorted row list)

use std::path::Path;

use vortex::array::stream::ArrayStreamExt;
use vortex::buffer::Buffer;
use vortex::io::runtime::BlockingRuntime;
use vortex::scan::strict_sorted_buffer::StrictSortedBuffer;

use crate::bench::{measure, Measured};
use crate::store;

pub fn run(shape: &str, n: usize, attrs: usize, out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();

    let table = store::synthetic(shape, n, attrs);
    let path = out_dir.join(format!("synthetic-{shape}.vortex"));
    let (written, per_column) = store::write(&path, &table)?;
    println!(
        "size: {} — {} rows, {} B raw -> {} B vortex ({:.1}%), file {} B",
        table.name,
        table.rows(),
        table.raw_bytes(),
        per_column.iter().sum::<u64>(),
        100.0 * per_column.iter().sum::<u64>() as f64 / table.raw_bytes().max(1) as f64,
        written
    );
    for ((name, _, col), bytes) in table.columns.iter().zip(per_column.iter()) {
        println!(
            "  {:<8} {:>8} values {:>10} B raw -> {:>9} B ({:>6.1} B/value)",
            name,
            col.len(),
            col.raw_bytes(),
            bytes,
            *bytes as f64 / col.len().max(1) as f64
        );
    }

    let (_session, rt, vfile) = store::open_mmap(&path)?;
    let root = rt.block_on(async { vfile.scan()?.into_array_stream()?.read_all().await })?;
    let rows = root.len();

    // The key column, decoded once (what any seek pays to touch the column).
    let decode = measure("O3 read_all (COMPRESSED arrays, not decoded)", 1, |_| {
        let a = rt
            .block_on(async { vfile.scan().unwrap().into_array_stream().unwrap().read_all().await })
            .unwrap();
        std::hint::black_box(a);
    });
    decode.report();
    results.push(decode);

    // O3a: no index — compare every value (the baseline the store replaces).
    let key = table.column("key").ok_or("no key column")?;
    let keys: Vec<String> = match key {
        store::Col::Str(v) => v.clone(),
        _ => return Err("key is not a string column".into()),
    };
    let o3a = measure("O3a scan+compare (no index)", iters, |i| {
        let target = format!("node-{:08}", ((i.wrapping_mul(2_654_435_761)) % n as u64));
        let hit = keys.iter().position(|k| *k == target);
        std::hint::black_box(hit);
    });
    o3a.report();
    results.push(o3a);

    // O3b: binary search over a sorted copy of the decoded column — the seek
    // the sorted key column would serve. (`flat` keys sort in id order; the
    // sorted copy is the index's own order.)
    let mut sorted: Vec<(&String, usize)> = keys.iter().enumerate().map(|(i, k)| (k, i)).collect();
    sorted.sort();
    let sorted_keys: Vec<&str> = sorted.iter().map(|(k, _)| k.as_str()).collect();
    let o3b = measure("O3b binary search (sorted key, in-memory)", iters, |i| {
        let target = format!("node-{:08}", ((i.wrapping_mul(2_654_435_761)) % n as u64));
        let hit = sorted_keys.binary_search(&target.as_str());
        std::hint::black_box(hit);
    });
    o3b.report();
    results.push(o3b);

    // O10b: read exactly the rows a morsel would carry — 1024 random ids,
    // sorted (the file wants a strictly sorted buffer).
    let mut idx: Vec<u64> = (0..1024)
        .map(|i| (i as u64 * 7919) % rows as u64)
        .collect();
    idx.sort_unstable();
    idx.dedup();
    let rows_per_batch = idx.len() as u64;
    let selection = StrictSortedBuffer::try_new(Buffer::from(idx))?;
    let o10b = measure("O10b with_row_indices gather (1024 rows)", iters.max(1) / 100 + 1, |_| {
        let batch = rt
            .block_on(async {
                vfile
                    .scan()?
                    .with_row_indices(selection.clone())
                    .into_array_stream()?
                    .read_all()
                    .await
            })
            .unwrap();
        std::hint::black_box(batch);
    });
    println!(
        "  (O10b reads {rows_per_batch} rows per call; ns/op above is per gather, not per row)"
    );
    o10b.report();
    results.push(o10b);

    Ok(results)
}
