//! The vault filesystem walk (ADR-0406 §4): the one traversal that
//! classifies vault entries, refuses symlinked entries, and derives the
//! guard stats — plus the shared `vault_matches` predicate and its
//! stat-first form. The walk's byte-fetching half — the serial and
//! striped read pass — lives in [`read`].
//!
//! Extracted from `mod.rs` (the no-1k-line rule, LABS-311).

use std::collections::BTreeMap;

#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Condvar, Mutex};

use relate_markdown as md;

use super::manifest::{FileStat, VaultManifest};
use crate::vfs::{read_to_vec, virt_join, Vfs, VfsMeta};
use relate_core::Result;

/// One walked `.md` entry: its path and the stat the walk's own
/// metadata fetch captured — the bytes-free shape [`walk_vault`] reads
/// over. Clone: the read pass carries each walked entry into its
/// [`VaultFile`] by clone, so the model's field list lives in this one
/// home.
#[derive(Clone)]
pub(super) struct WalkedPath {
    pub(super) path: String,
    /// The entry's stat as the walk saw it — length, mtime, shape —
    /// carried so the guard's stat-first comparison (ADR-0406 §5)
    /// consults the walk's own fetch instead of a second stat pass.
    pub(super) meta: VfsMeta,
}

impl WalkedPath {
    /// The file's name stem: the on-disk path minus `.md` (nested key
    /// paths keep their `/` separators) — the *filename* fact, distinct
    /// from the `Label/name` vault key load derives from it.
    pub(super) fn stem(&self) -> &str {
        self.path
            .strip_suffix(".md")
            .expect("walked vault paths end in .md")
    }

    /// A root file — loaded under the implicit `:Vault` label
    /// (ADR-0406 §2): precisely a vault-relative path holding no `/`.
    /// The fact is asked of the path, never a flag stored beside it.
    pub(super) fn root_file(&self) -> bool {
        !self.path.contains('/')
    }
}

/// One `.md` file in the vault: the walked entry (path, stat, root
/// classification — the walked model's one field list) plus its bytes.
pub(super) struct VaultFile {
    pub(super) walked: WalkedPath,
    pub(super) bytes: Vec<u8>,
}

/// Every guarded `.md` entry, root files flagged: the entry point over
/// the one traversal the platform takes — the concurrent walk on
/// native targets ([`walk_paths_concurrent`]), the serial recursion on
/// wasm32 ([`walk_dir`]) — plus the global path sort. Both traversals
/// share the one entry enumeration ([`walked_entries`]: sorted names,
/// hidden skipped, symlinks refused) and derive root classification at
/// the push site.
pub(super) fn walk_paths(vfs: &dyn Vfs) -> Result<Vec<WalkedPath>> {
    // The sort is the entry's own, once: both platform arms return
    // traversal order (the concurrent pass's merge order is whatever
    // the workers finished in; the serial recursion's is
    // parent-then-siblings), and the entry imposes the one global
    // path order every consumer reads.
    let mut paths = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            walk_paths_concurrent(vfs, walk_workers())?
        }
        #[cfg(target_arch = "wasm32")]
        {
            let mut paths = Vec::new();
            walk_dir(vfs, "", &mut paths)?;
            paths
        }
    };
    paths.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(paths)
}

/// The walk's one file-admission rule — the predicate both platform
/// traversals consult at their push site: a reserved generated-view
/// filename (judged through the case fold, so `Index.md` is the view
/// the same way `index.md` is) or any non-`.md` entry is not a node
/// file. One spelling, cfg-independent, the way `walked_entries`
/// owns the hidden/symlink/sorted rules.
/// The reserved-view name class, over the vault walk's own
/// `file_name` extractor — the one spelling of the predicate, consulted
/// by the node-file filter, the view walk, and the checkpoint's
/// deletion-phase pruning.
pub(super) fn path_is_view(path: &str) -> bool {
    md::is_reserved_view_file_name(file_name(path))
}

fn is_node_file(entry: &str) -> bool {
    !path_is_view(entry) && entry.ends_with(".md")
}

/// The vault I/O worker ceiling — the one policy the concurrent walk
/// and the striped read ([`read`]) both consult. Both passes are
/// syscall-bound (one stat or open/read chain per entry or file), so
/// the kernel, not the core count, is the wall: warm, eight
/// concurrent workers measured no better than four and sixteen
/// measurably worse; cold, what matters is queue depth against
/// per-file disk latency, and eight keeps the warm passes at their
/// floor while the cold ones get the depth.
#[cfg(not(target_arch = "wasm32"))]
pub(super) const IO_WORKER_CAP: usize = 8;

