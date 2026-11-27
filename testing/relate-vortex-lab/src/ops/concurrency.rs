//! Concurrency kernel: how four ways of sharing one decoded column scale with
//! reader threads (the re-measurement `concurrency-findings.md` asks for).
//!
//! Each arm caches the same canonical `Vec<i64>` (attr0, 150k rows, 1.2 MB) and
//! is read by N barrier-synchronized threads:
//!
//! 1. **lock-free** — one `OnceLock<Arc<Vec<i64>>>`: hit = one acquire load;
//!    a cold race decodes once (the store's adopted design).
//! 2. **`RwLock<Option<Arc<Vec>>>`** — readers share the lock; a cold race
//!    double-checks.
//! 3. **`Mutex<Option<Arc<Vec>>>`** — every reader serializes.
//! 4. **per-thread** — each worker owns a full copy: no sharing, N × resident.
//!
//! Per arm: hit ns/op for point and 64-row batch reads at 1, 2, 4, 8 threads,
//! the cold-cache decode count under an 8-thread stampede, and resident bytes
//! (the per-thread arm priced at N × the single cache). Every section prints
//! the load stamp: the box is shared, so a bare ns/op is not evidence.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, OnceLock, RwLock};
use std::time::Instant;

use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::VortexSessionExecute;
use vortex::session::VortexSession;

use crate::bench::{live_bytes, loadavg1, spread_row};
use crate::store;

/// The rows and the batch width the point/batch arms read.
const NODES: usize = 150_000;
const BATCH: usize = 64;
/// The thread counts the contention table reports.
const THREADS: [usize; 4] = [1, 2, 4, 8];

pub fn run(out_dir: &Path, iters: u64) -> Result<(), Box<dyn std::error::Error>> {
    let table = store::synthetic("mono", NODES, 4);
    let path = out_dir.join("concurrency.vortex");
    store::write(&path, &table)?;
    let (session, rt, vfile) = store::open_mmap(&path)?;
    let column = store::projected(&rt, &vfile, "attr0")?;
    let rows = column.len() as u64;
    let point_iters = iters;
    let batch_iters = (iters / 16).max(100);

    println!(
        "== concurrency kernel: attr0, {rows} rows, {} B single cache; load {:.1} ==",
        rows * 8,
        loadavg1()
    );

    // ---- arm 1: lock-free fill-once ----
    let counter = AtomicUsize::new(0);
    let cache: OnceLock<Arc<Vec<i64>>> = OnceLock::new();
    let before = live_bytes();
    cold_stampede(|| {
        cache.get_or_init(|| decode(&column, &session, &counter));
    });
    let resident = live_bytes().saturating_sub(before);
    report(
        "lock-free OnceLock<Arc<Vec>>",
        resident,
        counter.load(Ordering::Relaxed),
        point_iters,
        batch_iters,
        |_, i| cache.get().expect("warmed")[spread_row(i, rows)],
        |_, i| batch_sum(cache.get().expect("warmed"), i, rows),
    );

    // ---- arm 2: RwLock<Option<Arc<Vec>>> ----
    let counter = AtomicUsize::new(0);
    let rw: RwLock<Option<Arc<Vec<i64>>>> = RwLock::new(None);
    let before = live_bytes();
    cold_stampede(|| {
        if rw.read().expect("rwlock").is_some() {
            return;
        }
        let mut write = rw.write().expect("rwlock");
        write.get_or_insert_with(|| decode(&column, &session, &counter));
    });
    let resident = live_bytes().saturating_sub(before);
    report(
        "RwLock<Option<Arc<Vec>>>",
        resident,
        counter.load(Ordering::Relaxed),
        point_iters,
        batch_iters,
        |_, i| {
            let guard = rw.read().expect("rwlock");
            guard.as_ref().expect("warmed")[spread_row(i, rows)]
        },
        |_, i| {
            let guard = rw.read().expect("rwlock");
            batch_sum(guard.as_ref().expect("warmed"), i, rows)
        },
    );

    // ---- arm 3: Mutex<Option<Arc<Vec>>> ----
    let counter = AtomicUsize::new(0);
    let mutex: Mutex<Option<Arc<Vec<i64>>>> = Mutex::new(None);
    let before = live_bytes();
    cold_stampede(|| {
        let mut guard = mutex.lock().expect("mutex");
        guard.get_or_insert_with(|| decode(&column, &session, &counter));
    });
    let resident = live_bytes().saturating_sub(before);
    report(
        "Mutex<Option<Arc<Vec>>>",
        resident,
        counter.load(Ordering::Relaxed),
        point_iters,
        batch_iters,
        |_, i| {
            let guard = mutex.lock().expect("mutex");
            guard.as_ref().expect("warmed")[spread_row(i, rows)]
        },
        |_, i| {
            let guard = mutex.lock().expect("mutex");
            batch_sum(guard.as_ref().expect("warmed"), i, rows)
        },
    );

    // ---- arm 4: per-thread caches (one full copy per worker) ----
    let before = live_bytes();
    let per_thread: Vec<Arc<Vec<i64>>> = (0..THREADS[THREADS.len() - 1])
        .map(|_| decode(&column, &session, &AtomicUsize::new(0)))
        .collect();
    let resident = live_bytes().saturating_sub(before);
    report(
        "per-thread cache (x N)",
        resident,
        per_thread.len(),
        point_iters,
        batch_iters,
        |tid, i| per_thread[tid][spread_row(i, rows)],
        |tid, i| batch_sum(&per_thread[tid], i, rows),
    );

    Ok(())
}

