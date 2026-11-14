//! The engine's commit bookmarks (ADR-0502 §7): the commit sequence the
//! WAL boundary already assigns (`try_wal_commit`), threaded out of the
//! auto-commit write path (`QueryResult::bookmark`) and the
//! explicit-transaction commit (`Connection::commit`) in ADR-0310's
//! `relate:<n>` form — monotonically increasing per store, and absent
//! (a typed `Option::None`, never a sentinel) for everything that commits
//! nothing.

use crate::Database;

/// The `relate:<n>` sequence number of a bookmark — the module's one
/// parse, so every assertion reads the same ADR-0310 form.
fn sequence(bookmark: Option<&str>, label: &str) -> u64 {
    let Some(bookmark) = bookmark else {
        panic!("{label}: expected a bookmark, found none");
    };
    crate::commit::bookmark_sequence(bookmark)
        .unwrap_or_else(|| panic!("{label}: not the ADR-0310 form `relate:<n>`: {bookmark}"))
}

/// Two sequential auto-commit writes carry strictly increasing bookmarks,
/// and the sequence is the store's, not one connection's: a write through
/// a second connection continues past the first's.
#[test]
fn sequential_writes_return_strictly_increasing_bookmarks() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let first = sequence(
        conn.query("CREATE (:Kept)").unwrap().bookmark.as_deref(),
        "first write",
    );
    let second = sequence(
        conn.query("CREATE (:Kept)").unwrap().bookmark.as_deref(),
        "second write",
    );
    assert!(
        second > first,
        "the commit sequence is monotonic: {second} vs {first}"
    );

    // Per store, not per connection: another connection's write sorts
    // after both.
    let mut other = db.connect().unwrap();
    let third = sequence(
        other.query("CREATE (:Kept)").unwrap().bookmark.as_deref(),
        "another connection's write",
    );
    assert!(third > second);
}

/// Everything that commits nothing carries no bookmark: a read, a write
/// whose row pipeline was empty (it attempted a mutation and applied
/// none), and an `EXPLAIN` (nothing executes).
#[test]
fn statements_that_commit_nothing_carry_no_bookmark() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:Kept)").unwrap();

    assert!(conn.query("RETURN 1 AS one").unwrap().bookmark.is_none());
    assert!(conn
        .query("MATCH (n:Kept) RETURN n")
        .unwrap()
        .bookmark
        .is_none());

    let noop = conn.query("MATCH (n:Absent) DELETE n").unwrap();
    assert!(
        noop.effect.attempted(),
        "the no-op write did run a mutation"
    );
    assert!(!noop.effect.applied());
    assert!(
        noop.bookmark.is_none(),
        "a write that applied nothing crossed no WAL boundary"
    );

    assert!(
        conn.query("EXPLAIN CREATE (:Kept)")
            .unwrap()
            .bookmark
            .is_none(),
        "EXPLAIN never executes, so nothing commits"
    );
}

/// The explicit transaction's commit returns the bookmark; the
/// transaction's own statements carry none (their commit boundary is the
/// `commit` call), an empty transaction commits nothing, and the sequence
/// continues past the transaction's commit.
#[test]
fn explicit_commit_returns_the_bookmark_and_empty_commits_none() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let auto = sequence(
        conn.query("CREATE (:Kept)").unwrap().bookmark.as_deref(),
        "auto-commit write",
    );

    conn.begin().unwrap();
    assert!(
        conn.query("CREATE (:Kept)").unwrap().bookmark.is_none(),
        "an in-transaction statement commits at `commit`, not at the statement"
    );
    let committed = sequence(conn.commit().unwrap().as_deref(), "transaction commit");
    assert!(
        committed > auto,
        "the transaction's commit sorts after the earlier auto-commit: {committed} vs {auto}"
    );

    conn.begin().unwrap();
    assert_eq!(
        conn.commit().unwrap(),
        None,
        "an empty transaction crossed no WAL boundary, so no bookmark is fabricated"
    );

    let after = sequence(
        conn.query("CREATE (:Kept)").unwrap().bookmark.as_deref(),
        "write after the commits",
    );
    assert!(after > committed);
}

/// The batched paths keep the bookmark: a `CALL { … } IN TRANSACTIONS`
/// statement commits its batches mid-statement through the sink
/// (ADR-0210), so with no final redo its bookmark is the last batch's
/// commit; a `PROFILE` write's bookmark survives the epilogue that
/// attaches the plan.
#[test]
fn batched_and_profiled_writes_carry_bookmarks() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let seed = sequence(
        conn.query("UNWIND range(1, 25) AS i CREATE (:N {i: i})")
            .unwrap()
            .bookmark
            .as_deref(),
        "seed write",
    );

    let batched = conn
        .query("MATCH (n) CALL { WITH n DETACH DELETE n } IN TRANSACTIONS OF 10 ROWS")
        .unwrap();
    let batched = sequence(batched.bookmark.as_deref(), "batched write");
    assert!(
        batched > seed,
        "the last batch's commit sorts after the seed write: {batched} vs {seed}"
    );

    let profiled = conn.query("PROFILE CREATE (:Kept)").unwrap();
    let profiled = sequence(profiled.bookmark.as_deref(), "profiled write");
    assert!(profiled > batched);
}
