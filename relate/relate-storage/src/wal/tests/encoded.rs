//! The encode-once record (ADR-0094 as amended): the ops are serialized once
//! into a header-slack buffer and the header is stamped in place. The frame
//! must be byte-identical to the record the single-pass serde encoder
//! produced, and the payload cap must refuse at exactly the same figure.

use super::*;
use relate_core::tokens::WorkspaceCharge;
use relate_core::{CapacityKind, Value};
use serde::Serialize;
use std::io::Write;

/// The previous record encoder, kept as the reference: the whole record in
/// one serde pass.
#[derive(Serialize)]
struct WalRecordRef<'a> {
    lsn: u64,
    commit_ts: u64,
    ops: &'a [RedoOp],
}

/// Encode with no workspace charge.
fn encode(ops: &[RedoOp]) -> Result<EncodedOps> {
    EncodedOps::encode_unmetered(ops)
}

fn reference_payload(lsn: u64, commit_ts: u64, ops: &[RedoOp]) -> Vec<u8> {
    serde_json::to_vec(&WalRecordRef {
        lsn,
        commit_ts,
        ops,
    })
    .unwrap()
}

/// The previous counting pass: bytes of the record under `bound`, or the
/// total at which the bound was first crossed.
fn reference_count(
    lsn: u64,
    commit_ts: u64,
    ops: &[RedoOp],
    bound: u64,
) -> std::result::Result<u64, u64> {
    struct Counter {
        bytes: u64,
        bound: u64,
        crossed: Option<u64>,
    }
    impl Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let total = self.bytes + buf.len() as u64;
            if total > self.bound {
                self.crossed = Some(total);
                return Err(std::io::Error::other("bound"));
            }
            self.bytes = total;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter {
        bytes: 0,
        bound,
        crossed: None,
    };
    let result = serde_json::to_writer(
        &mut counter,
        &WalRecordRef {
            lsn,
            commit_ts,
            ops,
        },
    );
    match counter.crossed {
        Some(total) => Err(total),
        None => {
            result.expect("only the bound may stop the reference serializer");
            Ok(counter.bytes)
        }
    }
}

fn create_node(id: i64) -> RedoOp {
    RedoOp::CreateNode {
        id,
        labels: vec!["Person".into(), "Évènement \"q\"".into()].into(),
        properties: Default::default(),
    }
}

fn op_mixes() -> Vec<(&'static str, Vec<RedoOp>)> {
    vec![
        ("empty", vec![]),
        ("one node", vec![create_node(1)]),
        (
            "mixed graph ops",
            vec![
                create_node(1),
                create_node(2),
                RedoOp::CreateRel {
                    id: 10,
                    start: 1,
                    end: 2,
                    rel_type: "KNOWS".into(),
                    properties: Default::default(),
                },
                RedoOp::SetNodeProp {
                    id: 1,
                    key: "bio".into(),
                    value: Value::String("line\nbreak \u{1F600} \\ \"quoted\"".into()),
                },
                RedoOp::AddLabels {
                    id: 2,
                    labels: vec!["A".into(), "B".into()],
                },
                RedoOp::DeleteRel { id: 10 },
                RedoOp::DeleteNode { id: 2 },
            ],
        ),
        (
            "catalog ops",
            vec![
                RedoOp::CreateIndex {
                    name: "i".into(),
                    label: "Person".into(),
                    property: "name".into(),
                },
                RedoOp::DropIndex { name: "i".into() },
                RedoOp::PutDatabase {
                    name: "neo4j".into(),
                    language: 25,
                    composite: false,
                },
            ],
        ),
        (
            "ontology ops",
            vec![
                RedoOp::OntologySet {
                    database: "neo4j".into(),
                    spec: "{\"a\":[1,2,3]}".into(),
                },
                RedoOp::OntologyDrop {
                    database: "neo4j".into(),
                },
            ],
        ),
    ]
}

/// Header values of every width: single digit, mid, and the maximum.
const HEADERS: [(u64, u64); 5] = [
    (1, 1),
    (9, 10),
    (123_456, 7_890_123),
    (u64::MAX, u64::MAX - 1),
    (u64::MAX - 1, u64::MAX - 1),
];

