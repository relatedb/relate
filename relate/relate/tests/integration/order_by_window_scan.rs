//! An `ORDER BY … LIMIT` window keys a candidate before it projects it, and an
//! id-ordered window over a bare node scan stops that scan once it is full.
//! Every shape must answer what the unwindowed sort answers, and a shape the
//! shortcuts do not cover must keep its own behavior: a projection error on a
//! row the window would reject still surfaces.

use crate::common;

fn ids(c: &mut relate::Connection, q: &str) -> Vec<i64> {
    common::first_column_ints(&common::run(c, q))
}

fn seeded() -> relate::Connection {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND range(1, 40) AS i CREATE (:N {k: i})");
    common::run(&mut c, "UNWIND range(1, 10) AS i CREATE (:M {k: i})");
    common::run(&mut c, "MATCH (n:N) WHERE n.k % 4 = 0 DETACH DELETE n");
    c
}

const PAGES: [&str; 5] = [
    "LIMIT 7",
    "SKIP 4 LIMIT 7",
    "SKIP 100 LIMIT 3",
    "LIMIT 1",
    "SKIP 29 LIMIT 5",
];

#[test]
fn an_id_ordered_labeled_window_answers_what_the_sort_answers() {
    let mut c = seeded();
    for page in PAGES {
        let fast = ids(
            &mut c,
            &format!("MATCH (n:N) RETURN id(n) AS i ORDER BY id(n) {page}"),
        );
        // The oracle orders a projected column: the alias-scoped key keeps the
        // whole row path, never the keyed window.
        let oracle = ids(
            &mut c,
            &format!("MATCH (n:N) WITH id(n) AS i ORDER BY i {page} RETURN i"),
        );
        assert_eq!(fast, oracle, "page {page:?}");
    }
}

#[test]
fn a_property_column_beside_an_id_key_is_the_kept_rows_property() {
    let mut c = seeded();
    for page in PAGES {
        let fast = ids(
            &mut c,
            &format!("MATCH (n:N) RETURN n.k AS k ORDER BY id(n) {page}"),
        );
        let oracle = ids(
            &mut c,
            &format!("MATCH (n:N) WITH n.k AS k, id(n) AS i ORDER BY i {page} RETURN k"),
        );
        assert_eq!(fast, oracle, "page {page:?}");
    }
}

#[test]
fn a_property_key_window_answers_what_the_sort_answers() {
    let mut c = seeded();
    for page in PAGES {
        let fast = ids(
            &mut c,
            &format!("MATCH (n:N) RETURN n.k AS k ORDER BY n.k DESC {page}"),
        );
        let oracle = ids(
            &mut c,
            &format!("MATCH (n:N) WITH n.k AS k ORDER BY k DESC {page} RETURN k"),
        );
        assert_eq!(fast, oracle, "page {page:?}");
    }
}

#[test]
fn shapes_with_more_than_one_stream_keep_the_full_scan() {
    let mut c = seeded();
    // Two input rows: each restarts the id-ascending scan, so the first
    // stream's full window must not end the second's.
    let q = "UNWIND [1, 2] AS x MATCH (n:N) RETURN id(n) AS i ORDER BY id(n) LIMIT 6";
    let oracle = "UNWIND [1, 2] AS x MATCH (n:N) WITH id(n) AS i ORDER BY i LIMIT 6 RETURN i";
    assert_eq!(ids(&mut c, q), ids(&mut c, oracle));
    // A comma join, a property map and a WHERE: not a bare node scan.
    for (q, oracle) in [
        (
            "MATCH (n:N), (m:M) RETURN id(n) AS i ORDER BY id(n) LIMIT 6",
            "MATCH (n:N), (m:M) WITH id(n) AS i ORDER BY i LIMIT 6 RETURN i",
        ),
        (
            "MATCH (n:N {k: 5}) RETURN id(n) AS i ORDER BY id(n) LIMIT 3",
            "MATCH (n:N {k: 5}) WITH id(n) AS i ORDER BY i LIMIT 3 RETURN i",
        ),
        (
            "MATCH (n:N) WHERE n.k > 20 RETURN id(n) AS i ORDER BY id(n) LIMIT 4",
            "MATCH (n:N) WHERE n.k > 20 WITH id(n) AS i ORDER BY i LIMIT 4 RETURN i",
        ),
    ] {
        assert_eq!(ids(&mut c, q), ids(&mut c, oracle), "{q}");
    }
}

#[test]
fn a_projection_error_on_a_rejected_row_still_surfaces() {
    let mut c = seeded();
    // k = 24 is deleted (24 % 4 = 0), so use a row the page would reject:
    // dividing by (k - 38) is zero at k = 38, past a LIMIT 2 page.
    let result = c.query("MATCH (n:N) RETURN 1 / (n.k - 38) AS x ORDER BY id(n) LIMIT 2");
    assert!(
        result.is_err(),
        "the window must not hide an error projection raises on a later row"
    );
}

#[test]
fn a_property_filtered_id_window_answers_what_the_sort_answers() {
    let mut c = seeded();
    // First the unindexed label scan with a residual filter, then the same
    // filters served by a property index (whose id order is not guaranteed
    // and so must fall back to the full scan, not stop early).
    for indexed in [false, true] {
        if indexed {
            common::run(&mut c, "CREATE INDEX n_k FOR (n:N) ON (n.k)");
        }
        for filter in [
            "n.k > 10",
            "n.k >= 10 AND n.k < 30",
            "n.k = 7",
            "n.k <> 7 AND n.k > 2",
            "n.k > 'x'",
        ] {
            for page in PAGES {
                let fast = ids(
                    &mut c,
                    &format!("MATCH (n:N) WHERE {filter} RETURN n.k AS k ORDER BY id(n) {page}"),
                );
                let oracle = ids(
                    &mut c,
                    &format!(
                        "MATCH (n:N) WHERE {filter} \
                         WITH n.k AS k, id(n) AS i ORDER BY i {page} RETURN k"
                    ),
                );
                assert_eq!(fast, oracle, "indexed {indexed}, {filter}, {page}");
            }
        }
    }
}

#[test]
fn a_filter_that_can_raise_on_a_later_row_still_raises() {
    let mut c = seeded();
    let result =
        c.query("MATCH (n:N) WHERE 1 / (n.k - 38) > 0 RETURN n.k AS k ORDER BY id(n) LIMIT 2");
    assert!(
        result.is_err(),
        "a WHERE that raises on a row past the page must still raise"
    );
}

#[test]
fn a_property_read_of_an_entity_deleted_earlier_still_raises() {
    let mut c = seeded();
    // `a` is deleted before the windowed MATCH: reading its property raises,
    // and the keyed window must not take the read for a cannot-raise one.
    let result = c.query(
        "MATCH (a:N {k: 1}) DELETE a WITH a MATCH (b:M) \
         RETURN a.k AS x ORDER BY id(b) LIMIT 2",
    );
    let error = common::err_text(result);
    assert!(
        error.contains("DeletedEntityAccess"),
        "a property read of a deleted node must still raise: {error}"
    );
}
