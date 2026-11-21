//! The per-record WAL payload cap (ADR-0650, the capacity family's
//! `CapacityKind::WalPayload` kind since ADR-0494) and the preflight
//! proofs around it: a record that crosses the cap is refused before any
//! frame is written, a structurally invalid frame length fails every read
//! path loudly while an overrunning length reads as the torn tail it
//! indistinguishably is, and a stale preflight proof never writes a frame.

use super::*;
use relate_core::CapacityKind;

#[test]
fn the_frame_format_bounds_the_scanner_zero_loudly_overruns_as_torn_tails() {
    // A zero length is the one length the frame format itself cannot mean:
    // it fails every read path loudly, a `Storage` error naming the length
    // — deliberately NOT the typed cap variant (ADR-0492: a corrupt
    // segment is not a refused commit).
    let vfs = Arc::new(MemoryVfs::new());
    write_magic(&*vfs, "00000001.log", MAGIC, true).unwrap();
    write_header(&*vfs, "00000001.log", 0);
    let err = Wal::open(vfs, "", 0)
        .err()
        .unwrap_or_else(|| panic!("a zero length must fail the open"));
    assert!(matches!(err, Error::Storage(_)), "{err}");
    assert!(
        err.to_string().contains("invalid WAL payload length 0"),
        "{err}"
    );
    // A length the file cannot complete — a crash mid-frame or a corrupt
    // header, and under ADR-0650 any length up to the format's 32-bit
    // field, since a store may legitimately hold records larger than a
    // later open's admission cap — is an incomplete payload: a torn tail,
    // never a per-open-cap corruption refusal (a valid record written
    // under a bigger cap must still open). A writable open trims it; the
    // replay paths answer the records before it.
    for len in [Error::WAL_PAYLOAD_CAP_BYTES as u32 + 1, u32::MAX] {
        let vfs = Arc::new(MemoryVfs::new());
        write_magic(&*vfs, "00000001.log", MAGIC, true).unwrap();
        write_header(&*vfs, "00000001.log", len);
        let (wal, trimmed) = Wal::open_with_report(
            Arc::clone(&vfs) as Arc<dyn Vfs>,
            "",
            0,
            Error::WAL_PAYLOAD_CAP_BYTES,
        )
        .unwrap_or_else(|e| panic!("length {len} must open: {e}"));
        assert_eq!(trimmed.len(), 1, "length {len}: the tail is trimmed");
        assert_eq!(wal.lsn, 0);
        assert_eq!(wal.replay(0).unwrap().len(), 0);
    }
    let vfs = Arc::new(MemoryVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "", 0).unwrap();
    append_n(&mut wal, 1);
    write_header(&*vfs, "00000001.log", u32::MAX);
    // The live handle read the segment at open; replay answers the records
    // before the overrunning frame, which is the torn tail it is.
    assert_eq!(wal.replay(0).unwrap().len(), 1);
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
            .contains("per-commit WAL payload cap is 256 MB"),
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

#[test]
fn a_cap_the_frame_format_cannot_encode_refuses_the_constructor() {
    // The guard fires before any segment byte is created — a truncating
    // bound must never reach the append path — with the same refusal
    // sentence the open-time resolver renders for the knob's own spelling
    // (ADR-0650).
    let over = MAX_RECORD_PAYLOAD + 1;
    let err = Wal::open_notifying(
        Arc::new(MemoryVfs::new()) as Arc<dyn Vfs>,
        "",
        0,
        over,
        &mut |_| {},
    )
    .err()
    .unwrap_or_else(|| panic!("a cap over the frame bound must refuse the open"));
    assert!(matches!(err, Error::Semantic(_)), "{err}");
    assert!(err.to_string().contains("32 bits"), "{err}");
    assert!(err.to_string().contains("WAL payload cap"), "{err}");
    // The read-only constructor and both encoders run the same guard.
    let err = Wal::open_readonly(Arc::new(MemoryVfs::new()) as Arc<dyn Vfs>, "", 0, over)
        .err()
        .unwrap_or_else(|| panic!("a cap over the frame bound must refuse the read-only open"));
    assert!(err.to_string().contains("32 bits"), "{err}");
    let err = EncodedOps::encode_unmetered(&[], over).unwrap_err();
    assert!(err.to_string().contains("32 bits"), "{err}");
}

#[test]
fn the_append_boundary_refuses_a_record_encoded_against_a_larger_cap() {
    // The append boundary is the handle's own admission (ADR-0650): a
    // caller that encoded its record against another bound — a bigger
    // ad-hoc cap, or a direct construction — cannot append a frame this
    // store's resolved cap would have refused at encode.
    let vfs = Arc::new(MemoryVfs::new()) as Arc<dyn Vfs>;
    let (wal, trimmed) = Wal::open_notifying(vfs, "wal", 0, 2048, &mut |_| {}).unwrap();
    assert!(trimmed.is_empty(), "a fresh store holds no torn tail");
    let ops = [RedoOp::SetNodeProp {
        id: 1,
        key: "large".to_string(),
        value: relate_core::Value::String("x".repeat(4096).into()),
    }];
    let encoded = EncodedOps::encode_unmetered(&ops, MAX_RECORD_PAYLOAD).unwrap();
    let err = wal
        .preflight_encoded(1, 1, encoded)
        .err()
        .unwrap_or_else(|| panic!("a 4 KiB record must cross the 2048-byte cap"));
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
    // Nothing was written: the refusal precedes the frame.
    assert_eq!(wal.lsn, 0);
    assert_eq!(wal.replay(0).unwrap().len(), 0);
}
