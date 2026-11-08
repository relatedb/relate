//! The speedscope mode pins: one golden document over a fixed PROFILE
//! plan fixture (deterministic ids and times), plus the two error shapes
//! (no plan tree, no metrics).

use super::*;

/// The canonical producer's own output (ADR-0344/0345), never a
/// hand-copy of the wire form: a hand-built PROFILE tree with
/// deterministic ids (`assign_ids`) and times, so the golden document is
/// byte-stable. The children's inclusive times (4 + 7 microseconds) sum
/// past the parent's (10 microseconds), pinning the scale-to-fit layout.
fn profile_fixture() -> PlanFixture {
    let scan = relate::PlanNode::new(
        "AllNodesScan",
        "n".to_string(),
        vec!["n".to_string()],
        Vec::new(),
    )
    .with_metrics(Some(relate::ClauseMetrics {
        rows: 3,
        db_hits: 3,
        memory_bytes: 60,
        expression_built_bytes: 0,
        time_ns: 4_000,
        reads: Default::default(),
        phases: Default::default(),
        start: None,
        kernel: None,
        batched: false,
        zone_skips: 0,
        frontier: Default::default(),
        frontier_decline: None,
    }));
    let filter = relate::PlanNode::new("Filter", "n.age > 21".to_string(), Vec::new(), Vec::new())
        .with_metrics(Some(relate::ClauseMetrics {
            rows: 1,
            db_hits: 0,
            memory_bytes: 20,
            expression_built_bytes: 0,
            // 7 us, so the children sum to 11 us inside the parent's 10 us.
            time_ns: 7_000,
            reads: Default::default(),
            phases: Default::default(),
            start: None,
            kernel: None,
            batched: false,
            zone_skips: 0,
            frontier: Default::default(),
            frontier_decline: None,
        }));
    let mut root = relate::PlanNode::new(
        "ProduceResults",
        "who".to_string(),
        Vec::new(),
        vec![scan, filter],
    )
    .with_metrics(Some(relate::ClauseMetrics {
        rows: 1,
        db_hits: 0,
        memory_bytes: 20,
        expression_built_bytes: 0,
        time_ns: 10_000,
        reads: Default::default(),
        phases: Default::default(),
        start: None,
        kernel: None,
        batched: false,
        zone_skips: 0,
        frontier: Default::default(),
        frontier_decline: None,
    }));
    root.assign_ids();
    PlanFixture::of(&root)
}

/// Parse a speedscope-mode render, panicking with the output on failure.
fn parse_speedscope(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
}

#[test]
fn speedscope_mode_exports_the_evented_profile_shape() {
    // The upstream file-format contract: the schema URL, one frame per
    // operator (operator name plus id), one evented profile with
    // nanosecond units, and a balanced, non-decreasing O/C event list.
    let header = vec!["Query: RETURN 1".to_string(), "Plan:".to_string()];
    let s = render_plan(Mode::Speedscope, &[], &[], &profile_fixture(), &header);
    let doc = parse_speedscope(&s);
    assert_eq!(
        doc["$schema"],
        "https://www.speedscope.app/file-format-schema.json"
    );
    assert_eq!(
        doc["exporter"],
        concat!("relate@", env!("CARGO_PKG_VERSION"))
    );
    let frames = doc["shared"]["frames"]
        .as_array()
        .unwrap_or_else(|| panic!("frames is an array: {s}"));
    let names: Vec<&str> = frames
        .iter()
        .map(|f| {
            f["name"]
                .as_str()
                .unwrap_or_else(|| panic!("frame name: {s}"))
        })
        .collect();
    // One frame per operator, DFS (pre-order) — the operator plus its id.
    assert_eq!(
        names,
        vec!["ProduceResults 0", "AllNodesScan 1", "Filter 2"]
    );
    let profiles = doc["profiles"]
        .as_array()
        .unwrap_or_else(|| panic!("profiles is an array: {s}"));
    assert_eq!(profiles.len(), 1);
    let profile = &profiles[0];
    assert_eq!(profile["type"], "evented");
    assert_eq!(profile["unit"], "nanoseconds");
    assert_eq!(profile["startValue"], 0);
    assert_eq!(profile["endValue"], 10_000);
    let events = profile["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events is an array: {s}"));
    // The laid-out timeline: the children's 4 + 7 us sum past the
    // parent's 10 us, so the children scale to fit — the first child
    // gets 4 * 10_000 / 11_000 = 3_636 ns, the second the 6_364 ns
    // remainder. Parent intervals contain their children's; every open
    // has its close, well-nested.
    let expected: &[(&str, usize, i64)] = &[
        ("O", 0, 0),
        ("O", 1, 0),
        ("C", 1, 3_636),
        ("O", 2, 3_636),
        ("C", 2, 10_000),
        ("C", 0, 10_000),
    ];
    let got: Vec<(String, usize, i64)> = events
        .iter()
        .map(|e| {
            (
                e["type"]
                    .as_str()
                    .unwrap_or_else(|| panic!("event type: {s}"))
                    .to_string(),
                e["frame"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("event frame: {s}")) as usize,
                e["at"].as_i64().unwrap_or_else(|| panic!("event at: {s}")),
            )
        })
        .collect();
    let expected_tuples: Vec<(String, usize, i64)> = expected
        .iter()
        .map(|(t, f, at)| (t.to_string(), *f, *at))
        .collect();
    assert_eq!(got, expected_tuples, "events in {s}");
}

