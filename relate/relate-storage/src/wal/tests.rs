use super::*;
use crate::vfs::{BundleVfs, NativeVfs, VfsMeta};
use std::io::SeekFrom;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[path = "tests/encoded.rs"]
mod encoded;
#[path = "tests/lifecycle.rs"]
mod lifecycle;
#[path = "tests/ontology_magic.rs"]
mod ontology_magic;
#[path = "tests/open.rs"]
mod open;
#[path = "tests/payload_cap.rs"]
mod payload_cap;
#[path = "tests/undecodable.rs"]
mod undecodable;

fn write_frame(vfs: &dyn Vfs, path: &str, payload: &[u8]) {
    let mut f = vfs.open(path, VfsOpen::append_create()).unwrap();
    f.write_all(&crc32fast::hash(payload).to_le_bytes())
        .unwrap();
    f.write_all(&(payload.len() as u32).to_le_bytes()).unwrap();
    f.write_all(payload).unwrap();
}

fn write_header(vfs: &dyn Vfs, path: &str, len: u32) {
    let mut f = vfs.open(path, VfsOpen::append_create()).unwrap();
    f.write_all(&0u32.to_le_bytes()).unwrap();
    f.write_all(&len.to_le_bytes()).unwrap();
}

#[test]
fn crc_valid_max_commit_timestamp_is_a_storage_error_and_previous_value_replays() {
    for ts in [u64::MAX - 1, u64::MAX] {
        let vfs = Arc::new(MemoryVfs::new());
        write_magic(&*vfs, "00000001.log", MAGIC, true).unwrap();
        let bytes = serde_json::to_vec(&WalRecord {
            lsn: 1,
            commit_ts: ts,
            ops: vec![],
            metadata: None,
            changed_keys: None,
            record_type: None,
        })
        .unwrap();
        write_frame(&*vfs, "00000001.log", &bytes);
        if ts == u64::MAX {
            let err = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0)
                .err()
                .unwrap();
            assert!(matches!(err, Error::Storage(_)), "{err}");
            assert!(err.to_string().contains("timestamp"), "{err}");
        } else {
            let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
            assert_eq!(wal.replay(0).unwrap()[0].commit_ts, ts);
            let before = vfs.metadata("00000001.log").unwrap().len;
            assert!(matches!(
                wal.append_commit(u64::MAX, &[]),
                Err(Error::Storage(_))
            ));
            assert_eq!(vfs.metadata("00000001.log").unwrap().len, before);
            assert_eq!(wal.records_after(0, 1).unwrap()[0].commit_ts, ts);
        }
    }
}

#[test]
fn append_rejects_zero_repeated_and_backward_timestamps_before_writing() {
    let vfs = Arc::new(MemoryVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
    let rejects_without_writing = |wal: &mut Wal, ts| {
        let before = crate::vfs::read_to_vec(&*vfs, "00000001.log").unwrap();
        let (lsn, bytes, commits, last_ts) = (wal.lsn, wal.bytes, wal.commits, wal.last_commit_ts);
        assert!(matches!(wal.append_record(ts, &[]), Err(Error::Storage(_))));
        assert_eq!(
            (wal.lsn, wal.bytes, wal.commits, wal.last_commit_ts),
            (lsn, bytes, commits, last_ts)
        );
        assert_eq!(
            crate::vfs::read_to_vec(&*vfs, "00000001.log").unwrap(),
            before
        );
    };
    rejects_without_writing(&mut wal, 0);
    wal.append_commit(2, &[]).unwrap();
    for ts in [1, 2, 0] {
        rejects_without_writing(&mut wal, ts);
    }
    assert_eq!(wal.append_commit(3, &[]).unwrap(), 2);
    assert_eq!(wal.replay(0).unwrap().len(), 2);
}

#[test]
fn scan_rejects_zero_or_nonforward_commits_even_below_checkpoint_and_across_segments() {
    for (bad_ts, separate_segment) in [(0, false), (2, false), (1, true), (2, true)] {
        let vfs = Arc::new(MemoryVfs::new());
        let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
        wal.append_commit(2, &[]).unwrap();
        if separate_segment {
            wal.rotate().unwrap();
        }
        let path = if separate_segment {
            "00000002.log"
        } else {
            "00000001.log"
        };
        let bytes = serde_json::to_vec(&WalRecord {
            lsn: 2,
            commit_ts: bad_ts,
            ops: vec![],
            metadata: None,
            changed_keys: None,
            record_type: None,
        })
        .unwrap();
        write_frame(&*vfs, path, &bytes);

        for err in [
            wal.replay(1).unwrap_err(),
            wal.records_after(0, 2).unwrap_err(),
            Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 1)
                .err()
                .unwrap(),
        ] {
            assert!(matches!(err, Error::Storage(_)), "{bad_ts}: {err}");
            assert!(err.to_string().contains("timestamp"), "{err}");
        }
    }
}