#[test]
fn stamped_payload_is_byte_identical_to_the_single_pass_record() {
    for (name, ops) in op_mixes() {
        for (lsn, ts) in HEADERS {
            let mut encoded = encode(&ops).unwrap();
            encoded.stamp(lsn, ts);
            assert_eq!(
                encoded.payload(),
                reference_payload(lsn, ts, &ops).as_slice(),
                "{name} lsn {lsn} ts {ts}"
            );
        }
    }
}

#[test]
fn reservation_is_the_buffer_and_covers_the_maximum_width_record() {
    for (name, ops) in op_mixes() {
        let mut encoded = encode(&ops).unwrap();
        let max_record = reference_payload(u64::MAX, u64::MAX, &ops).len() as u64;
        // Small records are trimmed to their length: the figure a record
        // with maximum-width fields encodes to, exactly.
        assert_eq!(encoded.reservation_bytes(), max_record, "{name}");
        encoded.stamp(u64::MAX, u64::MAX);
        assert_eq!(encoded.payload().len() as u64, max_record, "{name}");
    }
}

#[test]
fn preflight_encoded_proof_matches_preflight_record_and_writes_the_same_frame() {
    for (name, ops) in op_mixes() {
        let direct = {
            let (dir, mut wal) = file_wal();
            wal.append_commit(1, &ops).unwrap();
            wal.sync().unwrap();
            newest_segment(dir.path())
        };
        let via_encoded = {
            let (dir, mut wal) = file_wal();
            let proof = wal.preflight_encoded(1, 1, encode(&ops).unwrap()).unwrap();
            wal.append_preflighted(proof).unwrap();
            wal.sync().unwrap();
            newest_segment(dir.path())
        };
        assert_eq!(direct, via_encoded, "{name}");
        // The frame carries the single-pass record under its CRC and length.
        let payload = reference_payload(1, 1, &ops);
        let mut frame = Vec::new();
        frame.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        assert!(direct.ends_with(&frame), "{name}");
    }
}

/// The newest segment file (an ontology op rotates to a fresh `MDBWAL02`
/// segment before it is written).
fn newest_segment(dir: &std::path::Path) -> Vec<u8> {
    let mut logs: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .collect();
    logs.sort();
    std::fs::read(logs.last().unwrap()).unwrap()
}

fn big_string_op(len: usize) -> RedoOp {
    RedoOp::SetNodeProp {
        id: 1,
        key: "k".into(),
        value: Value::String("x".repeat(len).into()),
    }
}

#[test]
fn payload_cap_boundary_matches_the_previous_count() {
    let cap = Error::WAL_PAYLOAD_CAP_BYTES;
    let base = reference_count(u64::MAX, u64::MAX, &[big_string_op(0)], u64::MAX).unwrap();
    let fits = (cap - base) as usize;

    let ops = [big_string_op(fits)];
    let encoded = encode(&ops).unwrap();
    // The charge's final settle is the buffer clamped to the cap.
    assert_eq!(encoded.reservation_bytes(), cap);
    assert_eq!(reference_count(u64::MAX, u64::MAX, &ops, cap), Ok(cap));

    // One byte past the cap refuses with the same crossing figure the
    // previous count pass recorded.
    let ops = [big_string_op(fits + 1)];
    let crossed = reference_count(u64::MAX, u64::MAX, &ops, cap).unwrap_err();
    let err = encode(&ops).unwrap_err();
    assert_eq!(
        err,
        Error::Capacity {
            used: crossed,
            limit: cap,
            kind: CapacityKind::WalPayload,
        }
    );
}

#[test]
fn a_boundary_record_still_appends_with_real_header_widths() {
    let cap = Error::WAL_PAYLOAD_CAP_BYTES;
    let base = reference_count(u64::MAX, u64::MAX, &[big_string_op(0)], u64::MAX).unwrap();
    let ops = [big_string_op((cap - base) as usize)];
    let mut wal = Wal::memory();
    wal.append_record(1, &ops).unwrap();
    wal.sync().unwrap();
    assert_eq!(wal.replay(0).unwrap().len(), 1);
}

