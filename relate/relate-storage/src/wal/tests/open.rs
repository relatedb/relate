//! The open-path tests: the constructor family's trim and repair
//! behavior (torn tails on the active and closed segments, the notify
//! callback, the read-only open's tolerance of a short highest segment).
//! Split from the root test module so neither file creeps past the 1k-line
//! boundary; the shared frame-writing helpers stay there and arrive
//! through the glob.

use super::*;

/// A torn frame suffix on `path`: a plausible-length header (crc, then a
/// length the file never completes) plus a truncated body — the byte shape
/// a crash mid-append or a relaxed rotate leaves behind. Both torn-tail
/// tests spell it through this one fn.
/// Poll once and require the repaired segment's frames: one record at
/// `expected_lsn` from segment 2. The `Idle`/`Gap` panics are the
/// contract both short-highest-segment tests pin — the follower must
/// neither miss the repaired frames nor misread the short header as a
/// gap. Returns the cursor so a caller can commit it.
fn assert_repaired_segment_delivers(follower: &mut Wal, expected_lsn: u64) -> Option<WalCursor> {
    match follower.poll_follow(0).unwrap() {
        WalCatchUp::Records { records, cursor } => {
            assert_eq!(records.len(), 1, "one record from the repaired segment");
            assert_eq!(records[0].lsn, expected_lsn);
            assert_eq!(cursor.seg_id, 2);
            Some(cursor)
        }
        WalCatchUp::Idle => panic!("frames in the repaired segment were not delivered"),
        WalCatchUp::Gap => panic!("a repaired short segment is not a gap"),
    }
}

fn append_torn_frame(path: &std::path::Path) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&123u32.to_le_bytes()).unwrap();
    file.write_all(&100u32.to_le_bytes()).unwrap();
    file.write_all(b"{\"").unwrap();
}

#[test]
fn wal_open_notifying_hands_each_trim_to_the_callback() {
    let vfs = Arc::new(MemoryVfs::new());
    write_magic(&*vfs, "00000001.log", MAGIC, true).unwrap();
    vfs.open("00000001.log", VfsOpen::append_create())
        .unwrap()
        .write_all(&[1, 2])
        .unwrap();
    write_magic(&*vfs, "00000002.log", MAGIC, true).unwrap();

    let mut seen = Vec::new();
    let (_wal, trimmed_tails) = Wal::open_notifying(
        Arc::clone(&vfs) as Arc<dyn Vfs>,
        "",
        0,
        Error::WAL_PAYLOAD_CAP_BYTES,
        &mut |tail| seen.push(tail.clone()),
    )
    .unwrap();
    assert_eq!(
        seen,
        vec![TrimmedTail {
            path: "00000001.log".to_string(),
            discarded_bytes: 2,
        }]
    );
    assert_eq!(seen, trimmed_tails, "the callback saw the returned trims");
}

#[test]
fn wal_torn_closed_tail_is_trimmed_but_existing_magic_must_be_complete() {
    // A relaxed (snapshot) rotate skips the barrier, so a torn suffix can land
    // on a just-rotated *closed* segment. Recovery keeps the intact prefix and
    // trims the torn 2-byte suffix instead of failing closed.
    let vfs = Arc::new(MemoryVfs::new());
    write_magic(&*vfs, "00000001.log", MAGIC, true).unwrap();
    vfs.open("00000001.log", VfsOpen::append_create())
        .unwrap()
        .write_all(&[1, 2])
        .unwrap();
    write_magic(&*vfs, "00000002.log", MAGIC, true).unwrap();

    let (wal, trimmed_tails) = Wal::open_with_report(
        Arc::clone(&vfs) as Arc<dyn Vfs>,
        "",
        0,
        Error::WAL_PAYLOAD_CAP_BYTES,
    )
    .unwrap();
    assert!(wal.replay(0).unwrap().is_empty());
    assert_eq!(
        vfs.metadata("00000001.log").unwrap().len,
        MAGIC.len() as u64,
        "closed torn suffix must be trimmed to the committed prefix"
    );
    assert_eq!(
        trimmed_tails,
        vec![TrimmedTail {
            path: "00000001.log".to_string(),
            discarded_bytes: 2,
        }],
        "a trimmed torn tail is reported by the open, not repaired silently"
    );

    // A previously existing segment whose magic is empty or an incomplete
    // non-frame prefix is still corruption: only a *complete* magic is normal.
    for prefix in [b"".as_slice(), b"MDBWAL".as_slice()] {
        let vfs = Arc::new(MemoryVfs::new());
        vfs.open("00000001.log", VfsOpen::create_truncate())
            .unwrap()
            .write_all(prefix)
            .unwrap();
        assert!(Wal::open(vfs, "", 0).is_err());
    }
}

