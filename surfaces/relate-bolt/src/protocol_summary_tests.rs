//! Result summary metadata (ADR-0309) and bookmarks (ADR-0310), over the
//! wire and at the `summary` builders.

use std::net::TcpStream;
use std::sync::Arc;

use super::*;
use crate::dialect::Dialect;
use crate::fields::NotificationConfig;
use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    bookmark_sequence, connected_client, connected_client_with, field, keys, map, read_msg,
    read_records, send_begin, send_commit, send_pull, send_rollback, send_run, send_run_at,
    success_map,
};
use crate::value_codec::ValueCodec;
use crate::version::BoltVersion;

/// The ADR-0491 phase keys, in their wire order beside `t_last`.
const PHASE_KEYS: [&str; 3] = ["parsing_time", "planning_time", "plan_execution_time"];

/// LABS-1120: the fine sub-split keys (sub-windows of the coarse three,
/// the same Float-seconds family) — never part of the coarse sum
/// invariant, which counts each window once.
const FINE_PHASE_KEYS: [&str; 4] = [
    "lex_parse_time",
    "rewrite_time",
    "lower_time",
    "result_generation_time",
];

/// One ending SUCCESS phase key's Float seconds, loud on any other shape
/// (the ADR-0491 contract keys are doubles — an Int under them is a
/// unit mix, not a value to coalesce).
fn phase_seconds(ending: &Pack, key: &str) -> f64 {
    match ending.get(key) {
        Some(Pack::Float(seconds)) => *seconds,
        other => panic!("{key} must be PackStream Float seconds, got {other:?} in {ending:?}"),
    }
}

/// RUN `cypher`, then PULL or DISCARD (`message`) with `n = -1`; the RUN
/// SUCCESS map and the ending SUCCESS map.
fn run_to_end(client: &mut TcpStream, cypher: &str, message: Message) -> (Pack, Pack) {
    send_run(client, cypher);
    let run = Pack::Map(success_map(read_msg(client)));
    send_pull(client, message, vec![("n", Pack::Int(-1))]);
    let (_, ending) = read_records(client);
    (run, Pack::Map(success_map(ending)))
}

/// A `stats` map: these counters, then `contains-updates: true`.
fn stats_map(counters: &[(&str, i64)]) -> Pack {
    let mut entries: Vec<(&str, Pack)> = counters
        .iter()
        .map(|(key, count)| (*key, Pack::Int(*count)))
        .collect();
    entries.push(("contains-updates", Pack::Bool(true)));
    map(entries)
}

fn metadata_keys(m: &Pack) -> Vec<&str> {
    let Pack::Map(entries) = m else {
        panic!("not a map: {m:?}");
    };
    keys(entries)
}

/// F-11, F-13: RUN SUCCESS is exactly fields, columnTypes and t_first —
/// no `type` (ADR-0162 item 3 reversed), no `qid` outside a transaction.
#[test]
fn run_success_is_fields_column_types_and_t_first() {
    let mut client = connected_client();
    send_run(&mut client, "RETURN 1 AS x");
    let run = success_map(read_msg(&mut client));
    assert_eq!(keys(&run), ["fields", "columnTypes", "t_first"]);
    let run = Pack::Map(run);
    assert_eq!(
        run.get("fields"),
        Some(&Pack::List(vec![Pack::String("x".into())]))
    );
    assert!(matches!(run.get("t_first"), Some(Pack::Int(ms)) if *ms >= 0));
}

/// F-18: a PULL that leaves records unread answers exactly
/// `{has_more: true}`.
#[test]
fn a_page_that_leaves_records_is_exactly_has_more() {
    let mut client = connected_client();
    send_run(&mut client, "UNWIND [1, 2] AS x RETURN x");
    success_map(read_msg(&mut client));
    for message in [Message::Pull, Message::Discard] {
        // One PULL page, then one DISCARD page of a fresh result.
        if message == Message::Discard {
            send_run(&mut client, "UNWIND [1, 2] AS x RETURN x");
            success_map(read_msg(&mut client));
        }
        send_pull(&mut client, message, vec![("n", Pack::Int(1))]);
        let (_, page) = read_records(&mut client);
        assert_eq!(
            success_map(page),
            vec![(Pack::String("has_more".into()), Pack::Bool(true))]
        );
    }
}