#[test]
fn encoding_carries_validation_and_the_ontology_fact() {
    // Invalid IDs are refused when the ops are encoded, so no body exists
    // for ops that failed validation.
    let err = encode(&[create_node(-1)]).unwrap_err();
    assert!(matches!(err, Error::Storage(_)), "{err}");
    // The ontology fact travels with the encoding: the append rotates to an
    // MDBWAL02 segment without being handed the ops.
    let vfs = Arc::new(MemoryVfs::new());
    let mut wal = Wal::open(Arc::clone(&vfs) as Arc<dyn Vfs>, "wal", 0).unwrap();
    let ops = [RedoOp::OntologyDrop {
        database: "neo4j".into(),
    }];
    let proof = wal.preflight_encoded(1, 1, encode(&ops).unwrap()).unwrap();
    wal.append_preflighted(proof).unwrap();
    assert_eq!(wal.active_magic, *ONTOLOGY_MAGIC);
}

use relate_test_support::peak_pool::FixedPool;

#[test]
fn the_charge_follows_the_buffer_and_is_released_with_it() {
    let pool = Arc::new(FixedPool::new(64 * 1024 * 1024));
    let mut ledger = relate_core::tokens::TokenLedger::new(pool.capacity, 0, pool.clone());
    let _installed = relate_core::tokens::LedgerGuard::install(&mut ledger);
    let ops = [big_string_op(300_000)];
    let mut charge = WorkspaceCharge::empty();
    let encoded = EncodedOps::encode(&ops, &mut charge).unwrap();
    // Doubling from 64 KiB: 512 KiB holds the 300 kB record.
    assert_eq!(encoded.reservation_bytes(), 512 * 1024);
    assert_eq!(charge.bytes(), encoded.reservation_bytes());
    assert_eq!(pool.reserved(), charge.bytes());
    drop(charge);
    assert_eq!(pool.reserved(), 0);
}

#[test]
fn a_refused_charge_aborts_the_encoding_and_releases_what_it_held() {
    let pool = Arc::new(FixedPool::new(256 * 1024));
    let mut ledger = relate_core::tokens::TokenLedger::new(pool.capacity, 0, pool.clone());
    let _installed = relate_core::tokens::LedgerGuard::install(&mut ledger);
    let mut charge = WorkspaceCharge::empty();
    let err = EncodedOps::encode(&[big_string_op(1 << 20)], &mut charge).unwrap_err();
    assert!(
        matches!(
            err,
            Error::ResourceLimit { name, .. } if name == Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT
        ),
        "{err:?}"
    );
    // The refused growth never raised the charge past a step it was granted.
    assert!(charge.bytes() <= pool.capacity, "{}", charge.bytes());
    drop(charge);
    assert_eq!(pool.reserved(), 0);
}

#[test]
fn the_payload_cap_refuses_before_any_charge_for_the_crossing_growth() {
    // A budget that admits the whole cap still yields the cap's own
    // refusal and figures, not a workspace refusal.
    let cap = Error::WAL_PAYLOAD_CAP_BYTES;
    let pool = Arc::new(FixedPool::new(4 * cap));
    let mut ledger = relate_core::tokens::TokenLedger::new(pool.capacity, 0, pool.clone());
    let _installed = relate_core::tokens::LedgerGuard::install(&mut ledger);
    let base = reference_count(u64::MAX, u64::MAX, &[big_string_op(0)], u64::MAX).unwrap();
    let ops = [big_string_op((cap - base) as usize + 1)];
    let crossed = reference_count(u64::MAX, u64::MAX, &ops, cap).unwrap_err();
    let mut charge = WorkspaceCharge::empty();
    let err = EncodedOps::encode(&ops, &mut charge).unwrap_err();
    assert_eq!(
        err,
        Error::Capacity {
            used: crossed,
            limit: cap,
            kind: CapacityKind::WalPayload,
        }
    );
    drop(charge);
    assert_eq!(pool.reserved(), 0);
}

#[test]
fn a_group_preflight_checks_each_record_against_its_predecessor() {
    let wal = Wal::memory();
    let group = |stamps: &[u64]| {
        stamps
            .iter()
            .map(|ts| (*ts, encode(&[]).unwrap()))
            .collect::<Vec<_>>()
    };
    // Each timestamp advances only past the log's tip, but the group's own
    // second record does not advance past its first.
    let err = wal.preflight_group(group(&[5, 5])).err().unwrap();
    assert!(
        err.to_string().contains("must advance past 5"),
        "the group's second record is checked against the first: {err}"
    );
    assert_eq!(wal.lsn, 0);
    let proofs = wal.preflight_group(group(&[5, 6, 9])).unwrap();
    assert_eq!(proofs.len(), 3);
    assert_eq!(proofs[2].commit_ts(), 9);
}
