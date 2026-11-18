//! Fake-handle persistence tests. The slot pool's catalog and `Vfs`
//! semantics (the prefix-family rename among them) are plain Rust and
//! run natively on every target; the `Database`-level persistence trio
//! runs under Node's wasm runner, and the browser smoke covers the real
//! sync-access handles.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use relate::Error;
#[cfg(target_arch = "wasm32")]
use relate::{Database, OpenOptions, Value};
use relate_storage::{Vfs, VfsOpen};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

use super::slot::SyncSlot;
use super::vfs::OpfsVfs;

struct MemSlot {
    data: Mutex<Vec<u8>>,
    fail_flush: AtomicBool,
}

impl MemSlot {
    fn new() -> Self {
        Self {
            data: Mutex::new(Vec::new()),
            fail_flush: AtomicBool::new(false),
        }
    }

    fn fail_flush(&self) {
        self.fail_flush.store(true, Ordering::Relaxed);
    }
}

impl SyncSlot for MemSlot {
    fn read_at(&self, at: u64, buf: &mut [u8]) -> relate::Result<usize> {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let start = at as usize;
        if start >= data.len() || buf.is_empty() {
            return Ok(0);
        }
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn write_at(&self, at: u64, buf: &[u8]) -> relate::Result<()> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let start = at as usize;
        if start > data.len() {
            data.resize(start, 0);
        }
        let end = start + buf.len();
        if end > data.len() {
            data.resize(end, 0);
        }
        data[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&self) -> relate::Result<()> {
        if self.fail_flush.load(Ordering::Relaxed) {
            Err(Error::storage("flush failed"))
        } else {
            Ok(())
        }
    }

    fn size(&self) -> relate::Result<u64> {
        Ok(self.data.lock().unwrap_or_else(|e| e.into_inner()).len() as u64)
    }

    fn truncate(&self, len: u64) -> relate::Result<()> {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resize(len as usize, 0);
        Ok(())
    }
}

pub(crate) struct Pool {
    catalog: Arc<MemSlot>,
    slots: Vec<Arc<MemSlot>>,
}

impl Pool {
    pub(crate) fn new(n: usize) -> Self {
        Self {
            catalog: Arc::new(MemSlot::new()),
            slots: (0..n).map(|_| Arc::new(MemSlot::new())).collect(),
        }
    }