/// The whole column as one canonical `Vec<i64>`; counts the decode so a cold
/// stampede's duplicate work is visible.
fn decode(
    column: &vortex::array::ArrayRef,
    session: &VortexSession,
    count: &AtomicUsize,
) -> Arc<Vec<i64>> {
    count.fetch_add(1, Ordering::Relaxed);
    let mut ctx = session.create_execution_ctx();
    let decoded = column
        .clone()
        .execute::<PrimitiveArray>(&mut ctx)
        .expect("the lab's own column decodes");
    Arc::new(decoded.as_slice::<i64>().to_vec())
}

/// Race `threads` threads through `f` once each, all released together.
fn cold_stampede<F: Fn() + Sync>(f: F) {
    let threads = THREADS[THREADS.len() - 1];
    let barrier = Barrier::new(threads);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let barrier = &barrier;
            let f = &f;
            scope.spawn(move || {
                barrier.wait();
                f();
            });
        }
    });
}

/// Sum `BATCH` consecutive rows starting at a pseudo-random offset.
fn batch_sum(values: &[i64], i: u64, rows: u64) -> i64 {
    let start = spread_row(i, rows - BATCH as u64);
    values[start..start + BATCH].iter().sum()
}

/// One arm's row: point and batch hit ns/op at each thread count, plus its
/// cold decode count and resident bytes.
#[allow(clippy::too_many_arguments, reason = "one flat report row")]
fn report<P, B>(
    name: &str,
    resident: u64,
    cold_decodes: usize,
    point_iters: u64,
    batch_iters: u64,
    point: P,
    batch: B,
) where
    P: Fn(usize, u64) -> i64 + Sync,
    B: Fn(usize, u64) -> i64 + Sync,
{
    let point_ns: Vec<f64> = THREADS
        .iter()
        .map(|&t| measure(t, point_iters, &point))
        .collect();
    let batch_ns: Vec<f64> = THREADS
        .iter()
        .map(|&t| measure(t, batch_iters, &batch))
        .collect();
    println!(
        "{:<30} point {:>6.1} {:>6.1} {:>6.1} {:>6.1} ns | batch {:>7.1} {:>7.1} {:>7.1} {:>7.1} ns | cold decodes {:>2} | resident {:>9} B",
        name,
        point_ns[0],
        point_ns[1],
        point_ns[2],
        point_ns[3],
        batch_ns[0],
        batch_ns[1],
        batch_ns[2],
        batch_ns[3],
        cold_decodes,
        resident
    );
    println!(
        "  ({name}: threads {}; load {:.1} at end)",
        THREADS
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join("/"),
        loadavg1()
    );
}

/// Per-thread hit ns/op: `threads` barrier-synchronized workers each run
/// `iters` operations of `f`. Returns elapsed ns per operation across all
/// threads.
fn measure<F: Fn(usize, u64) -> i64 + Sync>(threads: usize, iters: u64, f: &F) -> f64 {
    let barrier = Barrier::new(threads);
    let t = Instant::now();
    std::thread::scope(|scope| {
        for tid in 0..threads {
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                for i in 0..iters {
                    std::hint::black_box(f(tid, i));
                }
            });
        }
    });
    t.elapsed().as_nanos() as f64 / (threads as u64 * iters) as f64
}