/// ADR-0309 paging: a PULL `n` sends at most that many RECORDs and answers
/// `has_more: true` while rows remain; a drain PULL ends the result without
/// the key.
#[test]
fn pull_sends_at_most_n_records_and_reports_has_more() {
    let mut client = connected_client();
    send_run(&mut client, "UNWIND [1,2,3,4,5] AS x RETURN x");
    success_map(read_msg(&mut client));

    // PULL n=2: exactly two RECORDs then SUCCESS with `has_more: true`.
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(2))]);
    let (records, page) = read_records(&mut client);
    assert_eq!(records, vec![vec![Pack::Int(1)], vec![Pack::Int(2)]]);
    let page = success_map(page);
    assert!(
        page.iter()
            .any(|(k, v)| k.as_str() == Some("has_more") && *v == Pack::Bool(true)),
        "has_more must be true while rows remain: {page:?}"
    );

    // PULL n=-1 drains the remaining three.
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    let (records, ending) = read_records(&mut client);
    assert_eq!(
        records,
        vec![vec![Pack::Int(3)], vec![Pack::Int(4)], vec![Pack::Int(5)]]
    );
    let ending = success_map(ending);
    assert!(
        !ending.iter().any(|(k, _)| k.as_str() == Some("has_more")),
        "has_more must be absent once drained: {ending:?}"
    );
}

/// F-13, F-19, D-55: the ending SUCCESS of an auto-commit read, by PULL
/// or by DISCARD, is type "r", t_first, t_last, the ADR-0491 phase split
/// and db — no stats, no plan (a plain read carries neither), and no
/// bookmark: a read commits nothing, so the engine reports none
/// (ADR-0502 §7).
#[test]
fn the_ending_success_of_a_read_carries_type_t_first_t_last_and_db() {
    let mut client = connected_client();
    for message in [Message::Pull, Message::Discard] {
        let (_, ending) = run_to_end(&mut client, "UNWIND [1, 2] AS x RETURN x", message);
        assert_eq!(
            metadata_keys(&ending),
            [
                "type",
                "t_first",
                "t_last",
                "parsing_time",
                "planning_time",
                "plan_execution_time",
                "lex_parse_time",
                "rewrite_time",
                "lower_time",
                "result_generation_time",
                "plan_cache",
                "db"
            ]
        );
        assert_eq!(ending.get("type"), Some(&Pack::String("r".into())));
        assert_eq!(ending.get("db"), Some(&Pack::String("neo4j".into())));
        assert!(matches!(ending.get("t_first"), Some(Pack::Int(ms)) if *ms >= 0));
        assert!(matches!(ending.get("t_last"), Some(Pack::Int(ms)) if *ms >= 0));
    }
}

/// ADR-0491: the SUCCESS ending a result carries the statement's
/// PARSE/PLAN/EXECUTE split beside `t_last` — `parsing_time`,
/// `planning_time` and `plan_execution_time` as PackStream Float seconds,
/// the ADR-0491 contract keys and unit — while RUN SUCCESS carries none
/// of them (its ADR-0309 contract is unchanged). The three are disjoint
/// sub-windows of the total `t_first` reports, so their sum never exceeds
/// it; the 1 ms slack is `millis`'s whole-millisecond truncation of that
/// total, never a real overshoot.
#[test]
fn the_ending_success_carries_the_phase_split_and_run_success_does_not() {
    let mut client = connected_client();
    for message in [Message::Pull, Message::Discard] {
        let (run, ending) = run_to_end(&mut client, "UNWIND range(1, 100) AS x RETURN x", message);
        for key in PHASE_KEYS.iter().chain(FINE_PHASE_KEYS.iter()) {
            assert!(run.get(key).is_none(), "RUN SUCCESS has no {key}: {run:?}");
            let seconds = phase_seconds(&ending, key);
            assert!(seconds >= 0.0 && seconds.is_finite(), "{key}: {seconds}");
        }
        let t_first_ms = ending
            .get("t_first")
            .and_then(Pack::as_int)
            .expect("t_first is an Int");
        let phase_sum: f64 = PHASE_KEYS.iter().map(|k| phase_seconds(&ending, k)).sum();
        assert!(
            phase_sum <= t_first_ms as f64 / 1000.0 + 0.001,
            "the phase split {phase_sum}s must stay inside the total {t_first_ms}ms: {ending:?}"
        );
    }
}

