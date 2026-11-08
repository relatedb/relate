//! The RUN and PULL SUCCESS result-metadata tests: the `fields`/
//! `columnTypes` (ADR-0224) and `t_first` keys on RUN, and the
//! `plan`/`profile` summary (ADR-0344/0345, ADR-0353) on the final PULL
//! SUCCESS — Neo4j's placement (ADR-0357), where strict clients that read
//! only the pull summary find it. The query `type` is *not* here: ADR-0309
//! (reversing ADR-0162 item 3, matching Neo4j 4.4) rides it on the SUCCESS
//! that ends the result, asserted by
//! `protocol_tests::the_ending_success_carries_the_query_type`.

use std::net::TcpStream;

use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    connected_client_with, drain_all, field, read_msg, send_run, send_run_at, success_map,
    two_label_db, TWO_LABEL_CARTESIAN,
};
use crate::version::BoltVersion;

/// Send `cypher` as a RUN and return its SUCCESS map.
fn run_success(client: &mut TcpStream, cypher: &str) -> Pack {
    send_run(client, cypher);
    Pack::Map(success_map(read_msg(client)))
}

/// Drain the open result — PULL or DISCARD (`message`) with `n = -1` —
/// and return the ending SUCCESS map: the summary the plan/profile keys
/// ride (ADR-0357), the placement Neo4j uses. A thin wrapper on the one
/// drain spelling, [`crate::testutil::pull_ending`].
fn drain_success(client: &mut TcpStream, message: Message) -> Pack {
    drain_all(client, message).1
}

/// The per-case wire contract of a drained plan tree (ADR-0344/0345,
/// ADR-0357): the Neo4j dict shape — operatorType / args / identifiers /
/// children, the root operator named and numbered first — with the PROFILE
/// metric quartet riding args only on a profiled tree, the rendered
/// `string-representation`, and the honest planner/runtime identity args.
fn assert_plan_summary_contract(plan: &Pack, cypher: &str, root: &str, profiled: bool) {
    assert_eq!(
        plan.get("operatorType").and_then(Pack::as_str),
        Some(root),
        "{cypher}"
    );
    // The args map — bound once, the file's one loud spelling of a
    // required key off the plan map.
    let args = plan
        .get("args")
        .unwrap_or_else(|| panic!("{cypher} carries an args map: {plan:?}"));
    assert_eq!(
        args.get("Id").and_then(Pack::as_int),
        Some(0),
        "the root is numbered first ({cypher})"
    );
    // The PROFILE metric fields — one contract, one loop: all four
    // ride args together only on the profiled tree (metric fields,
    // ADR-0344).
    for field in ["Rows", "TimeNs", "DbHits", "MemoryBytes"] {
        assert_eq!(args.get(field).is_some(), profiled, "{cypher} args.{field}");
    }
    assert!(
        matches!(plan.get("identifiers"), Some(Pack::List(_))),
        "{cypher} carries an identifiers list: {plan:?}"
    );
    assert!(
        matches!(plan.get("children"), None | Some(Pack::List(_))),
        "{cypher} children, when present, is a list: {plan:?}"
    );
    // The Neo4j-facing root args (ADR-0357): the rendered plan text
    // (header included — tooling parses the Cypher/Planner/Runtime
    // lines from it) and the planner/runtime identity, named honestly.
    let representation = args
        .get("string-representation")
        .and_then(Pack::as_str)
        .unwrap_or_else(|| panic!("{cypher} root carries string-representation"));
    assert!(
        representation.starts_with("Cypher 5\n\nPlanner relate\n\nRuntime relate\n"),
        "the representation carries Neo4j's header block: {representation:?}"
    );
    assert!(representation.contains(root), "{representation:?}");
    for (arg, expected) in [
        ("version", "5"),
        ("planner", "relate"),
        ("planner-impl", "relate"),
        ("runtime", "relate"),
        ("runtime-impl", "relate"),
    ] {
        assert_eq!(
            args.get(arg).and_then(Pack::as_str),
            Some(expected),
            "{cypher} args.{arg}"
        );
    }
    for arg in ["planner-version", "runtime-version"] {
        assert!(
            args.get(arg).and_then(Pack::as_str).is_some(),
            "{cypher} args.{arg}"
        );
    }
    // The estimate mirrors Neo4j's float form on the wire.
    if let Some(estimate) = args.get("EstimatedRows") {
        assert!(
            matches!(estimate, Pack::Float(_)),
            "EstimatedRows mirrors Neo4j's float: {estimate:?}"
        );
    }
}

