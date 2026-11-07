//! Planning rewrite units over slots (ADR-0183 Decision 2): kept files that
//! hold more rows or bytes together than one file can stay apart, so the
//! halves of a file halved on an envelope miss are not merged again. Both
//! encodings cut a seed at the kind's page-envelope row cap — the wide cap
//! the smaller, its many leaf columns making rows a binding bound
//! (ADR-0534 — the writer test over a real graph covers that cut).

use super::super::coalesce::{plan_units, Step};
use super::super::layout::Slot;
use super::super::*;
use super::harness::checkpoint_at;
use super::harness::*;
use crate::codec::parquet::{decoded_preflight, take_encoded_rows, take_measured_rows};
use crate::memory_store;
use relate_core::Value;
use relate_executor::Graph;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

/// What planning makes of the slots: a kept part's `[id_lo, id_hi)`, or a
/// unit's slot indexes and the `[start, end)` of each of its files.
#[derive(Debug, PartialEq)]
enum Planned {
    Keep(i64, i64),
    Unit(Range<usize>, Vec<(i64, i64)>),
}

/// Plan `slots` over `all_ids` around `dirty` for `kind` at `target`
/// bytes under `encoding` (ADR-0534). The graph is empty, so no id costs
/// decoded bytes: only the width, the manifest and the row cap decide.
fn plan(
    kind: Kind,
    target: u64,
    encoding: Encoding,
    slots: &[Slot],
    all_ids: &[i64],
    dirty: &[i64],
) -> Vec<Planned> {
    let store = memory_store();
    let graph = Graph::default();
    let mut owned = Vec::new();
    let ctx = ShardWrite::new(&store, &graph, 1, kind, target, encoding, &mut owned);
    let steps = plan_units(&ctx, slots, all_ids, dirty).unwrap();
    steps
        .into_iter()
        .map(|step| match step {
            Step::Keep(part) => Planned::Keep(part.id_lo, part.id_hi),
            Step::Rewrite(unit) => {
                let files = unit.files.iter().map(|f| (f.start, f.end)).collect();
                Planned::Unit(unit.slots, files)
            }
        })
        .collect()
}

#[test]
fn kept_files_over_one_file_together_stay_apart() {
    let mid = 500_000;
    let pair = |rows, bytes| {
        [(0, mid), (mid, 2 * mid)].map(|(lo, hi)| Slot::Kept(node_part(lo, hi, rows, bytes)))
    };
    let merged = || vec![Planned::Unit(0..2, vec![(0, 2 * mid)])];
    // The left part seeds a unit, or the right one does.
    let apart = |dirty: i64| {
        if dirty < mid {
            vec![
                Planned::Unit(0..1, vec![(0, mid)]),
                Planned::Keep(mid, 2 * mid),
            ]
        } else {
            vec![
                Planned::Keep(0, mid),
                Planned::Unit(1..2, vec![(mid, 2 * mid)]),
            ]
        }
    };
    let target = 10_000;
    // Both encodings (ADR-0534): the wide row cap is the legacy one
    // shrunk by the column cap's leaf count.
    for encoding in [Encoding::Legacy, Encoding::Wide] {
        for kind in [Kind::Node, Kind::Rel] {
            // Two halves of the kind's page-envelope rows fill it — the
            // cap read from the kind itself, never restated; one row more
            // passes it.
            let cap = kind.page_envelope_rows(encoding.wide()) as u64;
            for (rows, joins) in [(cap / 2, true), (cap / 2 + 1, false)] {
                for dirty in [0, mid] {
                    let planned = plan(
                        kind,
                        target,
                        encoding,
                        &pair(rows, 5_000),
                        &[0, mid],
                        &[dirty],
                    );
                    let expected = if joins { merged() } else { apart(dirty) };
                    assert_eq!(planned, expected, "{kind:?} {encoding:?} {rows} rows");
                }
            }
        }
    }
    for dirty in [0, mid] {
        // The files' bytes together fill the target, or pass it.
        for (bytes, joins) in [(target / 2, true), (target / 2 + 1, false)] {
            let planned = plan(
                Kind::Node,
                target,
                Encoding::Legacy,
                &pair(1, bytes),
                &[0, mid],
                &[dirty],
            );
            let expected = if joins { merged() } else { apart(dirty) };
            assert_eq!(planned, expected, "{bytes} bytes");
        }
    }
}