#[test]
fn speedscope_mode_keeps_unscaled_children_within_the_parent() {
    // Without the overflow, each child keeps its own inclusive time:
    // children tile the parent's interval exactly, sequentially.
    let scan = relate::PlanNode::new(
        "AllNodesScan",
        "n".to_string(),
        vec!["n".to_string()],
        Vec::new(),
    )
    .with_metrics(Some(relate::ClauseMetrics {
        rows: 3,
        db_hits: 3,
        memory_bytes: 60,
        expression_built_bytes: 0,
        time_ns: 2_000,
        reads: Default::default(),
        phases: Default::default(),
        start: None,
        kernel: None,
        batched: false,
        zone_skips: 0,
        frontier: Default::default(),
        frontier_decline: None,
    }));
    let mut root = relate::PlanNode::new("ProduceResults", "n".to_string(), Vec::new(), vec![scan])
        .with_metrics(Some(relate::ClauseMetrics {
            rows: 3,
            db_hits: 0,
            memory_bytes: 60,
            expression_built_bytes: 0,
            time_ns: 9_000,
            reads: Default::default(),
            phases: Default::default(),
            start: None,
            kernel: None,
            batched: false,
            zone_skips: 0,
            frontier: Default::default(),
            frontier_decline: None,
        }));
    root.assign_ids();
    let plan = PlanFixture::of(&root);
    // The header is unused by the speedscope mode (ADR-0359: an EXPLAIN
    // result carries no rows), so the fixture carries none either.
    let header = vec!["Query: RETURN 1".to_string(), "Plan:".to_string()];
    let s = render_plan(Mode::Speedscope, &[], &[], &plan, &header);
    let events = &parse_speedscope(&s)["profiles"][0]["events"];
    let at: Vec<i64> = events
        .as_array()
        .expect("events is an array")
        .iter()
        .map(|e| e["at"].as_i64().expect("at is an integer"))
        .collect();
    assert_eq!(at, vec![0, 0, 2_000, 9_000]);
}

/// Run a speedscope-mode format that must fail, returning the error's
/// message — the render scaffold unwraps `Ok`, so the error shapes need
/// their own call.
fn speedscope_error(plan: Option<&PlanFixture>) -> String {
    let f = Formatter::new(Mode::Speedscope);
    let rows: Vec<IndexMap<String, CypherValue>> = Vec::new();
    let mut out = Vec::new();
    match f.format(
        &mut out,
        &["plan".to_string()],
        &rows,
        StatementMeta {
            column_types: &[],
            row_cap: None,
            plan: plan.map(|fixture| notified_view(&fixture.value, &[], &[], fixture.profiled)),
            timings: None,
        },
    ) {
        Err(e) => e.to_string(),
        Ok(()) => panic!("speedscope mode must fail without PROFILE times"),
    }
}

#[test]
fn speedscope_mode_without_a_plan_tree_is_an_error() {
    // A plain query in speedscope mode: no plan tree, no per-operator
    // times — an error, not an empty profile that would misrepresent the
    // run.
    let msg = speedscope_error(None);
    assert!(
        msg.contains("speedscope mode requires a PROFILE statement"),
        "{msg}"
    );
    assert!(msg.contains("no plan tree"), "{msg}");
}

#[test]
fn speedscope_mode_without_profile_metrics_is_an_error() {
    // An EXPLAIN tree carries no metrics — the same error, via the one
    // message policy.
    let node = relate::PlanNode::new(
        "ProduceResults",
        "1 item(s)".to_string(),
        Vec::new(),
        Vec::new(),
    );
    let plan = PlanFixture::of(&node);
    let msg = speedscope_error(Some(&plan));
    assert!(
        msg.contains("speedscope mode requires a PROFILE statement"),
        "{msg}"
    );
    assert!(msg.contains("no PROFILE metrics"), "{msg}");
}