#[test]
fn wal_in_range_truncated_closed_payload_trims_suffix_and_preserves_prefix() {
    let vfs = Arc::new(MemoryVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    append_n(&mut wal, 2);
    wal.rotate().unwrap();
    let path = "wal/00000001.log";
    let committed = crate::vfs::read_to_vec(&*vfs, path).unwrap();

    // A complete in-range header whose declared payload runs past physical EOF
    // in a *closed* segment is a torn suffix (relaxed rotate), not bitrot.
    write_header(&*vfs, path, 64); // Within the cap, beyond the closed file's end.
    vfs.open(path, VfsOpen::append_create())
        .unwrap()
        .write_all(b"partial")
        .unwrap();
    let corrupted = crate::vfs::read_to_vec(&*vfs, path).unwrap();
    assert!(corrupted.len() > committed.len());

    // The existing handle neither errors nor treats the torn suffix as a
    // commit: replay and a bounded record read both return the intact committed prefix.
    assert_eq!(
        wal.replay(0)
            .unwrap()
            .iter()
            .map(|r| r.lsn)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        wal.records_after(0, 2)
            .unwrap()
            .iter()
            .map(|r| r.lsn)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    drop(wal);

    // Reopen succeeds and atomically trims the damaged suffix back to the
    // committed prefix; the intact records still replay.
    let recovered = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    assert_eq!(
        recovered
            .replay(0)
            .unwrap()
            .iter()
            .map(|r| r.lsn)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(crate::vfs::read_to_vec(&*vfs, path).unwrap(), committed);
}

#[test]
fn wal_repairs_incomplete_active_tail_before_append_on_native_and_memory() {
    fn check(vfs: Arc<dyn Vfs>) {
        let mut wal = Wal::open(Arc::clone(&vfs), "wal", 0).unwrap();
        append_n(&mut wal, 2);
        drop(wal);
        let path = "wal/00000001.log";
        write_header(&*vfs, path, 12);
        vfs.open(path, VfsOpen::append_create())
            .unwrap()
            .write_all(b"{")
            .unwrap();
        let mut reopened = Wal::open(Arc::clone(&vfs), "wal", 0).unwrap();
        assert_eq!(reopened.replay(0).unwrap().len(), 2);
        assert_eq!(reopened.append_commit(3, &[]).unwrap(), 3);
        assert_eq!(
            reopened
                .replay(0)
                .unwrap()
                .iter()
                .map(|r| r.lsn)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(reopened.records_after(0, 10).unwrap().len(), 3);
        drop(reopened);
        assert_eq!(
            Wal::open(vfs, "wal", 0).unwrap().replay(0).unwrap().len(),
            3
        );
    }
    check(Arc::new(MemoryVfs::new()));
    let dir = tempfile::tempdir().unwrap();
    check(Arc::new(NativeVfs::new(dir.path())));
}

/// ADR-0098: the shared atomic writer must never follow a pre-existing
/// temp-path symlink.
#[cfg(unix)]
#[test]
fn wal_repair_does_not_overwrite_temp_symlink_target() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let vfs = Arc::new(NativeVfs::new(dir.path()));
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    append_n(&mut wal, 2);
    drop(wal);
    let path = "wal/00000001.log";
    write_header(&*vfs, path, 12);
    vfs.open(path, VfsOpen::append_create())
        .unwrap()
        .write_all(b"{")
        .unwrap();
    let sentinel = dir.path().join("sentinel");
    std::fs::write(&sentinel, b"do not truncate me").unwrap();
    let tmp = dir.path().join("wal/00000001.log.tmp");
    symlink(&sentinel, &tmp).unwrap();

    let mut repaired = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"do not truncate me");
    assert_eq!(std::fs::read_link(&tmp).unwrap(), sentinel);
    assert_eq!(repaired.replay(0).unwrap().len(), 2);
    assert_eq!(repaired.append_commit(3, &[]).unwrap(), 3);
    assert_eq!(repaired.replay(0).unwrap().len(), 3);
}

#[test]
fn open_trims_torn_final_record_before_append() {
    let (dir, wal) = file_wal();
    let path = dir.path().join("00000001.log");
    {
        let mut wal = wal;
        wal.append_commit(1, &[RedoOp::DeleteNode { id: 7 }])
            .unwrap();
    }
    let valid_len = std::fs::metadata(&path).unwrap().len();
    append_torn_frame(&path);
    let torn_len = std::fs::metadata(&path).unwrap().len();

    let (mut wal, trimmed_tails) = Wal::open_with_report(
        Arc::new(NativeVfs::new(dir.path())),
        "",
        0,
        Error::WAL_PAYLOAD_CAP_BYTES,
    )
    .unwrap();
    assert_eq!(wal.lsn, 1);
    assert_eq!(
        trimmed_tails,
        vec![TrimmedTail {
            path: "00000001.log".to_string(),
            discarded_bytes: torn_len - valid_len,
        }],
        "the trim is reported with its segment and discarded byte count"
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), valid_len);
    assert_eq!(wal.append_commit(2, &[]).unwrap(), 2);
    assert_eq!(wal.replay(0).unwrap().len(), 2);
}

/// A rotate that crashed after create-truncate, or one still in progress,
/// leaves a highest segment shorter than its magic. The follower treats it
/// as frameless and picks up the writer's frames once the segment is whole.
#[test]
fn poll_follow_tolerates_a_short_highest_segment() {
    let (dir, mut writer) = file_wal();
    writer
        .append_commit(1, &[RedoOp::DeleteNode { id: 7 }])
        .unwrap();
    writer.sync().unwrap();
    drop(writer);
    let mut follower = Wal::open_readonly(
        Arc::new(NativeVfs::new(dir.path())),
        "",
        0,
        Error::WAL_PAYLOAD_CAP_BYTES,
    )
    .unwrap();
    assert!(matches!(follower.poll_follow(0).unwrap(), WalCatchUp::Idle));

    let short = dir.path().join("00000002.log");
    std::fs::write(&short, &MAGIC[..3]).unwrap();
    assert!(matches!(follower.poll_follow(0).unwrap(), WalCatchUp::Idle));
    assert_eq!(std::fs::metadata(&short).unwrap().len(), 3);

    let mut writer = Wal::open(Arc::new(NativeVfs::new(dir.path())), "", 0).unwrap();
    assert_eq!(writer.inner.seg_id, 2);
    writer
        .append_commit(2, &[RedoOp::DeleteNode { id: 8 }])
        .unwrap();
    writer.sync().unwrap();
    if let Some(cursor) = assert_repaired_segment_delivers(&mut follower, 2) {
        follower.commit_follow_cursor(cursor);
    }
    assert!(matches!(follower.poll_follow(0).unwrap(), WalCatchUp::Idle));
}

/// A read-only open over an empty WAL whose highest segment is short must
/// not leave a cursor under the magic, or every later poll fails the offset
/// check even after the writer repairs the segment.
#[test]
fn readonly_open_on_short_highest_segment_polls_after_repair() {
    let (dir, wal) = file_wal();
    drop(wal);
    let short = dir.path().join("00000002.log");
    std::fs::write(&short, &MAGIC[..3]).unwrap();

    let mut follower = Wal::open_readonly(
        Arc::new(NativeVfs::new(dir.path())),
        "",
        0,
        Error::WAL_PAYLOAD_CAP_BYTES,
    )
    .unwrap();
    let cursor = follower.inner.follow_cursor.unwrap();
    assert_eq!(cursor.seg_id, 2);
    assert_eq!(cursor.offset, MAGIC.len() as u64);
    assert!(matches!(follower.poll_follow(0).unwrap(), WalCatchUp::Idle));

    let mut writer = Wal::open(Arc::new(NativeVfs::new(dir.path())), "", 0).unwrap();
    assert_eq!(writer.inner.seg_id, 2);
    writer
        .append_commit(1, &[RedoOp::DeleteNode { id: 7 }])
        .unwrap();
    writer.sync().unwrap();
    let _ = assert_repaired_segment_delivers(&mut follower, 1);
}

/// A read-only open over a WAL whose *sole* segment is shorter than its
/// magic fails: with no verified predecessor the short header cannot be a
/// fresh rotate's, so it is reported as truncation, not tolerated as
/// frameless.
#[test]
fn readonly_open_rejects_a_short_sole_segment() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("00000001.log"), &MAGIC[..3]).unwrap();
    match Wal::open_readonly(
        Arc::new(NativeVfs::new(dir.path())),
        "",
        0,
        Error::WAL_PAYLOAD_CAP_BYTES,
    ) {
        Err(err) => assert!(
            err.to_string().contains("truncated WAL magic"),
            "unexpected error: {err}"
        ),
        Ok(_) => panic!("a short sole segment must fail a read-only open"),
    }
}