#[test]
fn read_cursor_rejects_nonforward_timestamp_in_later_frame() {
    let vfs = Arc::new(MemoryVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
    wal.append_commit(2, &[]).unwrap();
    assert_eq!(wal.records_after(0, 1).unwrap()[0].commit_ts, 2);
    let bytes = serde_json::to_vec(&WalRecord {
        lsn: 2,
        commit_ts: 2,
        ops: vec![],
        metadata: None,
        changed_keys: None,
        record_type: None,
    })
    .unwrap();
    write_frame(&*vfs, "00000001.log", &bytes);
    wal.lsn = 2; // Simulate the newly persisted second frame in this open handle.
    let err = wal.records_after(1, 1).unwrap_err();
    assert!(matches!(err, Error::Storage(_)), "{err}");
    assert!(err.to_string().contains("timestamp"), "{err}");
}

#[test]
fn every_invalid_redo_id_is_rejected_on_read_and_direct_append() {
    use relate_core::Value;
    use relate_executor::PullKind;

    let node = |id| RedoOp::CreateNode {
        id,
        labels: Arc::new(vec![]),
        properties: std::sync::Arc::new(relate_core::PropertyMap::new()),
    };
    let rel = |id, start, end| RedoOp::CreateRel {
        id,
        start,
        end,
        rel_type: "R".into(),
        properties: std::sync::Arc::new(relate_core::PropertyMap::new()),
    };
    let invalid_ops = |bad| {
        vec![
            node(bad),
            rel(bad, 0, 0),
            rel(0, bad, 0),
            rel(0, 0, bad),
            RedoOp::DeleteNode { id: bad },
            RedoOp::DeleteRel { id: bad },
            RedoOp::SetNodeProp {
                id: bad,
                key: "k".into(),
                value: Value::Int(-1),
            },
            RedoOp::RemoveNodeProp {
                id: bad,
                key: "k".into(),
            },
            RedoOp::SetRelProp {
                id: bad,
                key: "k".into(),
                value: Value::Int(-1),
            },
            RedoOp::RemoveRelProp {
                id: bad,
                key: "k".into(),
            },
            RedoOp::AddLabels {
                id: bad,
                labels: vec!["L".into()],
            },
            RedoOp::RemoveLabels {
                id: bad,
                labels: vec!["L".into()],
            },
            RedoOp::PullIdentityPut {
                source: "source".into(),
                kind: PullKind::Node,
                remote_id: "-1".into(),
                local_id: bad,
            },
            RedoOp::PullIdentityPut {
                source: "source".into(),
                kind: PullKind::Rel,
                remote_id: i64::MAX.to_string(),
                local_id: bad,
            },
        ]
    };
    for (case, op) in [-1, i64::MAX].into_iter().flat_map(invalid_ops).enumerate() {
        let vfs = Arc::new(MemoryVfs::new());
        let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
        wal.append_commit(1, &[node(0)]).unwrap();
        let before = crate::vfs::read_to_vec(&*vfs, "00000001.log").unwrap();
        let (lsn, bytes, commits) = (wal.lsn, wal.bytes, wal.commits);
        // The valid first operation must not be installed if a later one fails.
        let err = wal.append_record(2, &[node(1), op.clone()]).unwrap_err();
        assert!(matches!(err, Error::Storage(_)), "case {case}: {err}");
        assert_eq!(
            (wal.lsn, wal.bytes, wal.commits),
            (lsn, bytes, commits),
            "case {case}"
        );
        assert_eq!(
            crate::vfs::read_to_vec(&*vfs, "00000001.log").unwrap(),
            before
        );
        drop(wal);
        let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
        assert_eq!(wal.replay(0).unwrap().len(), 1);
        let bytes = serde_json::to_vec(&WalRecord {
            lsn: 2,
            commit_ts: 2,
            ops: vec![node(1), op],
            metadata: None,
            changed_keys: None,
            record_type: None,
        })
        .unwrap();
        write_frame(&*vfs, "00000001.log", &bytes);
        for err in [
            wal.replay(0).unwrap_err(),
            wal.records_after(0, 2).unwrap_err(),
            Wal::open(vfs as Arc<dyn Vfs>, "", 0).err().unwrap(),
        ] {
            assert!(matches!(err, Error::Storage(_)), "case {case}: {err}");
        }
    }
}

#[test]
fn pull_identity_remote_id_and_property_values_are_not_local_ids() {
    use relate_core::Value;
    use relate_executor::PullKind;

    let mut wal = Wal::memory();
    let ops = [
        RedoOp::PullIdentityPut {
            source: "-1".into(),
            kind: PullKind::Rel,
            remote_id: i64::MAX.to_string(),
            local_id: i64::MAX - 1,
        },
        RedoOp::SetNodeProp {
            id: 0,
            key: "-1".into(),
            value: Value::Int(i64::MAX),
        },
    ];
    wal.append_commit(1, &ops).unwrap();
    assert_eq!(wal.replay(0).unwrap()[0].ops, ops);
}

#[test]
fn wal_short_active_header_replays_prefix_and_is_removed() {
    let vfs = Arc::new(MemoryVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
    append_n(&mut wal, 1);
    drop(wal);
    vfs.open("00000001.log", VfsOpen::append_create())
        .unwrap()
        .write_all(&[1, 2, 3])
        .unwrap();
    let mut wal = Wal::open(vfs, "", 0).unwrap();
    assert_eq!(wal.replay(0).unwrap().len(), 1);
    wal.append_commit(2, &[]).unwrap();
    assert_eq!(wal.replay(0).unwrap().len(), 2);
}

struct AppendFaultVfs {
    inner: MemoryVfs,
    fail_write: Arc<AtomicBool>,
    fail_barrier: Arc<AtomicBool>,
    fail_magic: Arc<AtomicBool>,
}
impl AppendFaultVfs {
    fn new() -> Self {
        Self {
            inner: MemoryVfs::new(),
            fail_write: Arc::new(AtomicBool::new(false)),
            fail_barrier: Arc::new(AtomicBool::new(false)),
            fail_magic: Arc::new(AtomicBool::new(false)),
        }
    }
}
struct AppendFaultFile {
    inner: Box<dyn VfsFile>,
    fail_write: Arc<AtomicBool>,
    fail_barrier: Arc<AtomicBool>,
}
impl VfsFile for AppendFaultFile {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        if self.fail_write.swap(false, Ordering::SeqCst) {
            self.inner.write_all(&bytes[..bytes.len().min(2)])?;
            return Err(Error::io("injected partial frame write"));
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
        self.inner.sync_all()?;
        if self.fail_barrier.swap(false, Ordering::SeqCst) {
            return Err(Error::io("injected ambiguous barrier"));
        }
        Ok(())
    }
    fn len(&self) -> Result<u64> {
        self.inner.len()
    }
    fn set_len(&mut self, len: u64) -> Result<()> {
        self.inner.set_len(len)
    }
}
impl Vfs for AppendFaultVfs {
    fn durable(&self) -> bool {
        true
    }
    fn create_dir_all(&self, path: &str) -> Result<()> {
        self.inner.create_dir_all(path)
    }
    fn create_dir_all_for_atomic(&self, path: &str) -> Result<()> {
        self.inner.create_dir_all(path)
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
        let file = self.inner.open(path, opts)?;
        if opts.append || (opts.truncate && path.ends_with("00000002.log")) {
            Ok(Box::new(AppendFaultFile {
                inner: file,
                fail_write: Arc::clone(if opts.append {
                    &self.fail_write
                } else {
                    &self.fail_magic
                }),
                fail_barrier: Arc::clone(&self.fail_barrier),
            }))
        } else {
            Ok(file)
        }
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
    fn sync_dir(&self, _path: &str) -> Result<()> {
        Ok(())
    }
}

#[test]
fn partial_append_and_ambiguous_barrier_recover_only_physical_prefix() {
    let assert_uncertain_outcome = |wal: &mut Wal, commit_ts: u64| {
        for error in [
            wal.append_record(commit_ts, &[]).unwrap_err(),
            wal.sync().unwrap_err(),
            wal.rotate().unwrap_err(),
            wal.recycle(1).unwrap_err(),
        ] {
            assert!(
                error.to_string().contains("uncertain WAL outcome"),
                "{error}"
            );
        }
    };
    let vfs = Arc::new(AppendFaultVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    wal.append_commit(1, &[]).unwrap();
    let before = crate::vfs::read_to_vec(&vfs.inner, "wal/00000001.log").unwrap();
    vfs.fail_write.store(true, Ordering::SeqCst);
    assert!(wal
        .append_record(2, &[])
        .unwrap_err()
        .to_string()
        .contains("partial frame"));
    assert_eq!(wal.lsn, 1);
    let torn = crate::vfs::read_to_vec(&vfs.inner, "wal/00000001.log").unwrap();
    assert_uncertain_outcome(&mut wal, 2);
    assert_eq!(
        crate::vfs::read_to_vec(&vfs.inner, "wal/00000001.log").unwrap(),
        torn
    );
    assert_eq!(
        crate::vfs::read_to_vec(&vfs.inner, "wal/00000001.log")
            .unwrap()
            .len(),
        before.len() + 2
    );
    drop(wal);
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    assert_eq!(wal.replay(0).unwrap().len(), 1);
    assert_eq!(
        crate::vfs::read_to_vec(&vfs.inner, "wal/00000001.log").unwrap(),
        before
    );
    wal.append_record(2, &[]).unwrap();
    vfs.fail_barrier.store(true, Ordering::SeqCst);
    assert!(wal
        .sync()
        .unwrap_err()
        .to_string()
        .contains("ambiguous barrier"));
    assert_uncertain_outcome(&mut wal, 3);
    drop(wal);
    let wal = Wal::open(vfs, "wal", 0).unwrap();
    assert_eq!(wal.replay(0).unwrap().len(), 2);
}

#[test]
fn failed_rotate_short_magic_recovers_only_verified_highest_segment() {
    let vfs = Arc::new(AppendFaultVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    wal.append_commit(1, &[]).unwrap();
    let previous = crate::vfs::read_to_vec(&vfs.inner, "wal/00000001.log").unwrap();
    vfs.fail_magic.store(true, Ordering::SeqCst);
    assert!(wal
        .rotate()
        .unwrap_err()
        .to_string()
        .contains("partial frame"));
    assert_eq!(wal.inner.seg_id, 1);
    assert!(wal.inner.closed.is_empty());
    assert_eq!(
        crate::vfs::read_to_vec(&vfs.inner, "wal/00000002.log").unwrap(),
        &MAGIC[..2]
    );
    assert!(wal
        .append_commit(2, &[])
        .unwrap_err()
        .to_string()
        .contains("uncertain WAL outcome"));
    drop(wal);
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    assert_eq!(
        crate::vfs::read_to_vec(&vfs.inner, "wal/00000001.log").unwrap(),
        previous
    );
    assert_eq!(wal.replay(0).unwrap().len(), 1);
    assert_eq!(wal.append_commit(2, &[]).unwrap(), 2);
    drop(wal);
    assert_eq!(
        Wal::open(vfs, "wal", 0).unwrap().replay(0).unwrap().len(),
        2
    );
}

struct ReadFaultVfs {
    inner: MemoryVfs,
    fail_on: Arc<AtomicUsize>,
    repair_chunks_only: bool,
    fail_rename: AtomicBool,
    fail_write_on: AtomicUsize,
    repair_reads: Arc<AtomicUsize>,
    repair_writes: Arc<AtomicUsize>,
    max_repair_write: Arc<AtomicUsize>,
    open_sources: Arc<AtomicUsize>,
}
impl ReadFaultVfs {
    fn new(fail_on: usize, repair_chunks_only: bool) -> Self {
        Self {
            inner: MemoryVfs::new(),
            fail_on: Arc::new(AtomicUsize::new(fail_on)),
            repair_chunks_only,
            fail_rename: AtomicBool::new(false),
            fail_write_on: AtomicUsize::new(usize::MAX),
            repair_reads: Arc::new(AtomicUsize::new(0)),
            repair_writes: Arc::new(AtomicUsize::new(0)),
            max_repair_write: Arc::new(AtomicUsize::new(0)),
            open_sources: Arc::new(AtomicUsize::new(0)),
        }
    }
}
struct ReadFaultFile {
    inner: Box<dyn VfsFile>,
    fail_on: Arc<AtomicUsize>,
    reads: usize,
    repair_chunks_only: bool,
    repair_reads: Arc<AtomicUsize>,
    open_sources: Arc<AtomicUsize>,
}
impl Drop for ReadFaultFile {
    fn drop(&mut self) {
        self.open_sources.fetch_sub(1, Ordering::Relaxed);
    }
}
struct RepairWriteFile {
    inner: Box<dyn VfsFile>,
    writes: Arc<AtomicUsize>,
    max_write: Arc<AtomicUsize>,
    fail_on: usize,
}
impl VfsFile for RepairWriteFile {
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        let call = self.writes.fetch_add(1, Ordering::Relaxed) + 1;
        self.max_write.fetch_max(buf.len(), Ordering::Relaxed);
        if call == self.fail_on {
            return Err(Error::io("injected WAL copy write failure"));
        }
        self.inner.write_all(buf)
    }
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        self.inner.read_exact(buf)
    }
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.inner.seek(pos)
    }
    fn sync_all(&mut self) -> Result<()> {
        self.inner.sync_all()
    }
    fn len(&self) -> Result<u64> {
        self.inner.len()
    }
    fn set_len(&mut self, len: u64) -> Result<()> {
        self.inner.set_len(len)
    }
}
impl VfsFile for ReadFaultFile {
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.inner.write_all(buf)
    }
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        // In repair-chunks-only mode only 64 KiB reads count or fail.
        if self.repair_chunks_only && buf.len() != 64 * 1024 {
            return self.inner.read_exact(buf);
        }
        self.reads += 1;
        if self.repair_chunks_only {
            self.repair_reads.fetch_add(1, Ordering::Relaxed);
        }
        if self.reads == self.fail_on.load(Ordering::Relaxed) {
            return Err(Error::io("injected WAL read failure"));
        }
        self.inner.read_exact(buf)
    }
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.inner.seek(pos)
    }
    fn sync_all(&mut self) -> Result<()> {
        self.inner.sync_all()
    }
    fn len(&self) -> Result<u64> {
        self.inner.len()
    }
    fn set_len(&mut self, len: u64) -> Result<()> {
        self.inner.set_len(len)
    }
}
impl Vfs for ReadFaultVfs {
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    fn create_dir_all(&self, path: &str) -> Result<()> {
        self.inner.create_dir_all(path)
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
        let inner = self.inner.open(path, opts)?;
        if opts.read && !opts.write {
            self.open_sources.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(ReadFaultFile {
                inner,
                fail_on: Arc::clone(&self.fail_on),
                reads: 0,
                repair_chunks_only: self.repair_chunks_only,
                repair_reads: Arc::clone(&self.repair_reads),
                open_sources: Arc::clone(&self.open_sources),
            }))
        } else if opts.create_new {
            Ok(Box::new(RepairWriteFile {
                inner,
                writes: Arc::clone(&self.repair_writes),
                max_write: Arc::clone(&self.max_repair_write),
                fail_on: self.fail_write_on.load(Ordering::Relaxed),
            }))
        } else {
            Ok(inner)
        }
    }
    fn remove_file(&self, path: &str) -> Result<()> {
        self.inner.remove_file(path)
    }
    fn remove_dir_all(&self, path: &str) -> Result<()> {
        self.inner.remove_dir_all(path)
    }
    fn rename(&self, from: &str, to: &str) -> Result<()> {
        assert_eq!(self.open_sources.load(Ordering::Relaxed), 0);
        if self.fail_rename.load(Ordering::Relaxed) {
            return Err(Error::io("injected WAL replacement failure"));
        }
        self.inner.rename(from, to)
    }
}

