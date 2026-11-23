//! The one read-tracking [`Vfs`]: logs the object opens a store makes, so a
//! suite can name exactly which shards a checkpoint read — and assert a clean
//! shard is never reopened. The store suites mount [`ReadLogVfs`] around the
//! backend under test; a fault-injecting wrapper that is already a `Vfs`
//! keeps the same [`ReadLog`] as a recorder and feeds it from its own `open`.

use relate_core::Result;
use relate_storage::{Vfs, VfsFile, VfsMeta, VfsOpen};
use std::sync::{Arc, Mutex};

/// What an open asked for, as far as the log's two views care.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Open {
    /// Opened for reading alone — the opens `read_opens`/`reads_of` count.
    Read,
    /// Opened with any write, create, append or truncate participation.
    Write,
}

/// The opens a store made, in order, so a test can name the shards read.
#[derive(Default)]
pub struct ReadLog {
    opens: Mutex<Vec<(String, Open)>>,
}

impl ReadLog {
    /// Record one open of `path`.
    pub fn record(&self, path: &str, kind: Open) {
        self.opens.lock().unwrap().push((path.to_owned(), kind));
    }

    /// Record one open of `path` from its read/write classification — the
    /// one home of the [`Open`] mapping. `reads_only` is the caller's
    /// [`VfsOpen::reads_only`], the one classification a recording wrapper
    /// consults.
    ///
    /// It takes the classified bit rather than a `&VfsOpen`: a crate's own
    /// unit tests compile the storage crate a second time beside this
    /// dependency build, so their `VfsOpen` is a different type than this
    /// one — the classification's home stays `VfsOpen::reads_only` there.
    pub fn record_open(&self, path: &str, reads_only: bool) {
        self.record(path, if reads_only { Open::Read } else { Open::Write });
    }

    /// Forget every open recorded so far.
    pub fn clear(&self) {
        self.opens.lock().unwrap().clear();
    }

    /// The recorded opens `pred` accepts, in order — the one lock-taking
    /// read the accessors below compose.
    fn matching(&self, pred: impl Fn(&(String, Open)) -> bool) -> Vec<(String, Open)> {
        self.opens
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| pred(entry))
            .cloned()
            .collect()
    }

    /// How many opens of `path` were recorded, of any kind.
    pub fn opens(&self, path: &str) -> usize {
        self.matching(|(opened, _)| opened == path).len()
    }

    /// How many read-only opens of `path` were recorded.
    pub fn read_opens(&self, path: &str) -> usize {
        self.matching(|(opened, kind)| opened == path && *kind == Open::Read)
            .len()
    }

    /// The distinct objects under `prefix` opened for reading, sorted.
    pub fn reads_of(&self, prefix: &str) -> Vec<String> {
        let mut reads: Vec<String> = self
            .matching(|(opened, kind)| opened.starts_with(prefix) && *kind == Open::Read)
            .into_iter()
            .map(|(opened, _)| opened)
            .collect();
        reads.sort();
        reads.dedup();
        reads
    }
}

/// A [`Vfs`] mounting `inner` behind a [`ReadLog`]: every open is recorded
/// before it reaches the backend, everything else delegates.
pub struct ReadLogVfs {
    inner: Arc<dyn Vfs>,
    log: Arc<ReadLog>,
}

impl ReadLogVfs {
    pub fn new(inner: Arc<dyn Vfs>) -> Self {
        Self {
            inner,
            log: Arc::new(ReadLog::default()),
        }
    }

    /// The log this wrapper fills; the caller asserts against it.
    pub fn log(&self) -> &ReadLog {
        &self.log
    }
}

impl Vfs for ReadLogVfs {
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    fn create_dir_all(&self, path: &str) -> Result<()> {
        self.inner.create_dir_all(path)
    }
    fn create_dir_all_for_atomic(&self, path: &str) -> Result<()> {
        self.inner.create_dir_all_for_atomic(path)
    }
    fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        self.inner.list_dir(path)
    }
    fn exists(&self, path: &str) -> Result<bool> {
        self.inner.exists(path)
    }
    fn metadata(&self, path: &str) -> Result<VfsMeta> {
        self.inner.metadata(path)
    }
    fn open(&self, path: &str, opts: VfsOpen) -> Result<Box<dyn VfsFile>> {
        self.log.record_open(path, opts.reads_only());
        self.inner.open(path, opts)
    }
    fn open_exclusive(&self, path: &str) -> Result<Option<Box<dyn VfsFile>>> {
        // An exclusive create carries no options and is always a write open;
        // record it so every generated immutable object is in the log.
        self.log.record(path, Open::Write);
        self.inner.open_exclusive(path)
    }
    fn remove_file(&self, path: &str) -> Result<()> {
        self.inner.remove_file(path)
    }
    fn remove_files(&self, paths: &[String]) -> Result<()> {
        self.inner.remove_files(paths)
    }
    fn remove_dir_all(&self, path: &str) -> Result<()> {
        self.inner.remove_dir_all(path)
    }
    fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.inner.rename(from, to)
    }
    fn sync_dir(&self, path: &str) -> Result<()> {
        self.inner.sync_dir(path)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn take_writer_lease(&self) -> Option<relate_storage::WriterLease> {
        self.inner.take_writer_lease()
    }
}