#[test]
fn every_short_or_empty_highest_magic_prefix_recovers_on_memory_and_native() {
    for native in [false, true] {
        // size 0 is the segment file created but stopped before its magic
        // was written; 1..8 are the torn header prefixes.
        for size in 0..MAGIC.len() {
            let dir = native.then(|| tempfile::tempdir().unwrap());
            let vfs: Arc<dyn Vfs> = match dir.as_ref() {
                Some(dir) => Arc::new(NativeVfs::new(dir.path())),
                None => Arc::new(MemoryVfs::new()),
            };
            let mut wal = Wal::open(Arc::clone(&vfs), "wal", 0).unwrap();
            wal.append_commit(1, &[]).unwrap();
            let first = crate::vfs::read_to_vec(&*vfs, "wal/00000001.log").unwrap();
            drop(wal);
            vfs.open("wal/00000002.log", VfsOpen::create_truncate())
                .unwrap()
                .write_all(&MAGIC[..size])
                .unwrap();
            let mut reopened = Wal::open(Arc::clone(&vfs), "wal", 0).unwrap();
            assert_eq!(reopened.replay(0).unwrap().len(), 1);
            assert_eq!(
                crate::vfs::read_to_vec(&*vfs, "wal/00000001.log").unwrap(),
                first
            );
            assert_eq!(reopened.append_commit(2, &[]).unwrap(), 2);
            drop(reopened);
            assert_eq!(
                Wal::open(vfs, "wal", 0).unwrap().replay(0).unwrap().len(),
                2
            );
        }
    }
}