/// The final PULL SUCCESS summary carries the structured plan of an
/// EXPLAIN/PROFILE statement (ADR-0344/0345, ADR-0357) in the Neo4j dict
/// shape — `plan` for EXPLAIN, `profile` for PROFILE — the same wire form
/// (`PlanNode::to_neo4j_value`) every surface reads, so the field shape
/// cannot drift between them. The RUN SUCCESS carries no plan key:
/// Neo4j's placement puts it on the drained result, where strict clients
/// (cypher-shell) read it.
#[test]
fn pull_success_carries_the_structured_plan() {
    let mut client = crate::testutil::connected_client();

    // (statement, summary key, root operator, first child operator when
    // one exists). Whether the root carries the PROFILE metrics pair is
    // the summary key itself: the drained result's PROFILE placement —
    // `profile` — is the only way a profiled plan rides it.
    for (cypher, key, root, child) in [
        (
            "EXPLAIN MATCH (n) RETURN n.name",
            "plan",
            "ProduceResults",
            Some("AllNodesScan"),
        ),
        (
            "PROFILE CREATE (n:N {name: 'A'})",
            "profile",
            "Create",
            None,
        ),
    ] {
        let profiled = key == "profile";
        // The RUN SUCCESS carries the row metadata, never the plan.
        let run = run_success(&mut client, cypher);
        assert!(
            run.get("plan").is_none() && run.get("profile").is_none(),
            "the plan rides the drained result, not RUN ({cypher}): {run:?}"
        );
        assert!(
            run.get("t_first").is_some(),
            "RUN SUCCESS carries t_first ({cypher})"
        );
        let pull = drain_success(&mut client, Message::Pull);
        let plan = pull
            .get(key)
            .unwrap_or_else(|| panic!("{cypher} must carry {key} on the drained result"));
        assert_plan_summary_contract(plan, cypher, root, profiled);
        if let Some(operator) = child {
            let Some(Pack::List(children)) = plan.get("children") else {
                panic!("{cypher} must carry a children list: {plan:?}")
            };
            assert_eq!(
                children[0].get("operatorType").and_then(Pack::as_str),
                Some(operator),
                "{cypher}"
            );
        }
    }
}

/// LABS-1117: the plan value is the engine's own tree, so the PROFILE
/// metadata carries the per-clause traversal rung (`KernelRung`) and the
/// static pattern-engine mode (`PatternEngine`) on the MATCH operator —
/// no Bolt-specific encoding, the shared plan presentation's args.
#[test]
fn pull_success_carries_the_traversal_rung() {
    let mut client = crate::testutil::connected_client();
    run_success(
        &mut client,
        relate_test_support::plan_rung::THREE_HOP_CORRIDOR,
    );
    drain_success(&mut client, Message::Pull);
    run_success(
        &mut client,
        &format!(
            "PROFILE {}",
            relate_test_support::plan_rung::THREE_HOP_MATCH
        ),
    );
    let pull = drain_success(&mut client, Message::Pull);
    let plan = pull
        .get("profile")
        .unwrap_or_else(|| panic!("the drained PROFILE carries the plan: {pull:?}"));

    /// The first node (pre-order) whose args carry `key`, over the plan
    /// value's plain nested maps and children list.
    fn arg<'a>(node: &'a Pack, key: &str) -> Option<&'a Pack> {
        if let Some(value) = node.get("args").and_then(|args| args.get(key)) {
            return Some(value);
        }
        match node.get("children") {
            Some(Pack::List(children)) => children.iter().find_map(|child| arg(child, key)),
            _ => None,
        }
    }

    assert_eq!(
        arg(plan, "KernelRung").and_then(Pack::as_str),
        Some("chain"),
        "the PROFILE summary names the chain rung: {plan:?}"
    );
    assert_eq!(
        arg(plan, "KernelHops").and_then(Pack::as_int),
        Some(3),
        "the chain rung's hop count rides beside it: {plan:?}"
    );
    assert_eq!(
        arg(plan, "PatternEngine").and_then(Pack::as_str),
        Some("auto"),
        "the static pattern-engine mode rides the MATCH operator: {plan:?}"
    );
}