#[test]
fn wal_repair_streams_many_chunks_and_preserves_original_on_io_failure() {
    let vfs = Arc::new(ReadFaultVfs::new(usize::MAX, true));
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    append_n(&mut wal, 20_000); // Intact prefix spans many 64 KiB reads.
    drop(wal);
    let path = "wal/00000001.log";
    write_header(&vfs.inner, path, 12);
    let before = crate::vfs::read_to_vec(&vfs.inner, path).unwrap();

    vfs.fail_on.store(5, Ordering::Relaxed);
    assert!(format!(
        "{}",
        Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0)
            .err()
            .unwrap()
    )
    .contains("injected WAL read failure"));
    assert_eq!(crate::vfs::read_to_vec(&vfs.inner, path).unwrap(), before);
    assert_eq!(vfs.inner.list_dir("wal").unwrap(), ["00000001.log"]);

    vfs.fail_on.store(usize::MAX, Ordering::Relaxed);
    vfs.fail_write_on.store(5, Ordering::Relaxed);
    vfs.repair_writes.store(0, Ordering::Relaxed);
    assert!(format!(
        "{}",
        Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0)
            .err()
            .unwrap()
    )
    .contains("injected WAL copy write failure"));
    assert_eq!(crate::vfs::read_to_vec(&vfs.inner, path).unwrap(), before);
    assert_eq!(vfs.inner.list_dir("wal").unwrap(), ["00000001.log"]);

    vfs.fail_write_on.store(usize::MAX, Ordering::Relaxed);
    vfs.fail_rename.store(true, Ordering::Relaxed);
    assert!(format!(
        "{}",
        Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0)
            .err()
            .unwrap()
    )
    .contains("injected WAL replacement failure"));
    assert_eq!(crate::vfs::read_to_vec(&vfs.inner, path).unwrap(), before);
    assert_eq!(vfs.inner.list_dir("wal").unwrap(), ["00000001.log"]);

    vfs.fail_rename.store(false, Ordering::Relaxed);
    vfs.repair_reads.store(0, Ordering::Relaxed);
    vfs.repair_writes.store(0, Ordering::Relaxed);
    vfs.max_repair_write.store(0, Ordering::Relaxed);
    let mut repaired = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    let writes = vfs.repair_writes.load(Ordering::Relaxed);
    assert!(
        writes > 8,
        "expected many bounded copy writes, got {writes}"
    );
    assert_eq!(vfs.repair_reads.load(Ordering::Relaxed), writes - 1);
    assert_eq!(vfs.max_repair_write.load(Ordering::Relaxed), 64 * 1024);
    assert_eq!(
        vfs.inner.metadata(path).unwrap().len,
        before.len() as u64 - 8
    );
    assert_eq!(repaired.replay(0).unwrap().len(), 20_000);
    assert_eq!(repaired.append_commit(20_001, &[]).unwrap(), 20_001);
    drop(repaired);
    assert_eq!(
        Wal::open(vfs, "wal", 0).unwrap().replay(0).unwrap().len(),
        20_001
    );
}