/// A two-segment WAL whose committed predecessor is valid — or
/// CRC-corrupted at its first frame header when `corrupt_predecessor` — and
/// whose highest segment holds exactly `bad` bytes: the store a stop between
/// the new segment's creation and its magic write leaves behind. The shared
/// fixture of the short-magic repair tests.
fn wal_with_committed_prefix_and_short_highest(
    bad: &[u8],
    corrupt_predecessor: bool,
) -> Arc<dyn Vfs> {
    let vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs), "wal", 0).unwrap();
    wal.append_commit(1, &[]).unwrap();
    drop(wal);
    if corrupt_predecessor {
        let mut corrupt = crate::vfs::read_to_vec(&*vfs, "wal/00000001.log").unwrap();
        corrupt[MAGIC.len()] ^= 1;
        crate::vfs::atomic_write(&*vfs, "wal/00000001.log", &corrupt).unwrap();
    }
    vfs.open("wal/00000002.log", VfsOpen::create_truncate())
        .unwrap()
        .write_all(bad)
        .unwrap();
    vfs
}

/// A mismatching short header on the highest segment is not a torn magic:
/// repair leaves it alone and open fails on the truncation.
#[test]
fn short_magic_repair_requires_exact_prefix() {
    let vfs = wal_with_committed_prefix_and_short_highest(b"MZ", false);
    assert!(Wal::open(Arc::clone(&vfs), "wal", 0).is_err());
    assert_eq!(
        crate::vfs::read_to_vec(&*vfs, "wal/00000002.log").unwrap(),
        b"MZ".as_slice()
    );
}