#[test]
fn invisible_ids_cost_nothing_and_width_alone_cuts_nothing() {
    // The graph is empty, so no id costs decoded bytes: a seed of any
    // width is one file, legacy or wide (ADR-0534). Only bytes or the
    // row cap cut, and both need visible rows.
    let ids: Vec<i64> = (0..1_500_000).collect();
    let wide_slot = [Slot::Window(0..3_000_000)];
    for encoding in [Encoding::Legacy, Encoding::Wide] {
        let planned = plan(Kind::Node, MAX_FILE, encoding, &wide_slot, &ids, &[]);
        assert_eq!(planned, [Planned::Unit(0..1, vec![(0, 3_000_000)])]);
    }
    let narrow = [Slot::Window(0..MAX_WINDOW)];
    let planned = plan(
        Kind::Node,
        MAX_FILE,
        Encoding::Legacy,
        &narrow,
        &ids[..1_000],
        &[],
    );
    assert_eq!(planned, [Planned::Unit(0..1, vec![(0, MAX_WINDOW)])]);
}

/// `len` characters of a 64-letter alphabet drawn from `seed`. Snappy finds
/// few repeats in them and does no entropy coding, so they barely compress.
fn noise(seed: &mut u64, len: usize) -> String {
    const LETTERS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    (0..len)
        .map(|_| {
            *seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            char::from(LETTERS[(*seed >> 58) as usize])
        })
        .collect()
}

#[test]
fn halves_of_a_halved_file_stay_apart() {
    let store = memory_store();
    let graph = Graph::default();
    let mut seed = 1;
    let mut noisy = || {
        BTreeMap::from([(
            "text".to_owned(),
            Value::String(noise(&mut seed, 2000).into()),
        )])
    };
    graph.begin_write().unwrap();
    for _ in 0..8 {
        graph.create_node(vec!["Item".into()], noisy()).unwrap();
    }
    let ts = apply_commit(&graph);
    // The target is the eight rows' decoded bytes, so they are planned as
    // one file. Encoded, the noise and the file's own overhead pass the
    // target: the file misses its envelope once and is halved.
    let decoded = |ts| decoded_preflight(&graph, ts, &graph.all_node_ids_at(ts), Kind::Node);
    let target = decoded(ts).unwrap();
    take_encoded_rows();
    checkpoint_at(&store, &graph, 0, ts, target).unwrap();
    let halves = manifest(&store).node_parts;
    assert_eq!(ranges(&halves), [(0, 4), (4, 4096)]);
    assert_eq!(take_encoded_rows(), 8 + 4 + 4);
    assert!(halves[0].bytes + halves[1].bytes > target, "{halves:?}");

    // A write to either half rewrites that half alone. The two halves' live
    // rows still fit the target together; their files' bytes do not, so
    // the other half is ruled out before its rows are measured or read.
    for (half, id) in [(0, 1), (1, 6)] {
        let before = manifest(&store).node_parts;
        graph.begin_write().unwrap();
        graph.set_property_node(id, "text", Value::String(noise(&mut seed, 2000).into()));
        let ts = apply_commit(&graph);
        assert_eq!(decoded(ts).unwrap(), target);
        let (watched, opens) = watch_opens(&store, &before[half].path);
        take_measured_rows();
        take_encoded_rows();
        checkpoint_at(&watched, &graph, 0, ts, target).unwrap();
        assert_eq!(take_measured_rows(), 4, "half {half}");
        assert_eq!(take_encoded_rows(), 1, "half {half}");
        assert_eq!(opens.load(Ordering::SeqCst), 1, "half {half}");
        let parts = manifest(&store).node_parts;
        assert_eq!(ranges(&parts), ranges(&before));
        assert_eq!(parts[1 - half], before[1 - half]);
        assert_ne!(parts[half].path, before[half].path);
        assert_eq!(store.vfs.list_dir("nodes").unwrap().len(), 2);
        assert_reopens_as(&store, &graph);
    }
}
