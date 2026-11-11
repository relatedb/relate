//! The per-record WAL payload cap (ADR-0086, the capacity family's
//! `CapacityKind::WalPayload` kind since ADR-0494) and the preflight
//! proofs around it: a record that crosses the cap is refused before any
//! frame is written, a length header beyond the cap is refused before
//! allocation on every read path, and a stale preflight proof never
//! writes a frame.

use super::*;
use relate_core::CapacityKind;

#[test]
fn wal_rejects_bogus_lengths_before_allocation_in_open_replay_and_record_read() {
    for len in [0, Error::WAL_PAYLOAD_CAP_BYTES as u32 + 1, u32::MAX] {
        let vfs = Arc::new(MemoryVfs::new());
        write_magic(&*vfs, "00000001.log", MAGIC, true).unwrap();
        write_header(&*vfs, "00000001.log", len);
        // The scanner's over-cap refusal is the corruption diagnostic — a
        // `Storage` error naming the length — deliberately NOT the typed
        // cap variant (ADR-0492: a corrupt segment is not a refused
        // commit).
        let err = Wal::open(vfs, "", 0)
            .err()
            .unwrap_or_else(|| panic!("length {len} must fail the open"));
        assert!(matches!(err, Error::Storage(_)), "length {len}: {err}");
        assert!(
            err.to_string().contains("invalid WAL payload length"),
            "length {len}: {err}"
        );
    }
    let vfs = Arc::new(MemoryVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
    append_n(&mut wal, 1);
    write_header(&*vfs, "00000001.log", u32::MAX);
    for err in [
        wal.replay(0).unwrap_err(),
        wal.records_after(0, 10).unwrap_err(),
    ] {
        assert!(matches!(err, Error::Storage(_)), "{err}");
        assert!(
            err.to_string().contains("invalid WAL payload length"),
            "{err}"
        );
    }
}

#[test]
fn wal_oversize_append_leaves_lsn_and_file_unchanged() {
    use relate_core::Value;
    let (dir, mut wal) = file_wal();
    let before = std::fs::metadata(dir.path().join("00000001.log"))
        .unwrap()
        .len();
    let ops = [RedoOp::SetNodeProp {
        id: 1,
        key: "large".to_string(),
        value: Value::String("x".repeat(Error::WAL_PAYLOAD_CAP_BYTES as usize).into()),
    }];
    // ADR-0494: the refusal is the capacity family's WalPayload kind —
    // the memory-limit class, its message naming the record and the cap —
    // not a `Storage` prose message to scan back.
    let err = wal.append_record(1, &ops).unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Capacity {
                kind: CapacityKind::WalPayload,
                ..
            }
        ),
        "{err}"
    );
    assert!(err.to_string().contains("single WAL record"), "{err}");
    assert!(
        err.to_string()
            .contains("per-record WAL payload cap is 64 MB"),
        "{err}"
    );
    assert_eq!(wal.lsn, 0);
    assert_eq!(wal.commits, 0);
    assert_eq!(wal.bytes, MAGIC.len() as u64);
    assert_eq!(
        std::fs::metadata(dir.path().join("00000001.log"))
            .unwrap()
            .len(),
        before
    );
    assert_eq!(wal.append_commit(2, &[]).unwrap(), 1);
}

#[test]
fn preflighted_append_rejects_stale_proof_before_first_frame() {
    let mut wal = Wal::memory();
    let stale = wal.preflight_record(1, 1, &[]).unwrap();
    wal.append_commit(1, &[]).unwrap();
    assert!(wal.append_preflighted(stale).is_err());
    assert_eq!(wal.lsn, 1);
    assert_eq!(wal.replay(0).unwrap().len(), 1);
    wal.append_commit(2, &[]).unwrap(); // deterministic errors did not poison
}
