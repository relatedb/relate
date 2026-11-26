//! End-to-end tests for the CLI's speedscope output mode: a PROFILE
//! statement exported as one speedscope evented-profile JSON document
//! (loadable at https://www.speedscope.app), and the two non-PROFILE
//! error shapes.

use crate::support;

use support::{run_cli, CliRunOptions as Opts};

/// Parse a speedscope-mode stdout document, panicking with the output on
/// failure.
fn parse(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}"))
}

#[test]
fn speedscope_mode_exports_a_profile_as_evented_json() {
    // The full pipeline (flags -> REPL -> engine -> formatter): a PROFILE
    // statement over a small graph exports one evented profile — the
    // upstream file-format shape (schema URL, shared frames, one evented
    // profile, nanosecond units, balanced O/C events in non-decreasing
    // order).
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--mode",
        "speedscope",
        "PROFILE CREATE (n:Person {name: 'Ada'}) RETURN n.name AS who",
    ]));
    assert!(status.success(), "exited non-zero: {stderr}");
    assert_eq!(stdout.lines().count(), 1, "one compact line: {stdout}");
    let doc = parse(&stdout);
    assert_eq!(
        doc["$schema"],
        "https://www.speedscope.app/file-format-schema.json"
    );
    let profiles = doc["profiles"]
        .as_array()
        .unwrap_or_else(|| panic!("profiles is an array: {stdout}"));
    assert_eq!(profiles.len(), 1);
    let profile = &profiles[0];
    assert_eq!(profile["type"], "evented");
    assert_eq!(profile["unit"], "nanoseconds");
    let start = profile["startValue"].as_i64().expect("startValue");
    let end = profile["endValue"].as_i64().expect("endValue");
    assert!(start < end, "the profile spans time: {stdout}");
    let events = profile["events"]
        .as_array()
        .unwrap_or_else(|| panic!("events is an array: {stdout}"));
    assert!(!events.is_empty(), "the profile has events: {stdout}");
    // One frame per operator: the frames and the events' frame indices
    // agree, and every frame name is the operator plus its id.
    let frames = doc["shared"]["frames"]
        .as_array()
        .unwrap_or_else(|| panic!("frames is an array: {stdout}"));
    assert_eq!(frames.len(), events.len() / 2, "one O and one C per frame");
    for frame in frames {
        let name = frame["name"].as_str().unwrap_or_else(|| panic!("{stdout}"));
        let (op, id) = name.rsplit_once(' ').unwrap_or_else(|| panic!("{stdout}"));
        assert!(!op.is_empty(), "operator name: {stdout}");
        assert!(id.parse::<i64>().is_ok(), "operator id: {stdout}");
    }
    // The event pairing: non-decreasing `at`, every O matched by its C,
    // well-nested (the import-time contract of the evented format).
    let mut at = i64::MIN;
    let mut open: Vec<(usize, i64)> = Vec::new();
    for event in events {
        let t = event["at"].as_i64().unwrap_or_else(|| panic!("{stdout}"));
        let frame = event["frame"]
            .as_u64()
            .unwrap_or_else(|| panic!("{stdout}")) as usize;
        assert!(t >= at, "events are non-decreasing: {stdout}");
        at = t;
        match event["type"].as_str().unwrap_or_else(|| panic!("{stdout}")) {
            "O" => open.push((frame, t)),
            "C" => {
                let (open_frame, open_at) = open
                    .pop()
                    .unwrap_or_else(|| panic!("unbalanced events: {stdout}"));
                assert_eq!(open_frame, frame, "C matches the innermost O: {stdout}");
                assert!(t >= open_at, "a closed interval: {stdout}");
            }
            other => panic!("event type is O or C, got {other}: {stdout}"),
        }
    }
    assert!(open.is_empty(), "every O is closed: {stdout}");
}

#[test]
fn speedscope_mode_without_profile_times_exits_non_zero() {
    // Both non-PROFILE shapes are the same honest error: a plain query
    // (no plan tree) and an EXPLAIN tree (no per-operator metrics) —
    // there are no times to export, and an empty profile would
    // misrepresent the run.
    for query in ["RETURN 1", "EXPLAIN RETURN 1"] {
        let (status, stdout, stderr) = run_cli(Opts::new(&["--mode", "speedscope", query]));
        assert!(!status.success(), "{query} exited zero: {stdout}");
        assert!(stdout.is_empty(), "{query} stdout: {stdout}");
        assert!(
            stderr.contains("speedscope mode requires a PROFILE statement"),
            "{query} stderr: {stderr}"
        );
    }
}
