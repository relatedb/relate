//! The speedscope mode pins: one golden document over a fixed PROFILE
//! plan fixture (deterministic ids and times), plus the two error shapes
//! (no plan tree, no metrics).

use super::*;

/// One profiled node of the test scaffold: the operator, its details and
/// identifiers, the rows / db hits / inclusive microseconds the export's
/// layout reads, and the children — every other metric field zeroed
/// through the one spelling, so a field addition patches one place.
fn profiled(
    operator: &str,
    details: &str,
    identifiers: Vec<String>,
    rows: u64,
    db_hits: u64,
    time_us: u128,
    children: Vec<relate::PlanNode>,
) -> relate::PlanNode {
    relate::PlanNode::new(operator, details.to_string(), identifiers, children).with_metrics(Some(
        relate::ClauseMetrics {
            rows,
            db_hits,
            memory_bytes: 60,
            expression_built_bytes: 0,
            time_ns: time_us * 1_000,
            reads: Default::default(),
            phases: Default::default(),
            start: None,
            kernel: None,
            batched: false,
            zone_skips: 0,
            frontier: Default::default(),
            frontier_decline: None,
        },
    ))
}

/// The canonical producer's own output (ADR-0344/0345), never a
/// hand-copy of the wire form: a hand-built PROFILE tree with
/// deterministic ids (`assign_ids`) and times, so the golden document is
/// byte-stable. The children's inclusive times (4 + 7 microseconds) sum
/// past the parent's (10 microseconds), pinning the scale-to-fit layout.
fn profile_fixture() -> PlanFixture {
    let scan = profiled(
        "AllNodesScan",
        "n",
        vec!["n".to_string()],
        3,
        3,
        4,
        Vec::new(),
    );
    // 7 us, so the children sum to 11 us inside the parent's 10 us.
    let filter = profiled("Filter", "n.age > 21", Vec::new(), 1, 0, 7, Vec::new());
    let mut root = profiled(
        "ProduceResults",
        "who",
        Vec::new(),
        1,
        0,
        10,
        vec![scan, filter],
    );
    root.assign_ids();
    PlanFixture::of(&root)
}

/// Parse a speedscope-mode render, panicking with the output on failure.
fn parse_speedscope(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
}

/// The document's frame names, in the export's order, panicking with the
/// output on a shape the export never writes.
fn frame_names<'a>(doc: &'a serde_json::Value, s: &str) -> Vec<&'a str> {
    doc["shared"]["frames"]
        .as_array()
        .unwrap_or_else(|| panic!("frames is an array: {s}"))
        .iter()
        .map(|frame| {
            frame["name"]
                .as_str()
                .unwrap_or_else(|| panic!("frame name: {s}"))
        })
        .collect()
}

/// The document's event `at` values, in order — the laid-out timeline.
fn event_ats(doc: &serde_json::Value, s: &str) -> Vec<i64> {
    doc["profiles"][0]["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events is an array: {s}"))
        .iter()
        .map(|event| {
            event["at"]
                .as_i64()
                .unwrap_or_else(|| panic!("event at: {s}"))
        })
        .collect()
}

/// The two-step plan the layout pins share: an `AllNodesScan` (3 rows,
/// 3 db hits, 2 us) under a `ProduceResults` (3 rows, 0 db hits, 9 us),
/// with `extra` appended after the scan — the one place the scaffold's
/// numbers live, so a test adds only what it exists for.
fn two_step_plan(extra: Vec<relate::PlanNode>) -> PlanFixture {
    let scan = profiled(
        "AllNodesScan",
        "n",
        vec!["n".to_string()],
        3,
        3,
        2,
        Vec::new(),
    );
    let mut children = vec![scan];
    children.extend(extra);
    let mut root = profiled("ProduceResults", "n", Vec::new(), 3, 0, 9, children);
    root.assign_ids();
    PlanFixture::of(&root)
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
    let names = frame_names(&doc, &s);
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
    let plan = two_step_plan(Vec::new());
    // The header is unused by the speedscope mode (ADR-0359: an EXPLAIN
    // result carries no rows), so the fixture carries none either.
    let header = vec!["Query: RETURN 1".to_string(), "Plan:".to_string()];
    let s = render_plan(Mode::Speedscope, &[], &[], &plan, &header);
    let doc = parse_speedscope(&s);
    let at = event_ats(&doc, &s);
    assert_eq!(at, vec![0, 0, 2_000, 9_000]);
}

#[test]
fn speedscope_mode_skips_fused_virtual_children_without_metrics() {
    // A fused virtual child is a display-only breakdown step with no
    // metrics of its own — the timeline exports the executed operators, so
    // the export succeeds and never lays the step out (or counts it)
    // against the parent's interval.
    let fused = relate::PlanNode::new(
        "NodeIndexSeek",
        "RANGE INDEX n:User(age) WHERE age > 5".to_string(),
        Vec::new(),
        Vec::new(),
    )
    .with_fused(0, None);
    let plan = two_step_plan(vec![fused]);
    let header = vec!["Query: RETURN 1".to_string(), "Plan:".to_string()];
    let s = render_plan(Mode::Speedscope, &[], &[], &plan, &header);
    let doc = parse_speedscope(&s);
    assert_eq!(
        frame_names(&doc, &s),
        vec!["ProduceResults 0", "AllNodesScan 1"]
    );
    let at = event_ats(&doc, &s);
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
