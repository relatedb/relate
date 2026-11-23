//! The pause harness: a [`Vfs`] wrapper that holds the snapshot dump at its
//! first non-WAL write, so a test can prove what a commit does while a dump
//! is in flight, plus the helpers that arm, release and time it.

use super::*;

use relate_storage::{MemoryVfs, Vfs, VfsFile, VfsMeta, VfsOpen};
use std::io::SeekFrom;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Barrier, Mutex};

/// A [`Vfs`] wrapper that holds the snapshot dump at its first non-WAL write:
/// the write reports on `entered` and then waits on `release`. Non-WAL writes
/// are the dump's alone — WAL frames live under `wal/` — so a commit's own
/// append never trips the gate.
pub(super) struct DumpPause {
    pub(super) armed: AtomicBool,
    pub(super) entered: Mutex<Option<mpsc::Sender<()>>>,
    pub(super) release: Barrier,
}

impl DumpPause {
    /// A pause gate and the signal that a dump reached it. `armed` starts it
    /// armed; a protocol that arms the gate just before its checkpoint starts
    /// it unarmed.
    pub(super) fn new(armed: bool) -> (Arc<Self>, mpsc::Receiver<()>) {
        let (entered, dump_entered) = mpsc::channel();
        (
            Arc::new(Self {
                armed: AtomicBool::new(armed),
                entered: Mutex::new(Some(entered)),
                release: Barrier::new(2),
            }),
            dump_entered,
        )
    }
}

pub(super) struct PausingVfs {
    pub(super) inner: Arc<dyn Vfs>,
    pub(super) pause: Arc<DumpPause>,
}

impl Vfs for PausingVfs {
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
        Ok(Box::new(PausingFile {
            inner: self.inner.open(path, opts)?,
            pause: Arc::clone(&self.pause),
            pausable: !path.starts_with("wal/"),
        }))
    }
    fn remove_file(&self, path: &str) -> Result<()> {
        self.inner.remove_file(path)
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
    /// Delegated like every other capability: `Database::open_store` takes the
    /// store's one writer lease from the outermost VFS, so a wrapper that
    /// dropped it would open the store it wraps without a lease at all.
    #[cfg(not(target_arch = "wasm32"))]
    fn take_writer_lease(&self) -> Option<relate_storage::WriterLease> {
        self.inner.take_writer_lease()
    }
}

struct PausingFile {
    inner: Box<dyn VfsFile>,
    pause: Arc<DumpPause>,
    pausable: bool,
}

impl VfsFile for PausingFile {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        if self.pausable && self.pause.armed.swap(false, Ordering::SeqCst) {
            if let Some(entered) = self.pause.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            self.pause.release.wait();
        }
        self.inner.write_all(bytes)
    }
    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.inner.read_exact(bytes)
    }
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.inner.seek(pos)
    }
    fn sync_all(&mut self) -> Result<()> {
        self.inner.sync_all()
    }
    fn durability_barrier(&mut self) -> Result<()> {
        self.inner.durability_barrier()
    }
    fn set_len(&mut self, len: u64) -> Result<()> {
        self.inner.set_len(len)
    }
    fn len(&self) -> Result<u64> {
        self.inner.len()
    }
}

/// Advance the store's WAL without changing the graph: one empty replicated
/// record at `commit_ts`. The replicated path takes the source commit
/// timestamp and assigns its own LSN, so this is the harness's cheap way to
/// reach a rotation threshold — one home, so the suites that close segments
/// spell it once.
pub(super) fn advance_wal(db: &Database, commit_ts: u64) {
    db.apply_replicated(commit_ts, vec![]).unwrap();
}

/// The commit group the stall test times: `count` one-node CREATEs on one
/// connection, from the first statement to the last commit's return.
pub(super) fn time_commit_group(db: &Database, count: usize) -> Duration {
    let started = Instant::now();
    let mut c = db.connect().unwrap();
    for i in 0..count {
        c.query(&format!("CREATE (:Commit {{i: {i}}})")).unwrap();
    }
    started.elapsed()
}

/// A store over `vfs` whose first non-WAL write — the dump's — parks until
/// the test releases it, with the checkpoint timer off so only the test's
/// checkpoint runs.
pub(super) fn pausing_store(vfs: Arc<dyn Vfs>, pause: &Arc<DumpPause>) -> Arc<Database> {
    Arc::new(
        Database::open_vfs(
            Arc::new(PausingVfs {
                inner: vfs,
                pause: Arc::clone(pause),
            }),
            // The crate's one timer-off writable preset: the store opens with
            // the checkpoint timer off, so only the test's checkpoint runs.
            crate::test_opts::writable(),
        )
        .unwrap(),
    )
}

/// A `:memory:` store whose snapshot dump can be held at its first write,
/// with the gate that releases it and the signal that it got there. The gate
/// starts unarmed: [`start_held_dump`] arms it just before its checkpoint.
pub(super) fn pausable_dump_db() -> (Arc<Database>, Arc<DumpPause>, mpsc::Receiver<()>) {
    let (pause, dump_entered) = DumpPause::new(false);
    let db = pausing_store(Arc::new(MemoryVfs::new()), &pause);
    (db, pause, dump_entered)
}

/// A pre-armed gate for the rotation-checkpoint protocol
/// ([`checkpoint_dump_in_flight`]).
pub(super) fn dump_pause() -> (Arc<DumpPause>, mpsc::Receiver<()>) {
    DumpPause::new(true)
}

/// The readiness deadline every helper here waits by.
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);

/// Wait for the dump's first-write signal, panicking if it never comes — the
/// one tail both arming protocols share.
fn await_first_write(entered: &mpsc::Receiver<()>) {
    entered
        .recv_timeout(HELPER_TIMEOUT)
        .expect("the checkpoint dump never reached its first write");
}

/// Rotate the seed's record closed and start the rotation checkpoint with
/// its dump parked at the first write.
pub(super) fn checkpoint_dump_in_flight(db: &Arc<Database>, dump_entered: &mpsc::Receiver<()>) {
    let shared = db.shared();
    let lsn = {
        let mut wal = shared.wal.lock().unwrap();
        wal.rotate().unwrap();
        wal.checkpoint_pin().0
    };
    assert_eq!(lsn, 1);
    schedule_checkpoint(&shared);
    await_first_write(dump_entered);
}

/// Block until the in-flight checkpoint clears its busy flag — the
/// engine's own wait protocol ([`wait_checkpoint_idle_within`]), bounded
/// so a stuck checkpoint fails the test instead of hanging it.
pub(super) fn wait_checkpoint_end(shared: &Shared) {
    crate::checkpoint::wait_checkpoint_idle_within(shared, HELPER_TIMEOUT)
        .expect("the checkpoint did not finish within the timeout");
}

/// Arm the pause and start a checkpoint, returning its thread once the dump
/// is provably in flight past its cut.
pub(super) fn start_held_dump(
    db: &Arc<Database>,
    pause: &DumpPause,
    entered: mpsc::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    pause.armed.store(true, Ordering::SeqCst);
    let db = Arc::clone(db);
    let checkpoint = std::thread::spawn(move || db.checkpoint().unwrap());
    await_first_write(&entered);
    checkpoint
}
