//! An allocator-backed regression for the retained-family publish path
//! (ADR-0555): unlike a peak asserted from sizes, this sees every Vec
//! the publish holds — including an uninstrumented one — so a publish
//! that materializes its interval's dirty delta (the whole-table
//! record clone, the whole-delta chain Vec, the one-shot part encode)
//! cannot pass. Both family publishes run inside one measured
//! `checkpoint_at` on a store whose live image is empty (every born
//! node is deleted again — the codec's own dump stays tiny, so the
//! measured peak is the publishes') while both families' deltas grow
//! past the budget: the records through large changed-key payloads,
//! the chains through the born-and-tombstoned version chains. The
//! measured child runs alone in its own test process, without
//! parallel-test noise (the `wal_recovery_alloc.rs` pattern).

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use relate_core::{PreviewFeature, PreviewFeatures, PropertyMap, Value};
use relate_engine_api::{ChangedKey, ChangedKeyKind, RecordType, RedoOp, TransactionRecord};
use relate_executor::Graph;
use relate_storage::{
    checkpoint_at, parse_uri_with_options, read_to_vec, SnapshotCodecKind, StoreTarget,
};

struct CountingAllocator;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static MEASURING: AtomicBool = AtomicBool::new(false);

fn allocated(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    if MEASURING.load(Ordering::Relaxed) {
        PEAK.fetch_max(live, Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// SAFETY: System owns the allocations; counters only observe successful operations.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            allocated(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        let next = System.realloc(ptr, old, new_size);
        if !next.is_null() {
            if new_size > old.size() {
                allocated(new_size - old.size());
            } else {
                LIVE.fetch_sub(old.size() - new_size, Ordering::Relaxed);
            }
        }
        next
    }
}

#[derive(Debug)]
struct Footprint {
    peak: usize,
    after: usize,
}

/// Measure `run`'s allocation high-water and live bytes at its end: a
/// native store keeps its durable output on disk — outside the
/// allocator — so `peak` above `after` is the run's own transient, and
/// a temporary Vec dropped before return still counts at the peak.
fn measure<T>(run: impl FnOnce() -> T) -> (T, Footprint) {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    MEASURING.store(true, Ordering::Relaxed);
    let value = run();
    MEASURING.store(false, Ordering::Relaxed);
    let footprint = Footprint {
        peak: PEAK.load(Ordering::Relaxed),
        after: LIVE.load(Ordering::Relaxed),
    };
    (value, footprint)
}

/// The one changed key every fixture commit carries: large enough that
/// a few thousand commits push both families' deltas far past the
/// transient budget (the fraud-shaped ratio — many small records, a
/// few versions each — at a scale a debug test tolerates).
const KEY_BYTES: usize = 3072;
/// The transient budget: the publish's batch (1 MiB) plus its row and
/// growth slop sits well under it, while the same deltas' materializing
/// forms cross it — the small fixture's by roughly twice, the large
/// fixture's by an order of magnitude.
const TRANSIENT_BUDGET: usize = 3 * 1024 * 1024;
/// The two dirty-delta sizes: the publish's peak must be the same one
/// batch under the same budget at both (ADR-0555's independence shape
/// — whatever the input count).
const SMALL_ELEMENTS: i64 = 300;
const LARGE_ELEMENTS: i64 = 3_000;

/// One fixture store: a fresh native directory under a fresh tempdir.
fn fixture_store(dir: &tempfile::TempDir) -> StoreTarget {
    parse_uri_with_options(
        dir.path().join("db").to_str().unwrap(),
        None,
        Some(SnapshotCodecKind::Json),
    )
    .unwrap()
}

/// One fixture graph with `value-provenance` retention on: `elements`
/// nodes each born with one large property and deleted again — the
/// live image stays empty while the version chains (the `chains/`
/// delta) and the per-commit retained records (the `transactions/`
/// delta) grow to ~2 KiB per element each. Seeded through the WAL
/// replay path's own apply, so the dirty marks the publish reads are
/// the replay's real ones.
fn fixture_graph(elements: i64) -> Graph {
    let mut graph = Graph::default();
    graph.configure_preview(PreviewFeatures::none().with_feature(PreviewFeature::ValueProvenance));
    let key = "k".repeat(KEY_BYTES);
    for i in 0..elements {
        let mut props = BTreeMap::new();
        props.insert("k".to_string(), Value::String(key.clone().into()));
        graph
            .apply_replayed_redo(
                vec![RedoOp::CreateNode {
                    id: i,
                    labels: Arc::new(Vec::new()),
                    properties: Arc::new(PropertyMap::from(props)),
                }],
                2 * i as u64 + 1,
            )
            .unwrap();
        graph
            .apply_replayed_redo(vec![RedoOp::DeleteNode { id: i }], 2 * i as u64 + 2)
            .unwrap();
        for commit_ts in [2 * i as u64 + 1, 2 * i as u64 + 2] {
            graph.record_transaction(TransactionRecord {
                lsn: commit_ts,
                commit_ts,
                metadata: None,
                changed_keys: Some(vec![ChangedKey {
                    kind: ChangedKeyKind::Node,
                    id: i,
                    key: key.clone(),
                }]),
                record_type: RecordType::default(),
            });
        }
    }
    graph
}

/// One measured checkpoint over a fixture, asserting the whole
/// `checkpoint_at` (both family publishes plus the tiny empty-image
/// dump) holds less than the transient budget above its durable
/// output — and that the publishes really happened: both families'
/// parts landed, the records delta split into parts at the batch
/// budget (so it really streamed), and both envelopes decode with the
/// load's field shapes.
fn measured_publish(store: &StoreTarget, graph: &Graph, elements: i64) {
    let ((), published) = measure(|| {
        checkpoint_at(store, graph, 1, graph.commit_ts(), None).unwrap();
    });
    assert!(
        published.peak.saturating_sub(published.after) < TRANSIENT_BUDGET,
        "the {elements}-element publish held input-proportional memory: {published:?}"
    );
    let vfs = store.vfs();
    let tx_parts = vfs.list_dir("transactions").unwrap();
    assert!(
        tx_parts.len() > 1,
        "the records delta ({elements} elements) must split into bounded parts, \
         got {tx_parts:?}"
    );
    for name in &tx_parts {
        let len = vfs.metadata(&format!("transactions/{name}")).unwrap().len;
        // One batch (1 MiB) plus one record row is the most a part
        // holds; the part bound's own class would refuse far more.
        assert!(
            len < 2 * 1024 * 1024,
            "transactions/{name} is {len} bytes: parts must stay at the batch budget"
        );
    }
    let chains_parts = vfs.list_dir("chains").unwrap();
    assert_eq!(chains_parts.len(), 1, "{chains_parts:?}");
    // Both families' first parts decode, with the envelope's fields —
    // the hand-assembled streaming envelope is pinned to the load's
    // decode shape here.
    for (dir, fields) in [
        ("transactions", &["records"][..]),
        ("chains", &["nodes", "rels"]),
    ] {
        let name = vfs.list_dir(dir).unwrap().remove(0);
        let bytes = read_to_vec(&*vfs, &format!("{dir}/{name}")).unwrap();
        let part: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        for field in fields {
            assert!(
                part.get(*field)
                    .map(serde_json::Value::is_array)
                    .unwrap_or(false),
                "{dir}/{name} must decode with a {field} array"
            );
        }
    }
}

/// The negative control's two halves — the former publish's memory
/// shape through today's own APIs: the whole-table record clone plus
/// its one-shot encode, and the whole-delta chain collection plus its
/// one-shot encode. Everything is dropped inside the measured closure
/// (the lengths are the only thing that escapes), so the durable
/// output cannot subtract itself from the peak: the same allocator
/// must catch both halves on the large fixture, so the budget above is
/// not vacuous.
fn materializing_controls(graph: &Graph) {
    let ((records_len, body_len), records_control) = measure(|| {
        let records = graph.retained_transactions();
        let body = serde_json::to_vec(&records).unwrap();
        (records.len(), body.len())
    });
    assert!(records_len > 0 && body_len > 0);
    assert!(
        records_control.peak.saturating_sub(records_control.after) > TRANSIENT_BUDGET,
        "the records control must catch the whole-table clone: {records_control:?}"
    );
    let ((chains_len, chains_body_len), chains_control) = measure(|| {
        let mut chains = Vec::new();
        graph
            .retained_node_chains_into(&mut |chain| {
                chains.push(chain.clone());
                Ok(())
            })
            .unwrap();
        let body = serde_json::to_vec(&chains).unwrap();
        (chains.len(), body.len())
    });
    assert!(chains_len > 0 && chains_body_len > 0);
    assert!(
        chains_control.peak.saturating_sub(chains_control.after) > TRANSIENT_BUDGET,
        "the chains control must catch the whole-delta collection: {chains_control:?}"
    );
}

fn run_child() {
    // The small delta: already past the budget in its materializing
    // form, one batch in its streaming one.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = fixture_store(&dir);
        let graph = fixture_graph(SMALL_ELEMENTS);
        measured_publish(&store, &graph, SMALL_ELEMENTS);
    }
    // The ten-times delta through the same one-batch footprint, after
    // the negative controls proved the instrument on this very fixture.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = fixture_store(&dir);
        let graph = fixture_graph(LARGE_ELEMENTS);
        materializing_controls(&graph);
        measured_publish(&store, &graph, LARGE_ELEMENTS);
    }
}

#[test]
fn family_publish_allocation_is_bounded_on_native() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "measured_family_publish_child", "--nocapture"])
        .env("RELATE_FAMILY_PUBLISH_MEASURE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "measured family-publish child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn measured_family_publish_child() {
    if std::env::var("RELATE_FAMILY_PUBLISH_MEASURE").is_ok() {
        run_child();
    }
}
