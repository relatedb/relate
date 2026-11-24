//! End-to-end differential for the tiered distinct set (ADR-0507 §9,
//! LABS-852): every DISTINCT site — `RETURN DISTINCT`, `WITH DISTINCT`,
//! `UNION`, `count(DISTINCT …)`, `collect(DISTINCT …)`, `coll.distinct` and
//! `DISTINCT … LIMIT n` — over seeded generated graphs, checked against a
//! reference computed here, not by a second executor.
//!
//! Each generated property is drawn from a vocabulary whose entries carry a
//! Cypher literal and a declared equivalence class (the `GroupKey`
//! contract: null is one key, every NaN is its own key, `1 ≡ 1.0 ≡` and
//! `0 ≡ 0.0 ≡ -0.0`, the two halves of a DST fall-back overlap are two
//! keys). The input stream is the same query without DISTINCT; the
//! reference keeps the first row of each class in stream order, so every
//! assertion pins both membership and first-seen order.
//!
//! The sizes cross every tier boundary of the set (15/16/17 distinct words,
//! a tag switch, past 2^7), on the word tier (one int / node column) and the
//! general / row tier (strings, temporals, points, lists, two columns).

mod common;

use std::collections::HashSet;

use common::{connection, rows};
use relate_test_support::rng::Rng;
use relate_test_support::temporal::{DST_OVERLAP_FIRST, DST_OVERLAP_SECOND};

/// One generated property: its Cypher literal and its equivalence class
/// (`None` for NaN, which equals nothing — not even itself).
#[derive(Clone)]
struct Entry {
    literal: String,
    class: Option<String>,
}

fn entry(literal: impl Into<String>, class: impl Into<String>) -> Entry {
    Entry {
        literal: literal.into(),
        class: Some(class.into()),
    }
}

fn nan() -> Entry {
    Entry {
        literal: "0.0 / 0.0".into(),
        class: None,
    }
}

/// The word-tier vocabulary: one `Int` tag — ints, integral floats, ±0.0.
fn int_entry(rng: &mut Rng, k: u64) -> Entry {
    let i = rng.below(k);
    match rng.below(4) {
        0 | 1 => entry(format!("{i}"), format!("num:{i}")),
        2 => entry(format!("{i}.0"), format!("num:{i}")),
        _ if i.is_multiple_of(2) => entry("0.0", "num:0"),
        _ => entry("-0.0", "num:0"),
    }
}

/// The mixed vocabulary: the int entries plus null, NaN, non-integral
/// floats, bools, strings, temporals (the DST pair among them) and points.
fn mixed_entry(rng: &mut Rng, k: u64) -> Entry {
    let i = rng.below(k);
    match rng.below(13) {
        0..=2 => int_entry(rng, k),
        3 => entry("null", "null"),
        4 => nan(),
        5 => entry(format!("{i}.25"), format!("float:{i}.25")),
        6 => entry(
            format!("{}", i.is_multiple_of(2)),
            format!("bool:{}", i.is_multiple_of(2)),
        ),
        7 => entry(format!("'s{}'", i % 23), format!("str:{}", i % 23)),
        8 => entry(
            format!("date('2020-01-{:02}')", i % 28 + 1),
            format!("date:{}", i % 28),
        ),
        9 => entry(
            format!("localdatetime('2020-01-01T{:02}:00:00')", i % 24),
            format!("ldt:{}", i % 24),
        ),
        10 => entry(
            format!("duration({{days: {}}})", i % 11),
            format!("dur:{}", i % 11),
        ),
        11 if i.is_multiple_of(2) => entry(DST_OVERLAP_FIRST, "dst:first"),
        11 => entry(DST_OVERLAP_SECOND, "dst:second"),
        _ => entry(
            format!("point({{x: {}, y: {}.0}})", i % 9, i % 2),
            format!("pt:{}:{}", i % 9, i % 2),
        ),
    }
}

/// The reference row key: `None` when any column is NaN (that row is
/// always new); a list row keys exactly like its columns (`[a, b]` and the
/// pair `a, b` share element semantics).
fn row_class(columns: &[Entry]) -> Option<Vec<String>> {
    columns.iter().map(|c| c.class.clone()).collect()
}

