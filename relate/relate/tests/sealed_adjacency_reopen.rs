//! LABS-1658: the sealed-adjacency tier's memory win survives a store
//! reopen. A checkpoint's GC pass folds the chunks at or above the seal
//! floor; the wholesale rebuild a reopen runs emits that folded shape
//! directly (ADR-0608), instead of re-deriving every chunk as delta and
//! leaving the tier's win to come back only at the next checkpoint.

use relate::Database;
use relate_executor::take_sealed_builds;
use relate_test_support::uri;

/// A generous count above the seal floor — the floor's exact value and its
/// boundary already live in the executor's own tests, and the store tier's
/// tuning knob is not this pin's business.
const HOPS: usize = 64;

/// One node with an above-floor out-chunk — the smallest the tier folds.
/// The targets each carry a single in-hop, below the floor.
fn build(db: &Database) {
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:N {id: 0})").unwrap();
    conn.query(&format!(
        "UNWIND range(1, {HOPS}) AS i CREATE (:N {{id: i}})"
    ))
    .unwrap();
    conn.query(&format!(
        "MATCH (a:N {{id: 0}}) UNWIND range(1, {HOPS}) AS i \
         MATCH (b:N {{id: i}}) CREATE (a)-[:R]->(b)"
    ))
    .unwrap();
}

#[test]
fn reopening_a_checkpointed_store_keeps_the_folded_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let store = uri(dir.path());
    {
        let db = Database::open(store).unwrap();
        build(&db);
        // The checkpoint's GC pass is the one moment a live store folds.
        db.checkpoint().unwrap();
    }
    let folded = {
        // Reset the seam, then reopen: the wholesale rebuild must emit the
        // same folded chunk the checkpoint's GC pass left. The charge is
        // the ADR's measured claim, not this pin's — the representation is.
        let before = take_sealed_builds();
        assert_eq!(before, 0, "the seam starts clean");
        let db = Database::open(store).unwrap();
        drop(db);
        take_sealed_builds()
    };
    assert_eq!(
        folded, 1,
        "the reopen did not build the folded chunk (LABS-1658)"
    );
}