/// The concurrent walk's worker count: the ceiling above, never more
/// than the available cores.
#[cfg(not(target_arch = "wasm32"))]
fn walk_workers() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, IO_WORKER_CAP)
}

/// The concurrent traversal: a shared queue of unvisited directories,
/// one worker per [`IO_WORKER_CAP`] slot, each worker serially
/// classifying what it pops (the walk's own `walked_entries` — sorted
/// names, hidden skipped, symlinks refused — never a parallel copy).
/// Cold opens are latency-bound — every entry's stat is a disk round
/// trip — so the win is the queue depth, not the CPU: on a warm cache
/// the pool merely matches the serial pass.
///
/// Determinism is preserved structurally: the entry globally sorts the
/// merged file set whatever order the workers found it in, and the
/// error verdict selects the minimum directory under the DFS visit
/// order — the exact directory the serial depth-first pass would have
/// stopped at.
#[cfg(not(target_arch = "wasm32"))]
fn walk_paths_concurrent(vfs: &dyn Vfs, workers: usize) -> Result<Vec<WalkedPath>> {
    // Only the queue needs sharing: each worker owns what it walked,
    // and its joined handle hands the locals back — no shared output
    // buffers, no per-item locking.
    let queue = WalkQueue::new();

    let (paths, first) = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut walked: Vec<WalkedPath> = Vec::new();
                    // The worker's own first failure under the serial
                    // pass's visit order — one answer, folded at the
                    // join, never a collected Vec of failures the
                    // parent then re-scans.
                    let mut failed: Option<(String, relate_core::Error)> = None;
                    while let Some(dir) = queue.pop_dir() {
                        // The guard owns this directory's completion —
                        // the one the success path arms with children
                        // and the failure and panic paths drop as-is.
                        let mut popped = PoppedDir {
                            queue: &queue,
                            children: Vec::new(),
                        };
                        match walked_entries(vfs, &dir) {
                            Ok(entries) => {
                                let (subdirs, files) = classify(entries);
                                popped.adopt(subdirs);
                                walked.extend(files);
                            }
                            Err(e) => {
                                if failed
                                    .as_ref()
                                    .is_none_or(|(kept, _)| dfs_before(&dir, kept))
                                {
                                    failed = Some((dir, e));
                                }
                            }
                        }
                    }
                    (walked, failed)
                })
            })
            .collect();
        let mut paths = Vec::new();
        let mut first: Option<(String, relate_core::Error)> = None;
        // A panicked worker surfaces at its own join — exactly where
        // the scope would have propagated it — never through a
        // poisoned shared buffer. Each worker arrives with its own
        // visit-order-first failure; the fold keeps the least of
        // them under the same comparison.
        for handle in handles {
            let (walked, failed) = crate::codec::join_or_resume(handle);
            paths.extend(walked);
            if let Some((dir, e)) = failed {
                if first
                    .as_ref()
                    .is_none_or(|(kept, _)| dfs_before(&dir, kept))
                {
                    first = Some((dir, e));
                }
            }
        }
        (paths, first)
    });

    // The serial pass stops at the first failing directory in its
    // depth-first order; the concurrent pass answers with the same
    // one: each worker's first failure under that order, folded at
    // the joins into the least of them.
    if let Some((_, first)) = first {
        return Err(first);
    }
    // Merge order, deliberately unsorted: `walk_paths` — this
    // function's only caller — owns the one global path sort.
    Ok(paths)
}

/// The serial pass's depth-first preorder over sorted children, as a
/// comparison of failing directory paths: segment-lexicographic
/// (`/`-split), a parent sorting before its descendants — NOT plain
/// path order, which would put the sibling `a-b` before the subtree
/// `a/` the recursion visits first.
#[cfg(not(target_arch = "wasm32"))]
fn dfs_before(a: &str, b: &str) -> bool {
    a.split('/').lt(b.split('/'))
}