/// The keep/drop decision of a first-seen DISTINCT over `classes`.
fn first_seen(classes: &[Option<Vec<String>>]) -> Vec<bool> {
    let mut seen = HashSet::new();
    classes
        .iter()
        .map(|class| match class {
            None => true,
            Some(class) => seen.insert(class.clone()),
        })
        .collect()
}

/// Rows rendered for comparison (`Debug`: NaN renders equal to NaN and
/// `-0.0` stays apart from `0.0`, so the first-seen spelling is pinned).
fn render(rows: &[Vec<relate::Value>]) -> Vec<String> {
    rows.iter().map(|r| format!("{r:?}")).collect()
}

/// A vocabulary: one generated property drawn below `k`.
type Draw = fn(&mut Rng, u64) -> Entry;

/// One generated graph: `n` nodes `(:P {i, c0, c1, hub})` with `c0`/`c1`
/// drawn by `draw`, plus `hubs` `(:Q {qid})` and one `(:P)-[:R]->(:Q)` per
/// `P`, the hub drawn uniformly.
struct Graph {
    conn: relate::Connection,
    columns: Vec<[Entry; 2]>,
    hub_of: Vec<u64>,
}

fn build(seed: u64, n: usize, hubs: u64, draw: Draw, k: u64) -> Graph {
    let mut rng = Rng(seed);
    let mut conn = connection();
    let mut columns = Vec::with_capacity(n);
    let mut hub_of = Vec::with_capacity(n);
    for chunk in (0..n).collect::<Vec<_>>().chunks(64) {
        let mut patterns = Vec::new();
        for &i in chunk {
            let pair = [draw(&mut rng, k), draw(&mut rng, k)];
            let hub = rng.below(hubs);
            patterns.push(format!(
                "(:P {{i: {i}, c0: {}, c1: {}, hub: {hub}}})",
                pair[0].literal, pair[1].literal
            ));
            columns.push(pair);
            hub_of.push(hub);
        }
        conn.query(&format!("CREATE {}", patterns.join(", ")))
            .expect("create the generated nodes");
    }
    conn.query(&format!(
        "UNWIND range(0, {}) AS q CREATE (:Q {{qid: q}})",
        hubs - 1
    ))
    .expect("create the hubs");
    conn.query("MATCH (n:P), (q:Q) WHERE q.qid = n.hub CREATE (n)-[:R {rid: n.i}]->(q)")
        .expect("wire the hubs");
    Graph {
        conn,
        columns,
        hub_of,
    }
}

/// One query family: the stream as prefix/suffix parts around the filter,
/// the row-identity expression indexing the generated classes, and the
/// projection as (expression, alias) items — the select list, the alias
/// list, the single-column distinction and the aggregate operand all
/// derive from the items, never from re-parsing a rendered string.
struct Shape<'a> {
    name: &'a str,
    /// The stream ahead of the filter (`MATCH (n:P) …`).
    prefix: &'a str,
    /// The stream behind the filter (a `WITH` chain, an `UNWIND`, or
    /// empty).
    suffix: &'a str,
    /// The expression identifying the row: its value indexes the
    /// generated classes (`n.i`), whatever the engine's stream order is.
    id: &'a str,
    /// The projected items as (expression, alias).
    items: &'a [(&'a str, &'a str)],
}