#[test]
fn wal_magic_header_and_payload_read_errors_are_not_eof_or_torn_tail() {
    for read in 1..=3 {
        let vfs = ReadFaultVfs::new(read, false);
        write_magic(&vfs.inner, "00000001.log", MAGIC, true).unwrap();
        write_frame(
            &vfs.inner,
            "00000001.log",
            br#"{"lsn":1,"commit_ts":1,"ops":[]}"#,
        );
        assert!(
            format!("{}", Wal::open(Arc::new(vfs), "", 0).err().unwrap())
                .contains("injected WAL read failure"),
            "read {read}"
        );
    }
    // A hinted read must not rescan from zero on a genuine read failure.
    let vfs = Arc::new(ReadFaultVfs::new(usize::MAX, false));
    let payload = br#"{"lsn":1,"commit_ts":1,"ops":[]}"#;
    write_magic(&vfs.inner, "00000001.log", MAGIC, true).unwrap();
    write_frame(&vfs.inner, "00000001.log", payload);
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
    assert_eq!(wal.records_after(0, 1).unwrap().len(), 1);
    // A read failure on the next frame is propagated even when the scan
    // begins at an otherwise valid cached offset.
    write_frame(
        &*wal.inner.vfs,
        "00000001.log",
        br#"{"lsn":2,"commit_ts":2,"ops":[]}"#,
    );
    wal.lsn = 2;
    wal.inner.page_hint = Some(super::WalCursor {
        lsn: 1,
        seg_id: 1,
        offset: MAGIC.len() as u64 + 8 + payload.len() as u64,
        commit_ts: 1,
    });
    vfs.fail_on.store(1, Ordering::Relaxed); // hinted scan's header read
    let err = wal.records_after(1, 1).err().unwrap();
    assert!(format!("{err}").contains("injected WAL read failure"));
}