/// The walk's shared work queue: the unvisited directories and the
/// in-flight count, under the one lock and the one condition
/// variable. `pending` counts pushes minus completions, so it reaches
/// zero exactly when the traversal is over (the root starts as the
/// one pending push). A pusher notifies, and the completion that
/// takes pending to zero wakes every waiter — idle workers sleep on
/// the condvar instead of spinning against the mutex the busy workers
/// need, and the whole termination argument lives here, beside the
/// arithmetic that maintains it.
///
/// The lock policy: the queue is held only across a pop or pure
/// arithmetic — never across I/O — so a poisoned lock can only
/// follow a worker panic, which [`PoppedDir`]'s completion guard
/// converts into a drain: the pool empties, the joins return, and
/// the panic propagates at its own join. The `expect`s are that
/// invariant.
#[cfg(not(target_arch = "wasm32"))]
struct WalkQueue {
    slots: Mutex<(Vec<String>, usize)>,
    work: Condvar,
}

#[cfg(not(target_arch = "wasm32"))]
impl WalkQueue {
    /// The seeded queue: the vault root, one push in flight.
    fn new() -> Self {
        Self {
            slots: Mutex::new((vec![String::new()], 1)),
            work: Condvar::new(),
        }
    }

    /// The next unvisited directory, sleeping while other workers
    /// hold in-flight ones. `None` is the traversal's end: the queue
    /// is empty and nothing is in flight.
    fn pop_dir(&self) -> Option<String> {
        let mut slots = self.slots.lock().expect("the walk queue lock");
        loop {
            if let Some(dir) = slots.0.pop() {
                return Some(dir);
            }
            if slots.1 == 0 {
                return None;
            }
            // Another worker holds an in-flight directory; its
            // children (or its completion) wake this one.
            slots = self
                .work
                .wait(slots)
                .expect("the walk queue lock survives a wake");
        }
    }

    /// The one completion: account for `children` joining the queue
    /// and the completed directory leaving the flight, waking every
    /// waiter when the traversal ends or new work arrived. Both the
    /// success and failure paths run it — through [`PoppedDir`]'s
    /// drop, with and without children.
    fn complete(&self, children: Vec<String>) {
        let mut slots = self.slots.lock().expect("the walk queue lock");
        let pushed = children.len();
        slots.0.extend(children);
        slots.1 += pushed;
        slots.1 -= 1;
        if slots.1 == 0 || pushed > 0 {
            self.work.notify_all();
        }
    }
}

/// One popped directory's completion guard: the popper owes the queue
/// exactly one completion, whether the directory's work succeeds,
/// fails, or panics — the guard holds the children to add at that
/// completion, so a worker that panics mid-directory still completes
/// it (with no children) and the pool drains, the joins return, and
/// the panic propagates at its own join, instead of parking every
/// waiter on a `pending` that never reaches zero: the open fails, it
/// never hangs.
#[cfg(not(target_arch = "wasm32"))]
struct PoppedDir<'a> {
    queue: &'a WalkQueue,
    children: Vec<String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl PoppedDir<'_> {
    /// Record the completed directory's subdirectories — they join
    /// the queue at the guard's drop.
    fn adopt(&mut self, children: Vec<String>) {
        self.children = children;
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for PoppedDir<'_> {
    fn drop(&mut self) {
        self.queue.complete(std::mem::take(&mut self.children));
    }
}

/// One directory's classification: its subdirectories and its node
/// files over the one entry enumeration's sorted order — the shared
/// tail of both traversals' per-directory step; each walker keeps only
/// its own enumeration shape.
fn classify(entries: Vec<(String, VfsMeta)>) -> (Vec<String>, Vec<WalkedPath>) {
    let mut subdirs = Vec::new();
    let mut files = Vec::new();
    for (entry, meta) in entries {
        if meta.is_dir() {
            subdirs.push(entry);
        } else if is_node_file(&entry) {
            files.push(WalkedPath { path: entry, meta });
        }
    }
    (subdirs, files)
}

/// Read every walked entry. The one read pass — no other vault walk
/// fetches bytes. The serial and striped implementations live in
/// [`read`]; this entry owns the ordering contract they fold under.
pub(super) fn walk_vault(vfs: &dyn Vfs) -> Result<Vec<VaultFile>> {
    let paths = walk_paths(vfs)?;
    read::read_walked(vfs, &paths)
}

/// The walk's byte-fetching half — the serial and striped read pass
/// (ADR-0406 §4). Visible within the codec subtree for the test
/// seams; the pass itself is the walk's own.
pub(in crate::codec::markdown) mod read;