impl Shape<'_> {
    fn stream(&self, filter: &str) -> String {
        format!("{} {} {}", self.prefix, filter, self.suffix)
    }

    /// The select list: `expr AS alias, …`.
    fn select_list(&self) -> String {
        self.items
            .iter()
            .map(|(e, a)| format!("{e} AS {a}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The aliases, comma-joined.
    fn aliases(&self) -> String {
        self.items
            .iter()
            .map(|(_, a)| *a)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The first-seen DISTINCT of the concatenated input streams (the same
/// streams without DISTINCT): the kept rows, in stream order.
fn reference(
    g: &mut Graph,
    shape: &Shape,
    filters: &[&str],
    classes: &[Option<Vec<String>>],
) -> Vec<Vec<relate::Value>> {
    let mut input = Vec::new();
    let mut input_classes = Vec::new();
    for filter in filters {
        let q = format!(
            "{} RETURN {} AS i_, {}",
            shape.stream(filter),
            shape.id,
            shape.select_list()
        );
        for mut row in rows(&mut g.conn, &q) {
            let i = match row.remove(0) {
                relate::Value::Int(i) => i as usize,
                other => panic!("[{}] {} is an int, got {other:?}", shape.name, shape.id),
            };
            input_classes.push(classes[i].clone());
            input.push(row);
        }
    }
    let keep = first_seen(&input_classes);
    filtered(input, &keep)
}

/// The items whose `keep` flag is set, in order.
fn filtered<T>(items: Vec<T>, keep: &[bool]) -> Vec<T> {
    items
        .into_iter()
        .zip(keep)
        .filter(|(_, k)| **k)
        .map(|(x, _)| x)
        .collect()
}

/// Every DISTINCT site over one shape, against the first-seen reference.
fn check(g: &mut Graph, shape: &Shape, classes: &[Option<Vec<String>>]) {
    let (s, p, a) = (shape.stream(""), shape.select_list(), shape.aliases());
    let expected = reference(g, shape, &[""], classes);
    let want = render(&expected);
    let name = shape.name;
    let mut same = |q: String, want: &[String]| {
        let got = render(&rows(&mut g.conn, &q));
        assert_eq!(got, want, "[{name}] {q}");
    };

    same(format!("{s} RETURN DISTINCT {p}"), &want);
    same(format!("{s} WITH DISTINCT {p} RETURN {a}"), &want);
    for limit in [1, 5, 15, 16, 17, want.len()] {
        let prefix = &want[..limit.min(want.len())];
        same(format!("{s} RETURN DISTINCT {p} LIMIT {limit}"), prefix);
        same(
            format!("{s} WITH DISTINCT {p} LIMIT {limit} RETURN {a}"),
            prefix,
        );
    }
    if let &[(e, _)] = shape.items {
        // The aggregates skip null, keep every NaN, and collect in
        // first-seen order.
        let non_null: Vec<relate::Value> = expected
            .iter()
            .filter(|r| !r[0].is_null())
            .map(|r| r[0].clone())
            .collect();
        let count = vec![vec![relate::Value::Int(non_null.len() as i64)]];
        let list = render(&[vec![relate::Value::List(non_null.into())]]);
        same(format!("{s} RETURN count(DISTINCT {e})"), &render(&count));
        same(format!("{s} RETURN collect(DISTINCT {e})"), &list);
        same(format!("{s} RETURN coll.distinct(collect({e}))"), &list);
    }
    // UNION: the two halves' streams, concatenated, first-seen.
    let half = classes.len() / 2;
    let (lo, hi) = (
        format!("WHERE {} < {half}", shape.id),
        format!("WHERE {} >= {half}", shape.id),
    );
    let union = reference(g, shape, &[&lo, &hi], classes);
    let q = format!(
        "{} RETURN {p} UNION {} RETURN {p}",
        shape.stream(&lo),
        shape.stream(&hi)
    );
    let got = render(&rows(&mut g.conn, &q));
    assert_eq!(got, render(&union), "[{name}] {q}");
}

/// One stream spelling: its name, the part ahead of the row filter, and
/// the part behind it.
type Stream<'a> = (&'a str, &'a str, &'a str);

/// The two streams every value shape runs over: an ordered one (DISTINCT
/// downstream of a `WITH … ORDER BY`) and a plain one (DISTINCT directly
/// after the MATCH, where `DISTINCT … LIMIT` rides the match cap).
const NODE_STREAMS: [Stream; 2] = [
    ("ordered", "MATCH (n:P)", "WITH n ORDER BY n.i"),
    ("plain", "MATCH (n:P)", ""),
];

/// The hub streams: the node `q` each `n` reaches, ordered and plain.
const HUB_STREAMS: [Stream; 2] = [
    (
        "ordered",
        "MATCH (n:P)",
        "WITH n ORDER BY n.i MATCH (n)-[:R]->(q:Q)",
    ),
    ("plain", "MATCH (n:P)-[:R]->(q:Q)", ""),
];

/// Check one projection over every stream of `streams`.
fn check_streams(
    g: &mut Graph,
    streams: &[Stream],
    name: &str,
    id: &str,
    items: &[(&str, &str)],
    classes: &[Option<Vec<String>>],
) {
    for (kind, prefix, suffix) in streams {
        let shape = Shape {
            name: &format!("{name} {kind}"),
            prefix,
            suffix,
            id,
            items,
        };
        check(g, &shape, classes);
    }
}

/// The graph sizes, as distinct-class targets `k` and node counts: both
/// sides of 16, the 15/16/17 boundary itself, and past 2^7.
const SIZES: [(u64, usize); 7] = [
    (8, 30),
    (15, 60),
    (16, 64),
    (17, 70),
    (40, 120),
    (130, 400),
    (300, 700),
];

fn one_column(g: &Graph, column: usize) -> Vec<Option<Vec<String>>> {
    g.columns
        .iter()
        .map(|c| row_class(&c[column..=column]))
        .collect()
}

/// One column at every size, as a value column and as the
/// `count`/`collect` operand, over both vocabularies: the word tier (one
/// `Int`-tagged column — ints, integral floats, ±0.0) and the general tier
/// (null, NaN, ±0.0, ints and floats, bools, strings, temporals with the
/// DST-overlap pair, and points).
#[test]
fn one_column_matches_the_reference_in_every_tier() {
    let vocabularies: [(&str, u64, Draw); 2] = [
        ("int", 0x852_0001, int_entry),
        ("mixed", 0x852_0300, mixed_entry),
    ];
    for (name, seed, draw) in vocabularies {
        for (round, (k, n)) in SIZES.into_iter().enumerate() {
            let mut g = build(seed + round as u64, n, 2, draw, k);
            let classes = one_column(&g, 0);
            check_streams(
                &mut g,
                &NODE_STREAMS,
                &format!("{name} k={k} n={n}"),
                "n.i",
                &[("n.c0", "v")],
                &classes,
            );
        }
    }
}

/// Word tier on engine ids: `DISTINCT q` over the hub each node reaches,
/// with hub counts on both sides of 16 and past 2^7, plus a relationship
/// column offered twice per row (`UNWIND [r, r]`).
#[test]
fn word_tier_node_and_rel_ids_match_the_reference() {
    for (round, hubs) in [3u64, 15, 16, 17, 40, 150].into_iter().enumerate() {
        let n = (hubs as usize) * 3 + 10;
        let mut g = build(0x852_0100 + round as u64, n, hubs, int_entry, 4);
        let classes: Vec<_> = g
            .hub_of
            .iter()
            .map(|h| Some(vec![format!("q:{h}")]))
            .collect();
        check_streams(
            &mut g,
            &HUB_STREAMS,
            &format!("hub hubs={hubs} n={n}"),
            "n.i",
            &[("q", "q")],
            &classes,
        );
        // Each relationship is offered twice: the distinct stream is every
        // relationship once, in node order.
        let rel_classes: Vec<_> = (0..n as i64)
            .map(|i| Some(vec![format!("rel:{i}")]))
            .collect();
        check_streams(
            &mut g,
            &[(
                "ordered",
                "MATCH (n:P)",
                "WITH n ORDER BY n.i MATCH (n)-[r:R]->() UNWIND [r, r] AS x",
            )],
            &format!("rels hubs={hubs} n={n}"),
            "n.i",
            &[("x", "x")],
            &rel_classes,
        );
    }
}

/// A tag switch: the column holds ints only until a bool arrives, which
/// moves the word tier to its per-tag tables before (k = 3) and after
/// (k = 16, 17) the inline capacity fills.
#[test]
fn a_mixed_type_column_forcing_the_tag_switch_matches_the_reference() {
    for (round, ints_first) in [3usize, 16, 17, 40].into_iter().enumerate() {
        let mut g = build(0x852_0200 + round as u64, ints_first * 3, 2, int_entry, 64);
        let n = g.columns.len() as i64;
        // Rewrite c0 in node order: distinct ints first, then a bool and
        // the ints again, then the other bool.
        let tail = ints_first as i64;
        g.conn
            .query(&format!(
                "MATCH (n:P) SET n.c0 = CASE \
                   WHEN n.i < {tail} THEN n.i \
                   WHEN n.i = {tail} THEN true \
                   WHEN n.i = {n} - 1 THEN false \
                   ELSE (n.i - {tail}) % {tail} END"
            ))
            .expect("rewrite c0");
        let classes: Vec<_> = (0..n)
            .map(|i| {
                let class = match i {
                    i if i < tail => format!("num:{i}"),
                    i if i == tail => "bool:true".to_string(),
                    i if i == n - 1 => "bool:false".to_string(),
                    i => format!("num:{}", (i - tail) % tail),
                };
                Some(vec![class])
            })
            .collect();
        check_streams(
            &mut g,
            &NODE_STREAMS,
            &format!("tag switch after {ints_first}"),
            "n.i",
            &[("n.c0", "v")],
            &classes,
        );
    }
}

/// Row tier: two columns (`DISTINCT a, b`) and the same pair as a list
/// (`DISTINCT [a, b]`, a general single key), over both vocabularies.
#[test]
fn row_tier_two_columns_and_lists_match_the_reference() {
    let vocabularies: [(&str, Draw); 2] = [("int", int_entry), ("mixed", mixed_entry)];
    for (name, draw) in vocabularies {
        for (round, (k, n)) in SIZES.into_iter().enumerate() {
            // A narrower draw so pairs repeat at every size.
            let k = (k / 3).max(2);
            let mut g = build(0x852_0400 + round as u64, n, 2, draw, k);
            let classes: Vec<_> = g.columns.iter().map(|c| row_class(c)).collect();
            check_streams(
                &mut g,
                &NODE_STREAMS,
                &format!("{name} pair k={k} n={n}"),
                "n.i",
                &[("n.c0", "a"), ("n.c1", "b")],
                &classes,
            );
            check_streams(
                &mut g,
                &NODE_STREAMS,
                &format!("{name} list k={k} n={n}"),
                "n.i",
                &[("[n.c0, n.c1]", "l")],
                &classes,
            );
        }
    }
}

/// The DST fall-back pair through every site: `=` holds, yet DISTINCT
/// keeps both halves (TCK TemporalCompare; ADR-0446).
#[test]
fn the_dst_overlap_pair_stays_two_rows_at_every_site() {
    let mut c = connection();
    c.query(&format!(
        "CREATE (:P {{i: 0, c0: {DST_OVERLAP_FIRST}}}), (:P {{i: 1, c0: {DST_OVERLAP_SECOND}}}), \
         (:P {{i: 2, c0: {DST_OVERLAP_FIRST}}}), (:P {{i: 3, c0: {DST_OVERLAP_SECOND}}})"
    ))
    .unwrap();
    let eq = rows(
        &mut c,
        &format!("RETURN {DST_OVERLAP_FIRST} = {DST_OVERLAP_SECOND} AS eq"),
    );
    assert_eq!(eq, vec![vec![relate::Value::Bool(true)]]);
    let s = "MATCH (n:P) WITH n ORDER BY n.i";
    for (q, want) in [
        (format!("{s} RETURN DISTINCT n.c0"), 2),
        (format!("{s} WITH DISTINCT n.c0 AS v RETURN v"), 2),
        (format!("{s} RETURN DISTINCT n.c0 LIMIT 5"), 2),
    ] {
        assert_eq!(rows(&mut c, &q).len(), want, "{q}");
    }
    let count = rows(&mut c, &format!("{s} RETURN count(DISTINCT n.c0)"));
    assert_eq!(count, vec![vec![relate::Value::Int(2)]]);
}
