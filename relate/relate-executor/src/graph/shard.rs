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

use relate_core::{
    current_load_threads, record_transient_lease, run_load_tasks, NodeId, TransientLease,
};

use super::rebuild::RelRow;

/// Rows a shard must carry to be worth a thread. Below it the rebuild
/// runs on the calling thread, which is what keeps the sharded and
/// serial paths one code path: a single shard *is* the serial walk.
const MIN_ROWS_PER_SHARD: usize = 20_000;

/// How many shards a partition over `rows` rows is cut into — one per
/// load thread of the surrounding load scope (one outside any scope),
/// capped by the row count so a small graph stays on the calling thread.
/// Private: [`shard_rows`] mints it, so no caller ever holds a count,
/// only the partition it produced.
fn rebuild_workers(rows: usize) -> usize {
    #[cfg(test)]
    if let Some(forced) = forced_workers() {
        return forced;
    }
    (rows / MIN_ROWS_PER_SHARD).clamp(1, current_load_threads().get())
}

/// A partition of row indices into shards, stored CSR-style: every
/// shard's indices sit back to back in one buffer, and `offsets` marks
/// where each shard starts (`offsets.len() == shards + 1`). One
/// allocation of exactly one index per row, instead of a growing `Vec`
/// per shard.
pub(super) struct ShardPartition {
    indices: Vec<usize>,
    offsets: Vec<usize>,
}

impl ShardPartition {
    /// The shards as disjoint mutable slices, in shard order.
    fn shards_mut(&mut self) -> Vec<&mut [usize]> {
        let mut rest = self.indices.as_mut_slice();
        self.offsets
            .windows(2)
            .map(|bounds| {
                let (shard, tail) = std::mem::take(&mut rest).split_at_mut(bounds[1] - bounds[0]);
                rest = tail;
                shard
            })
            .collect()
    }

    /// Charge the partition's buffers to the open's transient tracker
    /// until the returned lease drops; none outside a load scope.
    pub(super) fn lease(&self) -> Option<TransientLease> {
        let words = self.indices.capacity() + self.offsets.capacity();
        record_transient_lease((words * std::mem::size_of::<usize>()) as u64)
    }
}

/// Split `rows` into shards of row indices keyed by `endpoint`: a
/// counting pass sizes every shard, a second pass places each index, so
/// the one index buffer is allocated at its final size. Indices rather
/// than rows: a shard reads its rows through the shared buffer, so the
/// rebuild's per-row footprint stays the one row buffer instead of one
/// per shard. The shard count is minted here ([`rebuild_workers`], which
/// never returns zero), so a caller passes the rows and the key and holds
/// no count at all — the two sides of a build therefore cut the same
/// number of shards without coordinating.
pub(super) fn shard_rows(rows: &[RelRow], endpoint: impl Fn(&RelRow) -> NodeId) -> ShardPartition {
    let workers = rebuild_workers(rows.len());
    let bucket = |row: &RelRow| (endpoint(row).unsigned_abs() % workers as u64) as usize;
    let mut offsets = vec![0usize; workers + 1];
    for row in rows {
        offsets[bucket(row) + 1] += 1;
    }
    for shard in 0..workers {
        offsets[shard + 1] += offsets[shard];
    }
    let mut next = offsets[..workers].to_vec();
    let mut indices = vec![0usize; rows.len()];
    for (index, row) in rows.iter().enumerate() {
        let shard = bucket(row);
        indices[next[shard]] = index;
        next[shard] += 1;
    }
    ShardPartition { indices, offsets }
}

