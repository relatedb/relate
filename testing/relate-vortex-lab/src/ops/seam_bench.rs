//! The seam A/B: the same node-major table behind `PropStore` twice —
//! `RowProps` (compact-records-shaped baseline) vs `ColProps` (the vortex
//! store) — same operators, same answers asserted, both measured.
//!
//! The point is not to re-measure the kernels (vortex-research-results.md has those); it is
//! the seam overhead and the representation effect per operator: what the
//! executor would feel if the property store swapped behind the trait.

use std::path::Path;

use crate::bench::{measure, Measured};
use crate::colstore::ColProps;
use crate::seam::{Cmp, Pred, PropStore, Val};
use crate::store;

pub fn run(out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();

    let n = 150_000usize;
    let mut table = store::synthetic("mono", n, 4);
    // labels: 4 interleaved label codes (i%4) — every zone holds all
    // labels, so the label filter halves rows without pruning zones (the
    // realistic shape; correlated labels would prune too)
    table.columns.push((
        "label".into(),
        store::Ty::Int,
        store::Col::Int((0..n).map(|i| (i % 4) as i64).collect()),
    ));
    let row = crate::seam::RowProps::build(&table);
    let col = ColProps::build(&table, &out_dir.join("seam-mono.vortex"))?;
    println!(
        "seam A/B: {} rows, {} columns; row store built, col store mmap-opened",
        n,
        table.columns.len()
    );

    // A 1024-row morsel of ascending node ids (the executor's shape).
    let morsel: Vec<u64> = (0..1024).map(|i| (i * 147) % n as u64).collect();
    let mut morsel_sorted = morsel.clone();
    morsel_sorted.sort_unstable();

    // Selective range predicates at the one-third points (the measured
    // monotone shape; conjunction prunes the union of both ends).
    let a0 = match table.column("attr0") {
        Some(store::Col::Int(v)) => v.clone(),
        _ => unreachable!("mono attr0"),
    };
    let a1 = match table.column("attr1") {
        Some(store::Col::Int(v)) => v.clone(),
        _ => unreachable!("mono attr1"),
    };
    let third0 = a0.iter().copied().min().unwrap()
        + (a0.iter().copied().max().unwrap() - a0.iter().copied().min().unwrap()) / 3;
    let third1 = a1.iter().copied().min().unwrap()
        + (a1.iter().copied().max().unwrap() - a1.iter().copied().min().unwrap()) / 3;
    let pred = Pred::Cmp {
        key: "attr0",
        op: Cmp::Lt,
        val: 2 * third0,
    }
    .and(Pred::Cmp {
        key: "attr1",
        op: Cmp::Gt,
        val: third1,
    });
    let seek_key = "node-00071234";

    // ---- correctness: both stores must answer identically ----
    let exp_row = row.expand(&[], &pred, None);
    let exp_col = col.expand(&[], &pred, None);
    assert_eq!(exp_row, exp_col, "expand disagrees");
    assert_eq!(
        row.expand(&[], &pred, Some(100)),
        col.expand(&[], &pred, Some(100)),
        "capped expand disagrees"
    );
    for r in [0u64, 7, 74_943, n as u64 - 1] {
        assert_eq!(
            row.point(r, "attr0"),
            col.point(r, "attr0"),
            "point {r} disagrees"
        );
    }
    assert_eq!(
        row.batch(&morsel_sorted, &["attr0", "attr1"]),
        col.batch(&morsel_sorted, &["attr0", "attr1"]),
        "batch disagrees"
    );
    assert_eq!(
        row.seek("key", seek_key),
        col.seek("key", seek_key),
        "seek disagrees"
    );
    assert_eq!(
        row.distinct("attr0"),
        col.distinct("attr0"),
        "distinct disagrees"
    );
    assert_eq!(
        row.topk("attr1", 10),
        col.topk("attr1", 10),
        "topk disagrees"
    );
    println!(
        "  correctness: all operators agree ({}/{} rows expand, seek hit, 150k distincts, top-10)",
        exp_row.len(),
        n
    );

    // ---- S1: point read ----
    let point_iters = iters.clamp(100, 50_000);
    let m = measure("S1a point attr0 (row store)", point_iters, |i| {
        let v = row.point(i % n as u64, "attr0");
        std::hint::black_box(v);
    });
    m.report();
    results.push(m);
    let m = measure("S1b point attr0 (col store)", point_iters, |i| {
        let v = col.point(i % n as u64, "attr0");
        std::hint::black_box(v);
    });
    m.report();
    results.push(m);

    // ---- S2: morsel batch (1024 ascending rows x 2 keys) ----
    let m = measure("S2a batch 1024x2 (row store)", iters.clamp(5, 500), |_| {
        let v = row.batch(&morsel_sorted, &["attr0", "attr1"]);
        std::hint::black_box(v.len());
    });
    m.report();
    results.push(m);
    let m = measure("S2b batch 1024x2 (col store)", iters.clamp(5, 500), |_| {
        let v = col.batch(&morsel_sorted, &["attr0", "attr1"]);
        std::hint::black_box(v.len());
    });
    m.report();
    results.push(m);

    // ---- S3: bulk expansion with pushed conjunction ----
    let m = measure(
        "S3a expand conjunction (row store)",
        iters.clamp(1, 50),
        |_| {
            let v = row.expand(&[], &pred, None);
            std::hint::black_box(v.len());
        },
    );
    m.report();
    results.push(m);
    let m = measure(
        "S3b expand conjunction (col store)",
        iters.clamp(1, 50),
        |_| {
            let v = col.expand(&[], &pred, None);
            std::hint::black_box(v.len());
        },
    );
    m.report();
    results.push(m);

    // ---- S3c: capped expansion (the engine's cap chain) ----
    let m = measure("S3c expand cap=100 (row store)", iters.clamp(1, 50), |_| {
        let v = row.expand(&[], &pred, Some(100));
        std::hint::black_box(v.len());
    });
    m.report();
    results.push(m);
    let m = measure("S3c expand cap=100 (col store)", iters.clamp(1, 50), |_| {
        let v = col.expand(&[], &pred, Some(100));
        std::hint::black_box(v.len());
    });
    m.report();
    results.push(m);

    // ---- S7: batch node+property filter (labels AND conjunctive pred) ----
    // the candidates-funnel shape: lookup_nodes_upto(labels, props, cap)
    let half: Vec<u32> = vec![0, 1]; // i%4 in {0,1} = half the nodes
    assert_eq!(
        row.expand(&half, &pred, None),
        col.expand(&half, &pred, None),
        "label+pred expand disagrees"
    );
    let m = measure(
        "S7a label{0,1}+conj (row store)",
        iters.clamp(1, 50),
        |_| {
            let v = row.expand(&half, &pred, None);
            std::hint::black_box(v.len());
        },
    );
    m.report();
    results.push(m);
    let m = measure(
        "S7b label{0,1}+conj (col store)",
        iters.clamp(1, 50),
        |_| {
            let v = col.expand(&half, &pred, None);
            std::hint::black_box(v.len());
        },
    );
    m.report();
    results.push(m);

    // ---- S8: batched key check / IN-list (1024 sorted keys, one merge) ----
    let in_keys: Vec<String> = (0..1024)
        .map(|i| format!("node-{:08}", (i * 147) % n))
        .collect();
    let mut in_sorted = in_keys.clone();
    in_sorted.sort_unstable();
    assert_eq!(
        row.member("key", &in_sorted),
        col.member("key", &in_sorted),
        "member disagrees"
    );
    let m = measure(
        "S8a member 1024 keys (row store)",
        iters.clamp(5, 500),
        |_| {
            let v = row.member("key", &in_sorted);
            std::hint::black_box(v.len());
        },
    );
    m.report();
    results.push(m);
    let m = measure(
        "S8b member 1024 keys (col store)",
        iters.clamp(5, 500),
        |_| {
            let v = col.member("key", &in_sorted);
            std::hint::black_box(v.len());
        },
    );
    m.report();
    results.push(m);

    // ---- S8c: clustered batch (the morsel shape: keys from one region) ----
    let clustered: Vec<String> = (100_000..101_024).map(|i| format!("node-{i:08}")).collect();
    assert_eq!(
        row.member("key", &clustered),
        col.member("key", &clustered),
        "clustered member disagrees"
    );
    let m = measure(
        "S8c member 1024 clustered (row store)",
        iters.clamp(5, 500),
        |_| {
            let v = row.member("key", &clustered);
            std::hint::black_box(v.len());
        },
    );
    m.report();
    results.push(m);
    let m = measure(
        "S8c member 1024 clustered (col store)",
        iters.clamp(5, 500),
        |_| {
            let v = col.member("key", &clustered);
            std::hint::black_box(v.len());
        },
    );
    m.report();
    results.push(m);

    // ---- S4: equality seek on the sorted key arm ----
    let m = measure("S4a seek key (row store)", point_iters, |i| {
        let k = format!("node-{:08}", i % n as u64);
        let v = row.seek("key", &k);
        std::hint::black_box(&v);
    });
    m.report();
    results.push(m);
    let m = measure("S4b seek key (col store)", point_iters, |i| {
        let k = format!("node-{:08}", i % n as u64);
        let v = col.seek("key", &k);
        std::hint::black_box(&v);
    });
    m.report();
    results.push(m);

    // ---- S5: aggregation pushdown (distinct + counts) ----
    let m = measure("S5a distinct attr0 (row store)", iters.clamp(1, 20), |_| {
        let v = row.distinct("attr0");
        std::hint::black_box(v.len());
    });
    m.report();
    results.push(m);
    let m = measure("S5b distinct attr0 (col store)", iters.clamp(1, 20), |_| {
        let v = col.distinct("attr0");
        std::hint::black_box(v.len());
    });
    m.report();
    results.push(m);

    // ---- S6: sort/limit pushdown (top-10) ----
    let m = measure("S6a topk 10 attr1 (row store)", iters.clamp(1, 20), |_| {
        let v = row.topk("attr1", 10);
        std::hint::black_box(v.len());
    });
    m.report();
    results.push(m);
    let m = measure("S6b topk 10 attr1 (col store)", iters.clamp(1, 20), |_| {
        let v = col.topk("attr1", 10);
        std::hint::black_box(v.len());
    });
    m.report();
    results.push(m);

    Ok(results)
}