/// The path's final segment — the whole path at the vault root.
pub(super) fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// The one entry enumeration both traversals consume: names sorted for
/// deterministic order, hidden entries skipped, symlinked entries
/// refused — yielding each surviving entry with its stat, so every
/// walker keeps only its own tail (node classification or directory
/// pruning) and the classification conditionals exist once.
pub(super) fn walked_entries(vfs: &dyn Vfs, dir: &str) -> Result<Vec<(String, VfsMeta)>> {
    let mut names = vfs.list_dir(dir)?;
    names.sort();
    let mut entries = Vec::new();
    for name in names {
        let entry = virt_join(dir, &name);
        if is_hidden(&entry) {
            continue;
        }
        let meta = refuse_symlink(vfs, &entry)?;
        // A C0/DEL control byte in a NODE file's name is not vault
        // content: the name grammar's own set (the sanitizer's),
        // refused at the walk — such a name can never round-trip
        // (sanitize replaces the bytes on every write), so it fails
        // closed like the symlink and FIFO shapes rather than loading
        // as a node whose key no value can ever match. Scoped to the
        // entries the walk would load: a foreign non-`.md` name the
        // engine never reads (macOS custom-icon `Icon\r` files,
        // extraction residue) is skipped like any other non-node
        // entry, never fatal to the walk.
        if is_node_file(&entry) && file_name(&entry).chars().any(md::is_c0_del) {
            return Err(md::vault_error(format!(
                "vault entry {entry} carries a control byte in its name; \
                 a markdown vault holds printable names only"
            )));
        }
        entries.push((entry, meta));
    }
    Ok(entries)
}

/// Every generated-view file present on disk (the reserved names at
/// any depth, over the same one entry enumeration) — the view class
/// the checkpoint's deletion phase diffs against the views it
/// rendered, so a stale view is deleted by class membership, never by
/// re-deriving label directories from the guarded manifest.
pub(super) fn walk_view_paths(vfs: &dyn Vfs) -> Result<Vec<String>> {
    let mut views = Vec::new();
    walk_views(vfs, "", &mut views)?;
    Ok(views)
}

fn walk_views(vfs: &dyn Vfs, dir: &str, views: &mut Vec<String>) -> Result<()> {
    for (entry, meta) in walked_entries(vfs, dir)? {
        if meta.is_dir() {
            walk_views(vfs, &entry, views)?;
            continue;
        }
        if path_is_view(&entry) {
            views.push(entry);
        }
    }
    Ok(())
}

/// The one recursive traversal (the wasm32 fallback; native targets
/// run the concurrent traversal above over the same
/// [`walked_entries`]): every non-hidden, non-reserved `.md` is a node
/// file; the only root/label difference — whether the file loads under
/// the implicit `:Vault` label (ADR-0406 §2) — is derived where it is
/// pushed, never threaded as a mode flag.
#[cfg(target_arch = "wasm32")]
fn walk_dir(vfs: &dyn Vfs, dir: &str, paths: &mut Vec<WalkedPath>) -> Result<()> {
    let (subdirs, mut files) = classify(walked_entries(vfs, dir)?);
    paths.append(&mut files);
    for subdir in subdirs {
        walk_dir(vfs, &subdir, paths)?;
    }
    Ok(())
}

/// A vault is a hand-authored folder of plain files and directories; a
/// symlinked entry would redirect the store's read, write, and delete
/// surface outside the vault root, so it is refused before any of the
/// three can reach it.
fn refuse_symlink(vfs: &dyn Vfs, entry: &str) -> Result<VfsMeta> {
    let meta = vfs.metadata(entry)?;
    if meta.is_symlink() {
        return Err(md::vault_error(format!(
            "vault entry {entry} is a symlink; \
             a markdown vault refuses symlinked entries"
        )));
    }
    // A FIFO, socket, or device entry is not vault content either —
    // refused at the walk with its own shape named, never deferred to
    // a later open's regular-file check.
    if meta.is_other() {
        return Err(md::vault_error(format!(
            "vault entry {entry} is not a plain file or directory; \
             a markdown vault holds only those"
        )));
    }
    Ok(meta)
}

fn is_hidden(path: &str) -> bool {
    file_name(path).starts_with('.')
}