/// Run `f` over each shard of the one partition shape every sharded
/// phase reads — row-index slices — on the shared load pool when there is
/// more than one, and return the results in shard order. One shard (or a
/// call outside a load scope) runs on the calling thread. A shard is
/// handed out mutably so a phase may reorder its own indices in place;
/// the partition is shared by every start-keyed consumer of the rebuild,
/// so a phase may only rely on a shard's *set* of indices, never its
/// order.
///
/// **Panic contract.** A panicking shard is the rebuild's own bug, never a
/// recoverable condition: the pool finishes every shard, then re-raises the
/// first shard's original payload on the calling thread, and no result
/// vector is returned — so a caller that installs only from the returned
/// parts (every sharded phase does) installs nothing from a failed run.
/// The contract is fail-stop, not rollback: nothing in the engine catches
/// the unwind, and a rebuild that dies has already cleared the maps it was
/// refilling (`rebuild_adj` clears, then builds), so the graph that ran it
/// is dropped with the failed open or aborting write, never reused. The
/// partition is borrowed, not consumed, for the same reason: the
/// start-keyed one serves every start-keyed consumer of the rebuild.
pub(super) fn run_shards<R: Send>(
    partition: &mut ShardPartition,
    f: impl Fn(&mut [usize]) -> R + Sync,
) -> Vec<R> {
    let f = &f;
    let tasks: Vec<_> = partition
        .shards_mut()
        .into_iter()
        .map(|shard| {
            move || {
                let result = f(shard);
                #[cfg(test)]
                let result = (result, std::thread::current().id());
                result
            }
        })
        .collect();
    let results = run_load_tasks(tasks);
    #[cfg(test)]
    let results = {
        let (results, threads): (Vec<R>, Vec<_>) = results.into_iter().unzip();
        SHARD_THREADS.with(|ran| ran.borrow_mut().push(threads));
        results
    };
    results
}

#[cfg(test)]
thread_local! {
    /// The worker count a test pins. The only way a unit test reaches the
    /// sharded path: its row counts are orders of magnitude under
    /// `MIN_ROWS_PER_SHARD`, so the natural count is always 1.
    static FORCED_WORKERS: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };

    /// The threads each [`run_shards`] call on this thread ran its shards
    /// on, one entry per call since the last take. A list, not the last
    /// call's: one rebuild makes four `run_shards` calls — both adjacency
    /// sides, the degree fold, the count fold — so a last-write cell could
    /// only ever speak for the count fold, and the tests that pin the
    /// sharded rebuild have to speak for all four.
    static SHARD_THREADS: std::cell::RefCell<Vec<Vec<std::thread::ThreadId>>> =
        const { std::cell::RefCell::new(Vec::new()) };
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

/// Take the threads of every [`run_shards`] call on this thread since the
/// last take, resetting the record.
#[cfg(test)]
pub(super) fn take_shard_threads() -> Vec<Vec<std::thread::ThreadId>> {
    SHARD_THREADS.with(|ran| std::mem::take(&mut *ran.borrow_mut()))
}

/// Take this thread's accumulated [`run_shards`] shard count, resetting it.
#[cfg(test)]
pub(super) fn take_shards_run() -> usize {
    take_shard_threads().iter().map(Vec::len).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partition(shards: &[&[usize]]) -> ShardPartition {
        let mut offsets = vec![0];
        for shard in shards {
            offsets.push(offsets.last().unwrap() + shard.len());
        }
        ShardPartition {
            indices: shards.concat(),
            offsets,
        }
    }

    /// The CSR partition holds every row index exactly once, in the shard
    /// its endpoint selects, in one buffer allocated at its final size.
    #[test]
    fn csr_partition_places_every_row_once_in_exact_capacity() {
        let rows: Vec<RelRow> = (0..10)
            .map(|i| RelRow {
                id: i,
                start: i % 4,
                type_id: 0,
                end: 0,
                ts: None,
            })
            .collect();
        let mut shards = with_workers(3, || shard_rows(&rows, |row| row.start));
        assert_eq!(shards.indices.capacity(), rows.len());
        assert_eq!(shards.offsets.len(), 4);
        let parts = shards.shards_mut();
        assert_eq!(parts.iter().map(|part| part.len()).sum::<usize>(), 10);
        for (bucket, part) in parts.iter().enumerate() {
            assert!(part
                .iter()
                .all(|&index| rows[index].start as usize % 3 == bucket));
        }
    }

    /// A panicking shard re-raises its original payload on the calling
    /// thread (not a wrapper), and no partial result escapes.
    #[test]
    fn a_panicking_shard_resurfaces_its_own_payload() {
        let mut shards = partition(&[&[0], &[1], &[2]]);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_shards(&mut shards, |shard| {
                if shard[0] == 1 {
                    panic!("shard one failed");
                }
                shard[0]
            })
        }));
        let payload = caught.expect_err("the failed shard must propagate");
        assert_eq!(
            payload.downcast_ref::<&str>().copied(),
            Some("shard one failed")
        );
    }

    /// The serial path (one shard) is the same code path: its panic
    /// propagates untouched too.
    #[test]
    fn a_single_shard_panic_propagates_on_the_calling_thread() {
        let mut shards = partition(&[&[0]]);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_shards(&mut shards, |_| -> usize { panic!("only") })
        }));
        assert!(caught.is_err());
    }
}