/// A bad CRC in the predecessor must be reported before touching even an
/// otherwise repairable highest magic — empty or torn prefix alike.
#[test]
fn short_magic_repair_requires_clean_predecessor() {
    for bad in [b"".as_slice(), &MAGIC[..2]] {
        let vfs = wal_with_committed_prefix_and_short_highest(bad, true);
        assert!(Wal::open(Arc::clone(&vfs), "wal", 0).is_err());
        assert_eq!(
            crate::vfs::read_to_vec(&*vfs, "wal/00000002.log").unwrap(),
            bad
        );
    }
}

/// A sole segment shorter than the magic — empty or a torn prefix — cannot
/// be repaired: without a predecessor, an empty file may hide lost data.
#[test]
fn short_magic_repair_refuses_sole_short_segment() {
    for bad in [b"".as_slice(), &MAGIC[..2]] {
        let vfs = Arc::new(MemoryVfs::new());
        vfs.open("00000001.log", VfsOpen::create_truncate())
            .unwrap()
            .write_all(bad)
            .unwrap();
        assert!(
            Wal::open(vfs, "", 0).is_err(),
            "a sole segment of {bad:?} cannot be repaired"
        );
    }
}

/// A short *closed* segment is corruption, not a torn rotate: the repair
/// only ever touches the highest segment.
#[test]
fn short_magic_repair_refuses_short_closed_segment() {
    let vfs = Arc::new(MemoryVfs::new());
    vfs.create_dir_all("wal").unwrap();
    vfs.open("wal/00000001.log", VfsOpen::create_truncate())
        .unwrap()
        .write_all(&MAGIC[..3])
        .unwrap();
    write_magic(&*vfs, "wal/00000002.log", MAGIC, true).unwrap();
    assert!(
        Wal::open(vfs, "wal", 0).is_err(),
        "closed segment cannot be repaired"
    );
}

/// A highest segment appearing after a clean read-only open, whose short
/// header is not a torn magic prefix, fails the follower's poll exactly as
/// it would fail the open: one classification, and corruption is never
/// frameless.
#[test]
fn poll_follow_rejects_a_mismatching_short_highest_segment() {
    let (dir, writer) = file_wal();
    drop(writer);
    let mut follower = Wal::open_readonly(
        Arc::new(NativeVfs::new(dir.path())),
        "",
        0,
        Error::WAL_PAYLOAD_CAP_BYTES,
    )
    .unwrap();
    std::fs::write(dir.path().join("00000002.log"), b"MZ").unwrap();
    let err = follower.poll_follow(0).unwrap_err();
    assert!(
        err.to_string().contains("truncated WAL magic"),
        "unexpected error: {err}"
    );
}

#[test]
fn open_rejects_corruption_in_closed_segment() {
    use std::io::{Seek, Write};

    let (dir, wal) = file_wal();
    {
        let mut wal = wal;
        wal.append_commit(1, &[RedoOp::DeleteNode { id: 7 }])
            .unwrap();
        wal.rotate().unwrap();
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("00000001.log"))
        .unwrap();
    file.seek(SeekFrom::Start(16)).unwrap();
    file.write_all(b"!").unwrap();
    let reopened = Wal::open(Arc::new(NativeVfs::new(dir.path())), "", 0);
    assert!(matches!(reopened, Err(Error::Storage(_))));
}

/// `rotate_relaxed` does not barrier the rotated-out segment, so a crash
/// can leave a torn frame on a *closed* file. Open must trim that suffix
/// instead of failing the database.
#[test]
fn open_trims_torn_tail_on_closed_segment_after_relaxed_rotate() {
    let (dir, wal) = file_wal();
    let closed = dir.path().join("00000001.log");
    {
        let mut wal = wal;
        wal.append_record(1, &[RedoOp::DeleteNode { id: 7 }])
            .unwrap();
        wal.rotate_relaxed().unwrap();
        wal.append_record(2, &[]).unwrap();
    }
    let valid_len = std::fs::metadata(&closed).unwrap().len();
    append_torn_frame(&closed);

    let wal = Wal::open(Arc::new(NativeVfs::new(dir.path())), "", 0).unwrap();
    assert_eq!(wal.lsn, 2);
    assert_eq!(std::fs::metadata(&closed).unwrap().len(), valid_len);
    assert_eq!(wal.replay(0).unwrap().len(), 2);
}
