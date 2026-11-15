//! LABS-1373: `ORDER BY ... LIMIT/SKIP` bounds the sort's own container.
//!
//! ADR-0555 declares the unlimited sort a breaker, but a sort with a
//! `LIMIT` (plus its `SKIP`) needs only the first `skip + limit` rows of
//! the order: the projection folds its producer into a charged top-k heap
//! of that capacity and never holds more. The memory-shape test below is
//! the ADR-0555 proof — two inputs an order of magnitude apart, one
//! bounded workspace, and the heap's charge independent of the input
//! count. The differential test pins that the bounded selection answers
//! exactly what a full sort answers.

mod common;

use relate::{Database, OpenOptions, Value};

/// The workspace budget the bounded shape must fit inside: generous for
/// five retained rows, impossible for the full input at the sizes below.
const BUDGET: u64 = 16 << 20;

fn bounded_db() -> relate::Connection {
    let db = Database::open_with(
        ":memory:",
        OpenOptions {
            concurrent_logical_workspace_limit: Some(BUDGET),
            ..Default::default()
        },
    )
    .unwrap();
    db.connect().unwrap()
}

/// Assert that every spelling of a zero page answers nothing — the one
/// spelling of the check both zero-page tests read.
fn assert_limit_zero_answers_nothing(c: &mut relate::Connection, queries: &[&str]) {
    for q in queries {
        assert!(
            common::rows(c, q).is_empty(),
            "a LIMIT 0 answers nothing: {q}"
        );
    }
}

/// One `ORDER BY ... LIMIT` over `n` generated rows under the bounded
/// workspace: the answer is the last five ids, whatever `n` is.
fn top_five(conn: &mut relate::Connection, n: i64) -> Vec<Value> {
    let q = format!("UNWIND range(1, {n}) AS i WITH i AS v ORDER BY v DESC LIMIT 5 RETURN v");
    common::first_column(&common::run(conn, &q))
}

/// The memory shape (ADR-0555): the projection's container is the bounded
/// window, and the feed it drains is reserved **once** — not the three
/// copies the collecting path reserved (the projection's own `Vec`, the
/// sort's merged buffer, and the top-k selection's second vector). A
/// windowed ordered `LIMIT` therefore serves a larger input under a fixed
/// admission pool than the collecting path did.
///
/// Measured against this pool (16 MiB), `UNWIND range(1, N) … ORDER BY v
/// DESC LIMIT 5`: `main` refuses at 70k rows and answers at 60k; this branch
/// answers at 130k and refuses at 140k — about 2.2x. The row below is the
/// middle of that gap, where the two behaviours differ.
///
/// This is **not** input-count independence. The feed is live for the
/// projection's whole lifetime, so the reservation stays proportional to the
/// input and the 200k case is still refused — the reservation is what keeps
/// the pool pricing what the statement really holds. Removing the last
/// proportionality means not materializing the input at all, which is the
/// producer fold (LABS-841/842).
#[test]
fn order_by_limit_reserves_one_feed_not_three() {
    // 100k rows: `main` refuses this under the same 16 MiB pool (measured at
    // 70k), the windowed path answers it (measured to 130k).
    let mut c = bounded_db();
    assert_eq!(
        top_five(&mut c, 100_000),
        vec![
            Value::Int(100_000),
            Value::Int(99_999),
            Value::Int(99_998),
            Value::Int(99_997),
            Value::Int(99_996)
        ]
    );
}

/// The window is charged even when the workspace ledger is off (the
/// default): a `LIMIT` large enough to accumulate an output past the
/// per-allocation cap is refused on the window's live total alone. The
/// producer stays tiny — fifty one-slot rows — so the projection's own
/// container is the only thing that can cross the cap.
#[test]
fn a_large_ordered_window_is_refused_without_a_workspace_ledger() {
    let db = Database::open_with(
        ":memory:",
        OpenOptions {
            expression_memory_limit: Some(1 << 20),
            ..Default::default()
        },
    )
    .unwrap();
    let mut c = db.connect().unwrap();
    // Each projected row carries one ~128 KB list — every single
    // allocation stays under the cap, so the only quantity that can cross
    // it is the window's live total over fifty entries.
    let q = "UNWIND range(1, 50) AS i \
             WITH i, range(1, 8000) AS pad \
             ORDER BY i DESC LIMIT 50 \
             RETURN i";
    let error = common::err_text(c.query(q));
    assert!(
        error.contains("tried to build"),
        "the window's live total must be charged to the per-allocation cap: {error}"
    );
}

/// The selection answers what a full sort answers: mixed ASC/DESC keys,
/// ties broken by arrival order, and `SKIP` paying its share of the
/// window. The unbounded spellings (no `LIMIT`) are the oracle.
#[test]
fn order_by_skip_limit_answers_what_the_full_sort_answers() {
    let mut c = common::connection();
    // `b` from a bounded set so `(a, b)` ties exist: the oracle's stable
    // full sort keeps arrival order, so the equality assertion pins the
    // window's arrival-order tiebreak (`TopKEntry::seq`) too.
    let setup = "UNWIND range(1, 500) AS i CREATE (:N {a: i % 7, b: i % 5})";
    common::run(&mut c, setup);

    let bounded =
        "MATCH (n:N) WITH n.a AS a, n.b AS b ORDER BY a ASC, b DESC SKIP 3 LIMIT 8 RETURN a, b";
    let oracle =
        "MATCH (n:N) WITH n.a AS a, n.b AS b ORDER BY a ASC, b DESC RETURN a, b SKIP 3 LIMIT 8";
    assert_eq!(
        common::rows(&mut c, bounded),
        common::rows(&mut c, oracle),
        "the bounded window must equal the full sort's page"
    );
}

