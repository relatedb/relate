//! LABS-1658: the sealed-adjacency tier's memory win survives a store
//! reopen. A checkpoint's GC pass folds the chunks at or above the write
//! path's 32-hop floor; the wholesale rebuild a reopen runs emits folded
//! chunks in its own pass (ADR-0608), gated at the tier's 4-hop break-even
//! (ADR-0599). The gap between the two gates is the accepted drift, so the
//! reopened charge lands at or below the checkpointed one — on the
//! checkpoint's own figure whenever every chunk sits over its floor.

use relate::Database;
use relate_test_support::uri;

/// Node 0 plus `hops` targets, and node 0's `:R` out-chunk to each of them
/// — the target side's in-chunks stay at one hop, below every gate.
fn build(db: &Database, hops: usize) {
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:N {id: 0})").unwrap();
    conn.query(&format!(
        "UNWIND range(1, {hops}) AS i CREATE (:N {{id: i}})"
    ))
    .unwrap();
    conn.query(&format!(
        "MATCH (a:N {{id: 0}}) UNWIND range(1, {hops}) AS i \
         MATCH (b:N {{id: i}}) CREATE (a)-[:R]->(b)"
    ))
    .unwrap();
}

/// A store's live logical charge as its checkpoint left it, and as the
/// reopen's build leaves it: `(checkpointed, reopened)`.
fn charges(hops: usize) -> (u64, u64) {
    let dir = tempfile::tempdir().unwrap();
    let store = uri(dir.path());
    let closed = {
        let db = Database::open(store).unwrap();
        build(&db, hops);
        // The checkpoint's GC pass folds what the write path would fold.
        db.checkpoint().unwrap();
        db.memory_usage().unwrap().0
    };
    let db = Database::open(store).unwrap();
    (closed, db.memory_usage().unwrap().0)
}

#[test]
fn reopening_a_checkpointed_store_keeps_the_folded_chunks() {
    // A live writer maintains its count structures incrementally and a
    // rebuild derives them wholesale, so the two charges differ by a small
    // store-level term of their own — measured here on a store whose single
    // chunk sits below every gate, where the reopen changes no layout. The
    // claims below are about the adjacency alone.
    let (closed, reopened) = charges(1);
    let store_term = closed - reopened;

    // Every chunk over the write path's 32-hop floor: the checkpoint folded
    // it and the reopen's build folds it too, so the two layouts agree and
    // the reopen charges the checkpoint's figure, less nothing but that
    // store-level term.
    let (closed, reopened) = charges(64);
    assert!(
        reopened <= closed,
        "the reopen charges more than the checkpoint left ({reopened} vs {closed})"
    );
    assert_eq!(
        closed - reopened,
        store_term,
        "the reopen did not rebuild the checkpointed folded shape (LABS-1658)"
    );

    // Between the build's 4-hop gate and the write floor: the checkpoint
    // left the chunk delta, the reopen folds it — the accepted drift, a
    // strictly smaller charge.
    let (closed, reopened) = charges(8);
    assert!(
        reopened <= closed,
        "the reopen charges more than the checkpoint left ({reopened} vs {closed})"
    );
    assert!(
        closed - reopened > store_term,
        "the reopen must fold the shorter chunk the checkpoint left delta \
         ({} vs {store_term})",
        closed - reopened
    );
}
