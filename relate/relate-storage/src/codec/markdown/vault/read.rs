//! The vault read pass (ADR-0406 §4): the one place the walk's bytes
//! are fetched. Small vaults and wasm32 read serially on the current
//! thread; large vaults read on a few scoped workers, each owning a
//! contiguous stripe of the sorted paths — each stripe runs the serial
//! pass over its slice, so the fold is exactly the serial pass's:
//! same files, same order, and the first failing path in walk order
//! still fails the pass.

use super::{VaultFile, WalkedPath};
use crate::vfs::{read_to_vec, Vfs};
use relate_core::Result;

/// Read every walked path, in stripes when the vault is large enough.
pub(super) fn read_walked(vfs: &dyn Vfs, paths: &[WalkedPath]) -> Result<Vec<VaultFile>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let workers = read_workers(paths.len());
        if workers > 1 {
            return read_striped(vfs, paths, workers);
        }
    }
    read_serial(vfs, paths)
}

/// The fewest files that make one read worker worth its spawn.
#[cfg(not(target_arch = "wasm32"))]
const READ_STRIPE: usize = 16;

/// The concurrent vault read's worker count: up to the walk's one
/// I/O worker ceiling ([`super::IO_WORKER_CAP`]), never more than the
/// stripe count the vault sizes — a vault of fewer than two
/// `READ_STRIPE` stripes reads serially, so a small store never pays
/// a spawn.
#[cfg(not(target_arch = "wasm32"))]
fn read_workers(paths: usize) -> usize {
    #[cfg(test)]
    if let Some(forced) = forced_read_workers() {
        return forced;
    }
    let stripes = paths.div_ceil(READ_STRIPE);
    super::IO_WORKER_CAP.min(stripes).max(1)
}

/// The serial read pass — the one path wasm32 and small vaults take,
/// and the pass every stripe worker runs over its slice: the first
/// failing read fails the run, in path order.
fn read_serial(vfs: &dyn Vfs, paths: &[WalkedPath]) -> Result<Vec<VaultFile>> {
    let mut files = Vec::with_capacity(paths.len());
    for walked in paths {
        let bytes = read_to_vec(vfs, &walked.path)?;
        // The walked model's field list lives in its one home: the
        // entry rides into its file by clone, never re-listed here.
        files.push(VaultFile {
            walked: walked.clone(),
            bytes,
        });
    }
    Ok(files)
}

/// The striped read pass: `workers` scoped threads, each running the
/// serial pass over one contiguous stripe of the sorted paths.
/// Folding the stripes' `Result`s in order yields exactly the serial
/// pass's files, order and error precedence included — each stripe
/// reports only its own first failure, so the first failure across
/// the fold is the serial pass's first failing path.
#[cfg(not(target_arch = "wasm32"))]
fn read_striped(vfs: &dyn Vfs, paths: &[WalkedPath], workers: usize) -> Result<Vec<VaultFile>> {
    // An empty path list folds to one (empty) stripe — `chunks`
    // refuses a zero size, so the precondition is owned here, not
    // assumed of the caller.
    let stripe = paths.len().div_ceil(workers).max(1);
    let outcomes = std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .chunks(stripe)
            .map(|chunk| scope.spawn(move || read_serial(vfs, chunk)))
            .collect();
        handles
            .into_iter()
            .map(crate::codec::join_or_resume)
            .collect::<Vec<_>>()
    });
    // The seam's proof-of-run count is the stripes that actually ran —
    // `outcomes.len()`, the folded spawn count — never the requested
    // worker count, which a short vault never reaches (fewer files
    // than workers spawn fewer stripes).
    #[cfg(test)]
    READ_WORKERS_RUN.with(|spawned| spawned.set(spawned.get() + outcomes.len()));
    // The fold: stripes arrive in path order and each held its own
    // files in path order, so the files land in path order and the
    // first failure across the fold is the serial pass's first.
    let mut files = Vec::with_capacity(paths.len());
    for outcome in outcomes {
        let mut stripe = outcome?;
        files.append(&mut stripe);
    }
    debug_assert!(
        files
            .iter()
            .map(|file| file.walked.path.as_str())
            .eq(paths.iter().map(|walked| walked.path.as_str())),
        "the striped fold returns exactly the walked paths, in walk order"
    );
    Ok(files)
}

#[cfg(test)]
thread_local! {
    /// The worker count a test pins for the vault read. The only way a
    /// unit test reaches the striped path's count: the natural count is
    /// derived from the vault's size.
    static FORCED_READ_WORKERS: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };

    /// Striped vault reads on this thread — the forced-count seam's
    /// proof the parallel path actually ran.
    static READ_WORKERS_RUN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn forced_read_workers() -> Option<usize> {
    FORCED_READ_WORKERS.with(std::cell::Cell::get)
}

/// Run `f` with the vault read pinned to `workers` stripes, restoring
/// the previous pin afterwards — including on panic, so a failing test
/// never leaves the thread's count forced. The override mechanics are
/// the codec root's one [`crate::codec::with_test_override`].
#[cfg(test)]
pub(in crate::codec::markdown) fn with_read_workers<R>(workers: usize, f: impl FnOnce() -> R) -> R {
    crate::codec::with_test_override(&FORCED_READ_WORKERS, workers, f)
}

/// Take this thread's accumulated spawned-worker count, resetting it —
/// the proof a striped read actually ran.
#[cfg(test)]
pub(in crate::codec::markdown) fn take_read_workers() -> usize {
    READ_WORKERS_RUN.with(std::cell::Cell::take)
}