/// ADR-0491 at the builder: the ending summary's phase keys are the
/// engine result's own split, exactly — each the corresponding
/// `QueryResult` accessor rescaled to the ADR-0491 contract's Float
/// seconds, placed after `t_last` and before `db`, with `t_first` and
/// `t_last` still ADR-0309's integer milliseconds.
#[test]
fn the_ending_summary_renders_the_engine_phase_split_in_seconds() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let result = conn.query("UNWIND range(1, 100) AS x RETURN x").unwrap();
    let codec = ValueCodec::new(Dialect::plain_44(), result.header.namespace);
    let metadata = crate::summary::ending(
        &result,
        Duration::from_millis(3),
        codec,
        &NotificationConfig::default(),
    )
    .unwrap();
    let mut expected_keys = vec!["type", "t_first", "t_last"];
    expected_keys.extend(PHASE_KEYS);
    // LABS-1120: the fine sub-split and the plan-cache outcome, between
    // the coarse split and `db`.
    expected_keys.extend(FINE_PHASE_KEYS);
    expected_keys.push("plan_cache");
    expected_keys.push("db");
    assert_eq!(keys(&metadata), expected_keys);
    let ending = Pack::Map(metadata);
    assert_eq!(ending.get("t_last"), Some(&Pack::Int(3)));
    assert_eq!(
        ending.get("t_first"),
        Some(&Pack::Int(result.header.elapsed.as_millis() as i64))
    );
    assert_eq!(
        ending.get("parsing_time"),
        Some(&Pack::Float(result.parse_ms() / 1000.0))
    );
    assert_eq!(
        ending.get("planning_time"),
        Some(&Pack::Float(result.plan_ms() / 1000.0))
    );
    assert_eq!(
        ending.get("plan_execution_time"),
        Some(&Pack::Float(result.execute_ms() / 1000.0))
    );
}

/// ADR-0309: `t_last` is the whole milliseconds the result's PULL and
/// DISCARD messages spent streaming or discarding, measured after that work
/// — so a stream that actually wrote records reports a real total, and a
/// paged stream's first page contributes to the same sum. 20,000 records
/// are far past the millisecond granularity even on a fast machine.
#[test]
fn t_last_sums_the_streaming_time_of_every_page() {
    let mut client = connected_client();
    send_run(&mut client, "UNWIND range(1, 20000) AS x RETURN x");
    success_map(read_msg(&mut client));
    // One page that leaves records (its share is stored on the result),
    // then the PULL that drains them and ends the result.
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(1))]);
    let (page, has_more) = read_records(&mut client);
    assert_eq!(page.len(), 1);
    assert_eq!(
        success_map(has_more),
        vec![(Pack::String("has_more".into()), Pack::Bool(true))]
    );
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    let (records, ending) = read_records(&mut client);
    assert_eq!(records.len(), 19999);
    let ending = Pack::Map(success_map(ending));
    assert!(
        matches!(ending.get("t_last"), Some(Pack::Int(ms)) if *ms >= 1),
        "t_last must measure the streaming work, not the instant before it: {ending:?}"
    );
}

