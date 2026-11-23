//! Multi-store open peak RSS gate (ADR-0664): many stores opened and closed
//! on four threads under the default mimalloc allocator keep the process
//! high-water RSS within twice the steady-state memory they hold.
//!
//! The test installs mimalloc with its default settings to reproduce a host
//! that embeds mimalloc as its global allocator (Relate's release binaries do
//! not bundle it), because the system allocator hides the open's transient
//! memory behind its own retention. Churning opens while the other
//! stores stay held open is the shape of a host that re-opens tenant stores
//! while the rest stay resident.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use relate::test_opts::writable;
use relate::{Database, OpenOptions, Value};
use relate_test_support::sampling::process_peak_rss_bytes;
use relate_test_support::{uri, walk_tree};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const STORES: usize = 12;
const THREADS: usize = 4;
const ROUNDS: usize = 1;
const LOAD_THREADS: usize = 4;
/// The held steady state the gate is measured against.
const MIN_HELD_BYTES: u64 = 64 << 20;
const ITEMS: usize = 24_000;
const TAIL: i64 = 5;

fn open(path: &Path) -> Database {
    Database::open_with(
        uri(path),
        OpenOptions {
            load_threads: Some(LOAD_THREADS),
            ..writable()
        },
    )
    .unwrap()
}

/// A checkpointed Parquet store whose nodes span several row groups, plus a
/// short WAL of commits made after the checkpoint.
fn build_template(dir: &Path) {
    let db = Database::open_with(uri(dir), writable()).unwrap();
    let mut conn = db.connect().unwrap();
    conn.execute(&format!(
        "UNWIND range(1, {ITEMS}) AS i \
         CREATE (:Item {{id: i, text: 'item-' + toString(i) + '-{}'}})-[:NEXT]->(:Leaf {{id: i}})",
        "x".repeat(96)
    ))
    .unwrap();
    db.checkpoint().unwrap();
    for i in 0..TAIL {
        conn.execute(&format!("CREATE (:Tail {{id: {i}}})"))
            .unwrap();
    }
}

fn copy_store(from: &Path, to: &Path) {
    walk_tree(from, |path, rel| {
        let dest = to.join(rel);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(path, dest).unwrap();
    });
}

/// Query the opened store, and check its open's transient peak is positive
/// yet below what the store holds once open.
fn check(db: &Database) -> u64 {
    let rows = db
        .connect()
        .unwrap()
        .query("MATCH (n:Item) RETURN count(n)")
        .unwrap()
        .rows;
    assert_eq!(rows[0][0], Value::Int(ITEMS as i64));
    let (used, _) = db.memory_usage().unwrap();
    let peak = db.open_peak_transient_bytes();
    assert!(
        0 < peak && peak < used,
        "open peak {peak} vs steady state {used}"
    );
    used
}

#[test]
fn concurrent_open_churn_keeps_peak_rss_within_twice_the_held_state() {
    assert_ne!(
        std::env::var("MIMALLOC_PURGE_DELAY").as_deref(),
        Ok("0"),
        "the gate measures mimalloc's default purge delay"
    );
    let root = tempfile::tempdir().unwrap();
    let template = root.path().join("template");
    build_template(&template);
    let paths: Vec<PathBuf> = (0..STORES)
        .map(|i| {
            let path = root.path().join(format!("store-{i}"));
            copy_store(&template, &path);
            path
        })
        .collect();

    let slots: Vec<Mutex<Option<Database>>> = paths
        .iter()
        .map(|path| {
            let db = open(path);
            Mutex::new(Some(db))
        })
        .collect();
    let held: u64 = slots
        .iter()
        .map(|slot| check(slot.lock().unwrap().as_ref().unwrap()))
        .sum();
    assert!(
        held >= MIN_HELD_BYTES,
        "held {held} bytes, below the {MIN_HELD_BYTES} the gate needs"
    );

    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            let (paths, slots) = (&paths, &slots);
            scope.spawn(move || {
                for _ in 0..ROUNDS {
                    for i in (thread..STORES).step_by(THREADS) {
                        drop(slots[i].lock().unwrap().take());
                        let db = open(&paths[i]);
                        check(&db);
                        *slots[i].lock().unwrap() = Some(db);
                    }
                }
            });
        }
    });

    let peak = process_peak_rss_bytes().expect("no process RSS sampler on this platform");
    eprintln!(
        "peak_rss_bytes={peak} held_steady_state_bytes={held} ratio={:.3}",
        peak as f64 / held as f64
    );
    assert!(
        peak <= 2 * held,
        "peak RSS {peak} exceeds twice the held steady state {held}"
    );
}