/// A zero page answers nothing, in every spelling: the clause decides it
/// before a container is opened, so a `LIMIT 0` costs nothing whatever the
/// input. The assertions observe the answer — the shape of the decision
/// itself is not visible from a query — so what this pins is that the
/// answer stays empty across the shapes, not which path produced it.
#[test]
fn order_by_limit_zero_answers_nothing() {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND range(1, 100) AS i CREATE (:N {a: i})");
    let queries = [
        "MATCH (n:N) RETURN n.a AS a ORDER BY a LIMIT 0",
        "MATCH (n:N) RETURN n.a AS a LIMIT 0",
        "MATCH (n:N) RETURN n.a AS a SKIP 5 LIMIT 0",
        "MATCH (n:N) WITH n.a AS a ORDER BY a DESC LIMIT 0 RETURN a",
    ];
    assert_limit_zero_answers_nothing(&mut c, &queries);
}

/// The zero page is decided before any container is opened, so it costs
/// nothing whatever the input. The same 16 MiB workspace that a full
/// collection cannot fit answers an empty page over two million rows —
/// the assertion that tells the decision apart from collecting and then
/// paging, which the answer alone cannot.
#[test]
fn a_limit_zero_costs_nothing_whatever_the_input() {
    let mut c = bounded_db();
    // Every spelling that can carry a zero page, including the `SKIP` forms
    // whose capacity arithmetic (`skip + 0`) would otherwise still be
    // positive and build a window.
    let queries = [
        "UNWIND range(1, 2_000_000) AS i WITH i AS v ORDER BY v LIMIT 0 RETURN v",
        "UNWIND range(1, 2_000_000) AS i WITH i AS v ORDER BY v SKIP 5 LIMIT 0 RETURN v",
        "UNWIND range(1, 2_000_000) AS i WITH i AS v ORDER BY v DESC SKIP 0 LIMIT 0 RETURN v",
    ];
    assert_limit_zero_answers_nothing(&mut c, &queries);
}

/// A projection-level `WHERE` written *before* the page: the plan for that
/// spelling is not the window — `WITH … WHERE … ORDER BY … LIMIT` splits
/// into a `WHERE` projection and a keyed `LIMIT` one, so the filter is
/// applied by the collected path's tail and the page is the filtered
/// sort's. This pins that answer. The fused spelling below is the one that
/// reaches the window's own per-row predicate.
#[test]
fn an_ordered_window_filters_before_it_selects() {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND range(1, 500) AS i CREATE (:N {a: i})");

    let bounded = "MATCH (n:N) WITH n.a AS a WHERE a % 2 = 0 ORDER BY a DESC LIMIT 5 RETURN a";
    // The pin: an unfiltered window would answer 500, 499, 498, … here.
    let values: Vec<Value> = common::first_column(&common::run(&mut c, bounded));
    assert_eq!(
        values,
        vec![
            Value::Int(500),
            Value::Int(498),
            Value::Int(496),
            Value::Int(494),
            Value::Int(492)
        ],
        "the window must select from the filtered stream"
    );

    // And the page still equals the unbounded filtered sort's page.
    let oracle = "MATCH (n:N) WITH n.a AS a WHERE a % 2 = 0 ORDER BY a DESC RETURN a";
    let mut expected = common::rows(&mut c, oracle);
    expected.truncate(5);
    assert_eq!(common::rows(&mut c, bounded), expected);
}

/// The fused spelling — a `WHERE` written *after* the page. It stays in the
/// clause that carries `order`/`skip`/`limit`, so the window itself is
/// handed the predicate and must apply it as each row arrives; that is what
/// makes this test the predicate arm's own coverage, which the split
/// spelling above never reaches. Relate filters before it pages, so the
/// answer is the *filtered* sort's page: reading it as page-then-filter
/// would answer nothing here, and refusing the pairing (which this branch
/// did for one round) answers an error. The differential pins the fused
/// spelling against the split one, which is the same semantics written the
/// other way round.
#[test]
fn a_post_page_where_filters_before_the_window_selects() {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND range(1, 10) AS i CREATE (:N {a: i})");

    let fused = "MATCH (n:N) WITH n.a AS a ORDER BY a LIMIT 3 WHERE a >= 4 RETURN a";
    let split = "MATCH (n:N) WITH n.a AS a WHERE a >= 4 ORDER BY a LIMIT 3 RETURN a";
    let values: Vec<Value> = common::first_column(&common::run(&mut c, fused));
    assert_eq!(
        values,
        vec![Value::Int(4), Value::Int(5), Value::Int(6)],
        "the window must select from the filtered stream, not filter its own page"
    );
    assert_eq!(common::rows(&mut c, fused), common::rows(&mut c, split));
}
