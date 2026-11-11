//! The group append refuses a batch whose own timestamps do not advance
//! before writing any frame, and reports each record by its timestamp.

use relate_storage::{EncodedOps, Wal};
use relate_write_local::{append_assigned, AssignedRecord};

fn record(commit_ts: u64) -> AssignedRecord {
    AssignedRecord {
        commit_ts,
        encoded: EncodedOps::encode_unmetered(&[]).unwrap(),
    }
}

#[test]
fn a_non_advancing_batch_writes_nothing() {
    let mut wal = Wal::memory();
    assert!(append_assigned(&mut wal, vec![record(4), record(4)]).is_err());
    assert_eq!(wal.lsn, 0);
    assert!(wal.replay(0).unwrap().is_empty());
}

#[test]
fn each_appended_record_carries_its_own_timestamp() {
    let mut wal = Wal::memory();
    let appended = append_assigned(&mut wal, vec![record(4), record(7)]).unwrap();
    let seen: Vec<(u64, u64)> = appended.iter().map(|a| (a.lsn, a.commit_ts)).collect();
    assert_eq!(seen, [(1, 4), (2, 7)]);
}