/// A DISCARD ending carries the same pair (ADR-0357): the plan rides the
/// drained result, never the RUN, whichever message drains it.
#[test]
fn discard_success_carries_the_plan_summary() {
    let mut client = crate::testutil::connected_client();

    let run = run_success(&mut client, "EXPLAIN MATCH (n) RETURN n");
    assert!(
        run.get("plan").is_none(),
        "the plan rides the drained result, not RUN: {run:?}"
    );
    let discarded = drain_success(&mut client, Message::Discard);
    assert!(
        discarded.get("plan").is_some(),
        "a DISCARD SUCCESS carries the plan pair too: {discarded:?}"
    );
}

/// ADR-0486: an index-backed start narrates the seek on the Bolt wire
/// too — `NodeIndexSeekByRange` as the drained plan's child operator with
/// the seek line in `Details` and the posting window in `EstimatedRows`
/// (Neo4j's float form), the same engine wire form every surface reads.
#[test]
fn an_index_backed_start_narrates_the_seek_on_the_bolt_wire() {
    let (db, mut client) = crate::testutil::connected_client_with_db();
    {
        let mut conn = db.connect().unwrap();
        conn.query("CREATE RANGE INDEX user_age FOR (u:User) ON (u.age)")
            .unwrap();
        conn.query("UNWIND range(1, 10) AS i CREATE (:User {age: i})")
            .unwrap();
    }
    let cypher = "EXPLAIN MATCH (n:User) WHERE n.age > 5 RETURN n";
    let run = run_success(&mut client, cypher);
    assert!(
        run.get("plan").is_none(),
        "the plan rides the drained result, not RUN: {run:?}"
    );
    let pull = drain_success(&mut client, Message::Pull);
    let plan = pull
        .get("plan")
        .unwrap_or_else(|| panic!("{cypher} must carry the plan on the drained result"));
    assert_plan_summary_contract(plan, cypher, "ProduceResults", false);
    let Some(Pack::List(children)) = plan.get("children") else {
        panic!("{cypher} must carry a children list: {plan:?}")
    };
    let seek = &children[0];
    assert_eq!(
        seek.get("operatorType").and_then(Pack::as_str),
        Some("NodeIndexSeekByRange"),
        "{cypher}"
    );
    let args = seek
        .get("args")
        .unwrap_or_else(|| panic!("the seek node carries an args map: {seek:?}"));
    assert_eq!(
        args.get("Details").and_then(Pack::as_str),
        Some("RANGE INDEX n:User(age) WHERE age > 5, WHERE filter"),
        "{cypher}"
    );
    assert_eq!(
        args.get("EstimatedRows").and_then(Pack::as_int),
        Some(5),
        "the posting window rides the engine's integer form (the root's \
         Neo4j-facing float is the decoration's): {:?}",
        args.get("EstimatedRows")
    );
}

/// A plain run's drained result carries neither summary key.
#[test]
fn plain_runs_carry_no_plan_summary() {
    let mut client = crate::testutil::connected_client();

    run_success(&mut client, "RETURN 1 AS n");
    let plain = drain_success(&mut client, Message::Discard);
    assert!(plain.get("plan").is_none(), "plain runs carry no plan");
    assert!(
        plain.get("profile").is_none(),
        "plain runs carry no profile"
    );
}