fn file_wal() -> (tempfile::TempDir, Wal) {
    let dir = tempfile::tempdir().unwrap();
    let vfs = Arc::new(NativeVfs::new(dir.path()));
    let wal = Wal::open(vfs, "", 0).unwrap();
    (dir, wal)
}

fn bundle_wal() -> (tempfile::TempDir, Arc<BundleVfs>, Wal) {
    let dir = tempfile::tempdir().unwrap();
    let vfs = Arc::new(BundleVfs::open_or_create(dir.path().join("graph.rl")).unwrap());
    let wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    (dir, vfs, wal)
}

fn append_n(wal: &mut Wal, n: usize) {
    let start = wal.last_commit_ts;
    for i in 0..n {
        wal.append_commit(start + i as u64 + 1, &[]).unwrap();
    }
}

#[test]
fn old_shape_wal_json_replays_on_typed_code() {
    use relate_core::Value;
    use relate_executor::Graph;

    // Exact old on-disk shape: `properties`/`value` are tagged JSON maps
    // (`{"t":"string","v":...}`), not the new typed codec.
    let payload = br#"{
        "lsn": 1,
        "commit_ts": 42,
        "ops": [
            {"op":"CreateNode","id":1,"labels":["Person"],
             "properties":{"name":{"t":"string","v":"Alice"},"age":{"t":"int","v":30}}},
            {"op":"CreateNode","id":2,"labels":["Person"],"properties":{}},
            {"op":"CreateRel","id":10,"start":1,"end":2,"rel_type":"KNOWS",
             "properties":{"weight":{"t":"float","v":1.5}}},
            {"op":"SetRelProp","id":10,"key":"since","value":{"t":"int","v":2020}}
        ]
    }"#;

    // Write it as a real segment: magic + (crc, len, payload) frame.
    let dir = tempfile::tempdir().unwrap();
    let mut seg = std::fs::File::create(dir.path().join("00000001.log")).unwrap();
    use std::io::Write;
    seg.write_all(MAGIC).unwrap();
    seg.write_all(&crc32fast::hash(payload).to_le_bytes())
        .unwrap();
    seg.write_all(&(payload.len() as u32).to_le_bytes())
        .unwrap();
    seg.write_all(payload).unwrap();
    drop(seg);

    // Decode exactly as `Wal::open` / `Wal::replay` do.
    let wal = Wal::open(Arc::new(NativeVfs::new(dir.path())), "", 0).unwrap();
    let recs = wal.replay(0).unwrap();
    assert_eq!(recs.len(), 1, "old-shape record must decode cleanly");
    let rec = &recs[0];
    assert_eq!((rec.lsn, rec.commit_ts), (1, 42));
    assert_eq!(rec.ops.len(), 4);

    let g = Graph::default();
    g.apply_redo(rec.ops.clone(), rec.commit_ts).unwrap();

    let ts = g.commit_ts();
    assert_eq!(g.live_node_count(), 2);
    assert_eq!(
        g.node_prop_at(1, ts, "name"),
        Some(Value::String("Alice".into()))
    );
    assert_eq!(g.node_prop_at(1, ts, "age"), Some(Value::Int(30)));
    assert_eq!(g.node_labels_at(1, ts), Some(vec!["Person".to_string()]));
    assert_eq!(g.rel_ends_at(10, ts), Some((1, 2)));
    assert_eq!(g.rel_prop_at(10, ts, "weight"), Some(Value::Float(1.5)));
    assert_eq!(g.rel_prop_at(10, ts, "since"), Some(Value::Int(2020)));
}