pub(super) fn crc(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// ADR-0406 §5's one predicate — the vault's guarded files match the
/// manifest's expectation — consulted by the checkpoint external-write
/// guard and journal recovery, so both roles provably share one
/// definition. Every consumer lives inside the `codec::markdown`
/// subtree, so the grade is `pub(super)`, matching the sibling walk and
/// stat helpers.
pub(super) fn vault_matches(stats: &BTreeMap<String, FileStat>, manifest: &VaultManifest) -> bool {
    stats == &manifest.files
}

/// The stat-first form of the same predicate (ADR-0406 §5): a file
/// whose recorded length and mtime both match is unchanged without
/// reading a byte; only when the VFS reports no mtime (or the recorded
/// one is absent) does the comparison fall back to the crc.
pub(super) fn vault_matches_stat_first(vfs: &dyn Vfs, manifest: &VaultManifest) -> Result<bool> {
    // The file SET must match first: a file added to the vault shifts
    // every path-order node id, so replaying id-based redo over it is
    // the exact corruption ADR-0406 §5 refuses — the paths-only walk
    // detects it without reading a byte. Both sides are BTreeMap keys,
    // so the sorted iterators compare directly.
    let walked: BTreeMap<String, VfsMeta> = walk_paths(vfs)?
        .into_iter()
        .map(|walked| (walked.path, walked.meta))
        .collect();
    if walked.keys().ne(manifest.files.keys()) {
        return Ok(false);
    }
    for (path, stat) in &manifest.files {
        // The walk's own stat is the single classification source: a
        // file missing from the vault already showed up as the set
        // mismatch above (a legitimate "changed", never the guard's
        // error), and any other I/O failure is the walk's own error.
        let meta = walked
            .get(path)
            .expect("the set check proved every manifest path walked");
        if meta.len != stat.len {
            return Ok(false);
        }
        // The mtime is advisory, never a verdict: a mismatch — a fresh
        // write of identical bytes — falls through to the crc, the one
        // content-based definition of "changed" (FileStat::eq), shared
        // with the backends that report no mtime at all.
        if let (Some(recorded), Some(current)) = (stat.mtime, meta.mtime) {
            if recorded == current {
                continue;
            }
        }
        // No mtime to consult — or a mismatch to prove: the crc.
        let bytes = read_to_vec(vfs, path)?;
        if stat.differs_from(&bytes) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Reserved views are excluded (regenerated wholesale, never guarded).
pub(super) fn vault_stats(files: &[VaultFile]) -> BTreeMap<String, FileStat> {
    files
        .iter()
        .map(|file| {
            (
                file.walked.path.clone(),
                FileStat {
                    mtime: file.walked.meta.mtime,
                    ..FileStat::of(&file.bytes)
                },
            )
        })
        .collect()
}

/// Remove label directories that no longer hold node files or views.
/// Only empty directories go — a directory with attachments
/// (non-markdown files) is vault content the engine does not own.
pub(super) fn prune_empty_dirs(vfs: &dyn Vfs) -> Result<()> {
    prune_dir(vfs, "")
}

/// The pruning traversal, post-order: each directory is considered only
/// after its children, so a nested directory emptied by its own pruning
/// goes too — deepest-first for free, with no collected-then-sorted
/// directory list. The emptiness proof is a fresh listing: hidden
/// entries are skipped by the enumeration but are vault content that
/// keeps their directory alive.
fn prune_dir(vfs: &dyn Vfs, dir: &str) -> Result<()> {
    for (entry, meta) in walked_entries(vfs, dir)? {
        if meta.is_dir() {
            prune_dir(vfs, &entry)?;
        }
    }
    // The vault root itself is never a removal candidate.
    if !dir.is_empty() && vfs.list_dir(dir)?.is_empty() {
        vfs.remove_dir_all(dir)
            .map_err(|e| md::vault_error(format!("removing empty directory {dir}: {e}")))?;
    }
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod queue_tests {
    use super::*;

    #[test]
    fn a_panicking_popper_still_completes_its_directory() {
        // The rust-review pin: a worker that panics mid-directory owes
        // the queue its completion anyway — PoppedDir pays it on the
        // unwind, so the pool drains and the panic propagates at the
        // join, instead of parking every waiter on a `pending` that
        // never reaches zero (the open hangs, never fails). Without
        // the guard, the pop_dir below parks forever: this test hangs,
        // not fails.
        let queue = WalkQueue::new();
        std::thread::scope(|scope| {
            let panicker = scope.spawn(|| {
                let _dir = queue.pop_dir().expect("the seeded root");
                let _guard = PoppedDir {
                    queue: &queue,
                    children: vec!["child".to_string()],
                };
                panic!("mid-directory");
            });
            assert!(panicker.join().is_err(), "the worker panicked as arranged");
            // The guard completed the panicking popper's directory —
            // child queued, waiters woken — so the traversal continues
            // and ends exactly as the protocol promises.
            assert_eq!(queue.pop_dir().as_deref(), Some("child"));
            queue.complete(Vec::new());
            assert_eq!(queue.pop_dir(), None);
        });
    }
}
