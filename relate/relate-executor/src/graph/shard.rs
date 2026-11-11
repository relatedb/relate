//! The wholesale rebuild's sharding kit — the worker count, the
//! partition, and the parallel run — one home for everything that
//! answers "how does the rebuild shard?", so the sharded phases
//! (`adjacency.rs`, `degree_stats.rs`, `count_state.rs`) and the
//! orchestrator (`rebuild.rs`) all read one module instead of each
//! spelling a half of the policy.
//!
//! Shards are keyed on a node endpoint, so a shard carries **every**
//! row of every node it owns. A shard's per-node result is therefore
//! the whole node's, and shard outputs merge without coordination —
//! no lock, no ordering contract, no cross-shard reconciliation.

use relate_core::NodeId;

use super::RelRow;

/// Rows a shard must carry to be worth a thread. Below it the rebuild
/// runs on the calling thread, which is what keeps the sharded and
/// serial paths one code path: a single shard *is* the serial walk.
const MIN_ROWS_PER_SHARD: usize = 20_000;

/// How many shards a partition over `rows` rows is cut into — one per
/// core, capped by the row count so a small graph stays on the calling
/// thread. Private: [`shard_rows`] mints it, so no caller ever holds a
/// count, only the partition it produced.
fn rebuild_workers(rows: usize) -> usize {
    #[cfg(test)]
    if let Some(forced) = forced_workers() {
        return forced;
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    (rows / MIN_ROWS_PER_SHARD).clamp(1, cores)
}

/// Split `rows` into shards of row indices, one pass, keyed by
/// `endpoint`. Indices rather than rows: a shard reads its rows through
/// the shared buffer, so the rebuild's per-row footprint stays the one
/// row buffer instead of one per shard. The shard count is minted here
/// ([`rebuild_workers`], which never returns zero), so a caller passes
/// the rows and the key and holds no count at all — the two sides of a
/// build therefore cut the same number of shards without coordinating.
pub(super) fn shard_rows(rows: &[RelRow], endpoint: impl Fn(&RelRow) -> NodeId) -> Vec<Vec<usize>> {
    let workers = rebuild_workers(rows.len());
    let mut shards: Vec<Vec<usize>> = vec![Vec::new(); workers];
    for (index, row) in rows.iter().enumerate() {
        let bucket = (endpoint(row).unsigned_abs() % workers as u64) as usize;
        shards[bucket].push(index);
    }
    shards
}

/// Run `f` over each shard of the one partition shape every sharded
/// phase reads — row-index lists — in parallel when there is more than
/// one, and return the results in shard order. One shard runs on the
/// calling thread. Shards are borrowed, not consumed: the start-keyed
/// partition is one allocation shared by every start-keyed consumer of
/// the rebuild, so no consumer may take it by value.
pub(super) fn run_shards<R: Send>(
    shards: &[Vec<usize>],
    f: impl Fn(&[usize]) -> R + Sync,
) -> Vec<R> {
    #[cfg(test)]
    SHARDS_RUN.with(|ran| ran.set(ran.get() + shards.len()));
    if shards.len() <= 1 {
        return shards.iter().map(|shard| f(shard)).collect();
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = shards
            .iter()
            .map(|shard| scope.spawn(|| f(shard)))
            .collect();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(result) => result,
                // A shard that panicked is the rebuild's own bug; re-raise
                // the original payload rather than a wrapper that loses it.
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    })
}

#[cfg(test)]
thread_local! {
    /// The worker count a test pins. The only way a unit test reaches the
    /// sharded path: its row counts are orders of magnitude under
    /// `MIN_ROWS_PER_SHARD`, so the natural count is always 1.
    static FORCED_WORKERS: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };

    /// Shards every [`run_shards`] on this thread has run since the last
    /// take. A sum, not the last call's count: one rebuild makes four
    /// `run_shards` calls — both adjacency sides, the degree fold, the
    /// count fold — so a last-write cell could only ever speak for the
    /// count fold, and the test that pins the sharded rebuild is the
    /// single-shard one has to be able to speak for all four.
    static SHARDS_RUN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn forced_workers() -> Option<usize> {
    FORCED_WORKERS.with(std::cell::Cell::get)
}

/// Run `f` with the rebuild pinned to `workers` shards, restoring the
/// previous pin afterwards (the override is process-global per thread, so a
/// test that leaks it would shard every later rebuild in the same thread).
#[cfg(test)]
pub(super) fn with_workers<R>(workers: usize, f: impl FnOnce() -> R) -> R {
    // The one seam that authors a count instead of minting it: assert
    // the invariant `rebuild_workers` guarantees and `shard_rows`
    // divides by, rather than letting a zero reach the modulo.
    assert!(workers > 0, "a shard count is non-zero");
    struct Reset(Option<usize>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FORCED_WORKERS.with(|pin| pin.set(self.0));
        }
    }
    let previous = FORCED_WORKERS.with(|pin| pin.replace(Some(workers)));
    let _reset = Reset(previous);
    f()
}

/// Take this thread's accumulated [`run_shards`] shard count, resetting it.
#[cfg(test)]
pub(super) fn take_shards_run() -> usize {
    SHARDS_RUN.with(|ran| ran.replace(0))
}