/// F-40, D-52 to D-54, D-56: the ending SUCCESS of an auto-commit write
/// carries Neo4j's non-zero kebab-case counters plus contains-updates.
#[test]
fn write_statements_report_neo4j_stats() {
    let mut client = connected_client();
    for (cypher, query_type, stats) in [
        (
            "CREATE (a:P {k: 1})-[:R]->(b:P) SET b.k = 2",
            "w",
            stats_map(&[
                ("nodes-created", 2),
                ("relationships-created", 1),
                ("properties-set", 2),
                ("labels-added", 2),
            ]),
        ),
        (
            "CREATE (:A {a: 1, b: 2})-[:REL {w: 1}]->(:A)",
            "w",
            stats_map(&[
                ("nodes-created", 2),
                ("relationships-created", 1),
                ("properties-set", 3),
                ("labels-added", 2),
            ]),
        ),
        (
            // ADR-0332: a write with no returned columns — a MATCH that
            // only feeds SET/REMOVE returns no columns (D-53).
            "MATCH (n:A) WHERE n.a = 1 SET n:X, n.c = 3 REMOVE n.b",
            "w",
            stats_map(&[("properties-set", 2), ("labels-added", 1)]),
        ),
        (
            // ADR-0332: REMOVE without RETURN is a write with no columns
            // (D-53).
            "MATCH (n:X) REMOVE n:X",
            "w",
            stats_map(&[("labels-removed", 1)]),
        ),
        (
            // ADR-0332: DETACH DELETE without RETURN is a write with no
            // columns (D-54).
            "MATCH (n:A) DETACH DELETE n",
            "w",
            stats_map(&[("nodes-deleted", 2), ("relationships-deleted", 1)]),
        ),
        (
            "CREATE INDEX i FOR (n:A) ON (n.k)",
            "s",
            stats_map(&[("indexes-added", 1)]),
        ),
        ("DROP INDEX i", "s", stats_map(&[("indexes-removed", 1)])),
    ] {
        let (_, ending) = run_to_end(&mut client, cypher, Message::Pull);
        assert_eq!(ending.get("stats"), Some(&stats), "{cypher}");
        assert_eq!(
            ending.get("type"),
            Some(&Pack::String(query_type.into())),
            "{cypher}"
        );
    }
}

/// F-20, F-21: inside an explicit transaction the ending PULL SUCCESS
/// carries stats but no bookmark; COMMIT SUCCESS carries the bookmark.
#[test]
fn a_transaction_write_reports_stats_and_commit_carries_the_bookmark() {
    let mut client = connected_client();
    send_begin(&mut client, vec![]);
    assert!(success_map(read_msg(&mut client)).is_empty());
    let (_, ending) = run_to_end(
        &mut client,
        "CREATE (n:T {k: 1}) RETURN n.k AS k",
        Message::Pull,
    );
    assert_eq!(
        ending.get("stats"),
        Some(&stats_map(&[
            ("nodes-created", 1),
            ("properties-set", 1),
            ("labels-added", 1)
        ]))
    );
    assert!(ending.get("bookmark").is_none(), "{ending:?}");
    send_commit(&mut client);
    let commit = success_map(read_msg(&mut client));
    assert_eq!(keys(&commit), ["bookmark"]);
}

/// BEGIN, RUN `cypher` to its end, COMMIT: the COMMIT SUCCESS map.
fn commit_write(client: &mut TcpStream, cypher: &str) -> Pack {
    send_begin(client, vec![]);
    assert!(success_map(read_msg(client)).is_empty());
    run_to_end(client, cypher, Message::Pull);
    send_commit(client);
    Pack::Map(success_map(read_msg(client)))
}