/// A plan tree nested past packstream's depth limit (ADR-0353): the
/// drained result's summary carries the `planTruncated` marker instead of
/// silently omitting the key — distinguishable from a plain run (which
/// carries neither key nor marker), so a client cannot mistake a
/// depth-truncated PROFILE for a plain run.
#[test]
fn pull_success_marks_a_depth_truncated_plan() {
    let mut client = crate::testutil::connected_client();

    // 100 `WITH` clauses: each nests one operator deeper in the plan
    // tree (each level a children list + a child map, two packstream
    // containers), past the 128-container depth limit the pack
    // conversion enforces.
    let cypher = format!("PROFILE {} RETURN x", "WITH 1 AS x ".repeat(100));
    run_success(&mut client, &cypher);
    let pull = drain_success(&mut client, Message::Pull);
    assert!(
        pull.get("profile").is_none(),
        "the too-deep tree does not pack: {pull:?}"
    );
    assert!(
        matches!(pull.get("planTruncated"), Some(Pack::Bool(true))),
        "the depth-truncated plan carries its marker: {pull:?}"
    );
}

/// A connected client against the shared two-label store
/// ([`two_label_db`]: one `:Person` and one `:City`, the labels the
/// cartesian statement names — ADR-0363).
fn two_label_client() -> TcpStream {
    connected_client_with(two_label_db(), Default::default())
}

/// One Bolt notification entry (ADR-0363 decision 11): exactly the five
/// Neo4j keys, in the engine's key order, and no native or positional
/// extra — the shape the engine's Neo4j serializer emits, not a key set
/// this surface assembles.
fn assert_notification_entry(entry: &Pack, code: &str) {
    let Pack::Map(entries) = entry else {
        panic!("a notification entry is a map: {entry:?}");
    };
    assert_eq!(
        crate::testutil::keys(entries),
        ["code", "title", "description", "severity", "category"],
        "exactly the five Neo4j keys, in order: {entry:?}"
    );
    assert_eq!(field(entry, "code"), code);
    assert!(
        !entries
            .iter()
            .any(|(key, _)| key.as_str() == Some("remedy")),
        "the remedy is folded into description, never its own key: {entry:?}"
    );
    assert!(
        !entries
            .iter()
            .any(|(key, _)| key.as_str() == Some("position")),
        "no position key ever appears: {entry:?}"
    );
}

/// RUN SUCCESS never carries notifications (ADR-0363 decision 11): the
/// list rides only the SUCCESS that ends the result. Covers both a
/// notification-bearing EXPLAIN and a plain statement.
#[test]
fn run_success_never_carries_notifications() {
    let mut client = two_label_client();
    for cypher in [TWO_LABEL_CARTESIAN, "RETURN 1 AS n"] {
        let run = run_success(&mut client, cypher);
        assert!(
            run.get("notifications").is_none(),
            "RUN SUCCESS never carries notifications ({cypher}): {run:?}"
        );
        drain_success(&mut client, Message::Discard);
    }
}

/// The PULL SUCCESS that ends an EXPLAIN statement carries the query
/// notifications (ADR-0363 decision 11) in the engine's Neo4j-shaped
/// form: the cartesian warning's entry has exactly the five keys and the
/// canonical remedy appended to `description` as ` (remedy: …)`.
#[test]
fn pull_success_carries_the_neo4j_shaped_notifications() {
    let mut client = two_label_client();

    let run = run_success(&mut client, TWO_LABEL_CARTESIAN);
    assert!(
        run.get("notifications").is_none(),
        "the list rides the ended result, not RUN: {run:?}"
    );
    let pull = drain_success(&mut client, Message::Pull);
    let Some(Pack::List(notifications)) = pull.get("notifications") else {
        panic!("the ending summary carries the notifications list: {pull:?}");
    };
    assert_eq!(
        notifications.len(),
        1,
        "one cartesian warning, one notification: {notifications:?}"
    );
    let entry = &notifications[0];
    assert_notification_entry(entry, "Neo.ClientNotification.Statement.CartesianProduct");
    assert_eq!(field(entry, "title"), "Cartesian product");
    assert_eq!(field(entry, "severity"), "INFORMATION");
    assert_eq!(field(entry, "category"), "PERFORMANCE");
    assert_eq!(
        field(entry, "description"),
        "the MATCH patterns share no join anchor (remedy: Give the patterns a join anchor: \
         reuse a node variable across them, or add a pattern that relates their endpoints; \
         otherwise run them as separate queries.)"
    );
}