/// LABS-903: only a read-only WAL can release its source — an appender
/// needs the segments it writes to.
#[test]
fn a_writable_wal_refuses_to_release_its_source() {
    let mut wal = Wal::memory();
    let err = wal.release_source(Arc::new(MemoryVfs::new())).unwrap_err();
    assert!(err.to_string().contains("only a read-only WAL"), "{err}");
}

/// LABS-903: a released WAL refuses a record read that would need the
/// released segments — the history left with the source, and a reader
/// must hear that, not a silent empty answer read off the fresh empty
/// segment. Caught-up polls still answer empty: nothing can ever be
/// appended to a released WAL.
#[test]
fn a_released_wal_refuses_record_reads_below_the_loaded_lsn() {
    let source = Arc::new(MemoryVfs::new());
    {
        let mut wal = Wal::open(source.clone(), "wal", 0).unwrap();
        wal.append_commit(
            1,
            &[RedoOp::CreateNode {
                id: 1,
                labels: Default::default(),
                properties: Default::default(),
            }],
        )
        .unwrap();
    }
    let mut wal = Wal::open_readonly(source, "wal", 0, Error::WAL_PAYLOAD_CAP_BYTES).unwrap();
    let lsn = wal.lsn;
    assert_eq!(wal.records_after(0, 10).unwrap().len(), 1);
    wal.release_source(Arc::new(MemoryVfs::new())).unwrap();
    let err = wal.records_after(0, 10).unwrap_err();
    assert!(
        err.to_string().contains("the WAL source was released"),
        "{err}"
    );
    // Every record reader refuses alike — replay and the recovery cursor
    // read the same released segments, never the fresh empty one.
    let err = wal.replay(0).unwrap_err();
    assert!(
        err.to_string().contains("the WAL source was released"),
        "{err}"
    );
    assert!(wal.records_after(lsn, 10).unwrap().is_empty());
    assert!(wal.records_after(u64::MAX, 10).unwrap().is_empty());
}