/// ADR-0310, ADR-0502 §7: Bolt's bookmarks are the engine's commit
/// sequence, not a listener-local count. Two sequential Bolt commits carry
/// consecutive engine bookmarks; a write through the engine API between
/// them takes the next engine value, and the following Bolt commit
/// continues past it — exactly what `Connection::commit` (and the FFI's
/// `relate_conn_commit`) report for the same store. A commit, read or
/// write that committed nothing carries no bookmark, ROLLBACK answers
/// `{}`, and bookmarks a client sends back are accepted.
#[test]
fn bookmarks_are_the_engine_commit_sequence() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let mut client = connected_client_with(Arc::clone(&db), ConnConfig::default());

    let first = bookmark_sequence(&commit_write(&mut client, "CREATE (:B)"), "first commit");
    let second = bookmark_sequence(&commit_write(&mut client, "CREATE (:B)"), "second commit");
    assert_eq!(second, first + 1, "sequential commits are consecutive");

    // Another writer — the engine API directly — advances the same
    // sequence, and Bolt's next commit continues past it.
    let mut engine = db.connect().unwrap();
    engine.begin().unwrap();
    engine.query("CREATE (:B)").unwrap();
    let engine_commit = engine.commit().unwrap();
    assert_eq!(
        engine_commit.as_deref(),
        Some(format!("relate:{}", second + 1).as_str())
    );
    let third = bookmark_sequence(&commit_write(&mut client, "CREATE (:B)"), "third commit");
    assert_eq!(
        third,
        second + 2,
        "the engine API's commit took {}",
        second + 1
    );

    // An auto-commit write carries the engine's bookmark too.
    let (_, write) = run_to_end(&mut client, "CREATE (:B)", Message::Discard);
    assert_eq!(bookmark_sequence(&write, "auto-commit write"), third + 1);

    // Nothing committed, nothing fabricated: an empty transaction's
    // COMMIT is `{}`, and a read or an empty write ends without one.
    send_begin(
        &mut client,
        vec![(
            Pack::String("bookmarks".into()),
            Pack::List(vec![Pack::String(format!("relate:{third}"))]),
        )],
    );
    assert!(success_map(read_msg(&mut client)).is_empty());
    send_commit(&mut client);
    assert!(success_map(read_msg(&mut client)).is_empty());
    for cypher in ["MATCH (n) RETURN n", "MATCH (n:Absent) DELETE n"] {
        let (_, ending) = run_to_end(&mut client, cypher, Message::Pull);
        assert!(ending.get("bookmark").is_none(), "{cypher}: {ending:?}");
    }

    send_begin(&mut client, vec![]);
    success_map(read_msg(&mut client));
    send_rollback(&mut client);
    assert!(success_map(read_msg(&mut client)).is_empty());
}

/// The `stats` builder: Neo4j's update counters in canonical order,
/// kebab-case, zeros dropped, `None` for a read. The graph-diff counters
/// (`labels_added`, `properties_set`, …) are the TCK's and never reach it.
#[test]
fn the_stats_map_is_non_zero_kebab_case_counters() {
    assert_eq!(crate::summary::stats(&relate::QueryStats::default()), None);
    let stats = relate::QueryStats {
        nodes_created: 1,
        properties_set: 7,
        labels_added: 7,
        properties_written: 3,
        constraints_added: 1,
        ..Default::default()
    };
    assert_eq!(
        crate::summary::stats(&stats),
        Some(stats_map(&[
            ("nodes-created", 1),
            ("properties-set", 3),
            ("constraints-added", 1)
        ]))
    );
}

/// ADR-0309 (reversing ADR-0162 item 3): the query type rides on the
/// SUCCESS that ends the result — here a DISCARD — never on RUN SUCCESS.
#[test]
fn the_ending_success_carries_the_query_type() {
    let mut client = connected_client();
    for (cypher, expected) in [
        ("MATCH (n) RETURN n", "r"),
        ("CREATE (n:N)", "w"),
        ("MATCH (n) SET n.x = 1 RETURN n", "rw"),
        ("CREATE INDEX i FOR (n:N) ON (n.p)", "s"),
        // EXPLAIN/PROFILE report the wrapped statement's type (ADR-0162).
        ("EXPLAIN MATCH (n) RETURN n", "r"),
        ("EXPLAIN CREATE (n)", "w"),
        ("PROFILE MATCH (n) RETURN n", "r"),
        ("PROFILE MATCH (n) SET n.x = 1 RETURN n", "rw"),
    ] {
        send_run_at(&mut client, BoltVersion::V4_4, cypher, None);
        let run = Pack::Map(success_map(read_msg(&mut client)));
        assert!(
            run.get("type").is_none(),
            "RUN SUCCESS has no type: {run:?}"
        );
        send_pull(&mut client, Message::Discard, vec![]);
        let ending = Pack::Map(success_map(read_msg(&mut client)));
        assert_eq!(field(&ending, "type"), expected, "{cypher}");
    }
}