/// A result with no notifications omits the key entirely (ADR-0363
/// decision 11): neither RUN SUCCESS nor the ending SUCCESS carries an
/// empty list.
#[test]
fn statements_without_notifications_omit_the_key() {
    let mut client = crate::testutil::connected_client();

    let run = run_success(&mut client, "RETURN 1 AS n");
    assert!(
        run.get("notifications").is_none(),
        "RUN SUCCESS omits the key: {run:?}"
    );
    let ending = drain_success(&mut client, Message::Pull);
    assert!(
        ending.get("notifications").is_none(),
        "a notification-free result omits the key: {ending:?}"
    );
}

/// A DISCARD that ends the result carries the list too (ADR-0363 decision
/// 11): the placement is the ended result, whichever message ends it.
#[test]
fn discard_success_carries_the_notifications() {
    let mut client = two_label_client();

    run_success(&mut client, TWO_LABEL_CARTESIAN);
    let discarded = drain_success(&mut client, Message::Discard);
    let Some(Pack::List(notifications)) = discarded.get("notifications") else {
        panic!("a DISCARD ending carries the notifications list: {discarded:?}");
    };
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    assert_notification_entry(
        &notifications[0],
        "Neo.ClientNotification.Statement.CartesianProduct",
    );
}

/// The `columnTypes` strings of a RUN SUCCESS map (ADR-0224): the
/// engine-stamped per-column type names, one per `fields` entry.
fn column_types_strings(m: &Pack) -> Vec<String> {
    let Some(Pack::List(types)) = m.get("columnTypes") else {
        panic!("RUN SUCCESS must carry columnTypes: {m:?}");
    };
    types
        .iter()
        .map(|t| {
            t.as_str()
                .expect("a columnTypes entry is a string")
                .to_string()
        })
        .collect()
}

/// The RUN SUCCESS `columnTypes` (ADR-0224): the engine-stamped
/// per-column type names beside `fields`, in column order, under the
/// Relate extension key (not the standard Bolt `t_first` timing key)
/// — the same 18-name strings the wire envelope and the FFI carry, so the
/// Bolt surface cannot drift from the rest.
#[test]
fn run_success_carries_the_engine_column_types() {
    let (db, mut client) = crate::testutil::connected_client_with_db();

    for cypher in [
        "RETURN 1 AS n",
        "RETURN null AS n, 'x' AS s",
        "RETURN [1, 2] AS l, {a: 1} AS m",
        "CREATE (n:N {v: 1}) RETURN n, n.v AS v",
        "MATCH (n) RETURN count(n) AS c",
    ] {
        // The engine stamp the RUN SUCCESS must echo: run the same
        // statement through a plain engine connection.
        let mut conn = db.connect().unwrap();
        let expected: Vec<String> = conn.query(cypher).unwrap().header.column_types.clone();
        drop(conn);

        send_run_at(&mut client, BoltVersion::V4_4, cypher, None);
        let run = Pack::Map(success_map(read_msg(&mut client)));
        let column_types = column_types_strings(&run);
        assert_eq!(column_types, expected, "{cypher}");
        // One type per column, same order as `fields` — strict extraction:
        // a wrong-typed `fields` fails loudly instead of skipping the check.
        let Pack::List(fields) = run.get("fields").expect("RUN SUCCESS carries fields") else {
            panic!("RUN SUCCESS fields must be a list: {run:?}");
        };
        assert_eq!(column_types.len(), fields.len(), "{cypher}");
        // Drain the buffered result so the next RUN starts clean.
        crate::testutil::discard(&mut client);
    }
}