    pub(crate) fn open(&self) -> OpfsVfs {
        let files = self
            .slots
            .iter()
            .map(|slot| Box::new(SharedSlot(Arc::clone(slot))) as Box<dyn SyncSlot>)
            .collect();
        OpfsVfs::open_slots(Box::new(SharedSlot(Arc::clone(&self.catalog))), files).unwrap()
    }
}

/// Test seam for the failed-reset pin in bindings.rs: refuse the flush
/// of an untouched slot, so the OPFS wipe — which truncates and
/// flushes every slot, used or free — fails while the live store's
/// files stay untouched. Gated to the wasm32 test build, where its
/// consumer (the bindings module and its pins) compiles.
#[cfg(all(test, target_arch = "wasm32"))]
impl Pool {
    pub(crate) fn fail_wipe(&self) {
        self.slots
            .last()
            .expect("a slot pool is never empty")
            .fail_flush();
    }
}

struct SharedSlot(Arc<MemSlot>);

impl SyncSlot for SharedSlot {
    fn read_at(&self, at: u64, buf: &mut [u8]) -> relate::Result<usize> {
        self.0.read_at(at, buf)
    }
    fn write_at(&self, at: u64, buf: &[u8]) -> relate::Result<()> {
        self.0.write_at(at, buf)
    }
    fn flush(&self) -> relate::Result<()> {
        self.0.flush()
    }
    fn size(&self) -> relate::Result<u64> {
        self.0.size()
    }
    fn truncate(&self, len: u64) -> relate::Result<()> {
        self.0.truncate(len)
    }
}

/// Plant the legacy sidecar tree a pre-rename store carries: the
/// manifest and a WAL segment under `.cypherlite/`, plus a schema-mirror
/// file — no bare `.cypherlite` entry, the implicit-directory shape.
fn legacy_sidecar_tree(vfs: &OpfsVfs) {
    let files = [
        (
            ".cypherlite/manifest.json",
            &b"{\"codec\":\"markdown\"}"[..],
        ),
        (".cypherlite/wal/00000001.log", b"segment"),
        (".cypherlite/schema/schema.cypher", b""),
    ];
    for (path, bytes) in files {
        let mut file = vfs.open(path, VfsOpen::create_new()).unwrap();
        file.write_all(bytes).unwrap();
    }
}

/// A directory rename re-keys the whole prefix family, not one entry:
/// the legacy engine directory (an implicit directory — children only,
/// no bare `.cypherlite` key) moves to `.relate/` with every child and
/// its bytes, so a pre-rename OPFS vault adopts through the engine's
/// `Vfs::rename` (ADR-0623) instead of erroring on the missing bare key
/// or splitting the store.
#[test]
fn a_directory_rename_moves_every_child_of_the_family() {
    let pool = Pool::new(8);
    let vfs = pool.open();
    legacy_sidecar_tree(&vfs);

    vfs.rename(".cypherlite", ".relate").unwrap();

    assert!(vfs.exists(".relate/manifest.json").unwrap());
    assert!(vfs.exists(".relate/wal/00000001.log").unwrap());
    assert!(vfs.exists(".relate/schema/schema.cypher").unwrap());
    assert!(vfs.metadata(".relate").unwrap().is_dir());
    assert!(!vfs.exists(".cypherlite/manifest.json").unwrap());
    assert!(!vfs.exists(".cypherlite").unwrap());

    // The bytes survived the move: the manifest reads back through the
    // new key with its slot's content intact.
    let mut file = vfs
        .open(".relate/manifest.json", VfsOpen::read_only())
        .unwrap();
    let mut buf = vec![0u8; b"{\"codec\":\"markdown\"}".len()];
    file.read_exact(&mut buf).unwrap();
    assert_eq!(buf, b"{\"codec\":\"markdown\"}");
}

/// A rename with a bare entry and prefixed children — the mixed shape —
/// moves both, and an untouched sibling outside the family stays put.
#[test]
fn a_rename_of_a_bare_entry_moves_it_with_its_children() {
    let pool = Pool::new(8);
    let vfs = pool.open();
    let mut marker = vfs.open("dir", VfsOpen::create_new()).unwrap();
    marker.write_all(b"marker").unwrap();
    let mut child = vfs.open("dir/child.txt", VfsOpen::create_new()).unwrap();
    child.write_all(b"child").unwrap();
    let mut other = vfs.open("other/file.txt", VfsOpen::create_new()).unwrap();
    other.write_all(b"other").unwrap();

    vfs.rename("dir", "moved").unwrap();

    assert!(vfs.exists("moved").unwrap());
    assert!(vfs.exists("moved/child.txt").unwrap());
    assert!(!vfs.exists("dir").unwrap());
    assert!(!vfs.exists("dir/child.txt").unwrap());
    assert!(vfs.exists("other/file.txt").unwrap());
}

/// A rename of a name with neither a bare entry nor prefixed children
/// fails closed through the missing-name error, like every backend's
/// `rename` — never a silent no-op.
#[test]
fn a_rename_of_a_missing_name_fails_closed() {
    let pool = Pool::new(4);
    let vfs = pool.open();
    let err = vfs
        .rename(".cypherlite", ".relate")
        .expect_err("nothing to rename");
    assert!(
        matches!(err, Error::Io(ref msg) if msg.contains("missing .cypherlite")),
        "{err}"
    );
}

/// A rename that cannot commit leaves the catalog exactly as it was
/// (the staged-mutation discipline): both spellings keep their keys,
/// nothing half-moves.
#[test]
fn a_failed_rename_leaves_the_catalog_untouched() {
    let pool = Pool::new(8);
    let vfs = pool.open();
    legacy_sidecar_tree(&vfs);
    // The persisted catalog before the refused flush — decoded through
    // the catalog's own reader, so the pin asks about the bytes on the
    // slot, not the live in-memory maps.
    let before = super::catalog::read_catalog(&*pool.catalog).unwrap();
    pool.catalog.fail_flush();

    let err = vfs
        .rename(".cypherlite", ".relate")
        .expect_err("the catalog flush must fail");
    assert!(
        matches!(err, Error::Storage(ref msg) if msg.contains("catalog flush failed")),
        "{err}"
    );
    assert!(vfs.exists(".cypherlite/manifest.json").unwrap());
    assert!(vfs.exists(".cypherlite/wal/00000001.log").unwrap());
    assert!(!vfs.exists(".relate/manifest.json").unwrap());
    assert!(!vfs.exists(".relate").unwrap());
    // The persisted catalog survived: the staged-mutation discipline
    // must hold on the SLOT, not only in the live entries — a decode of
    // the catalog after the refused flush finds the pre-rename
    // generation, never the half-published new one.
    let persisted = super::catalog::read_catalog(&*pool.catalog).unwrap();
    assert_eq!(
        persisted, before,
        "the persisted catalog survived the refused flush"
    );
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test]
fn reopen_replays_a_committed_node() {
    let pool = Pool::new(8);
    {
        let vfs = Arc::new(pool.open());
        let db = Database::open_vfs(vfs as Arc<dyn Vfs>, OpenOptions::default()).unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute("CREATE (:Kept {k: 1})").unwrap();
    }
    let vfs = Arc::new(pool.open());
    let db = Database::open_vfs(vfs as Arc<dyn Vfs>, OpenOptions::default()).unwrap();
    let mut conn = db.connect().unwrap();
    let found = conn.execute("MATCH (n:Kept) RETURN count(n) AS c").unwrap();
    assert_eq!(found.rows[0][0], Value::Int(1));
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test]
fn pool_exhaustion_is_a_storage_error() {
    let vfs = Pool::new(1).open();
    vfs.open("a", VfsOpen::create_truncate()).unwrap();
    let err = match vfs.open("b", VfsOpen::create_truncate()) {
        Ok(_) => panic!("second file should exhaust the pool"),
        Err(err) => err,
    };
    assert!(
        matches!(err, Error::Storage(ref msg) if msg.contains("slot pool exhausted")),
        "{err}"
    );
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test]
fn catalog_flush_failure_is_a_storage_error() {
    let pool = Pool::new(4);
    pool.catalog.fail_flush();
    let vfs = pool.open();
    let err = match vfs.open("a", VfsOpen::create_truncate()) {
        Ok(_) => panic!("catalog flush should fail"),
        Err(err) => err,
    };
    assert!(
        matches!(err, Error::Storage(ref msg) if msg.contains("catalog flush failed")),
        "{err}"
    );
    assert!(!vfs.exists("a").unwrap());
}
