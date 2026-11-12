//! The LABS-1120 timing block's per-mode rendering: one keyed `timings`
//! object (the model's one builder) with the `planCache` outcome beside
//! it, in every serializing machine mode — and nothing in the strict or
//! human modes. The fixture stamp is hand-built with the same partition
//! the engine's invariants guarantee (fine sub-windows inside their
//! coarse phases), so the rendered numbers are deterministic.

use super::*;

/// The fixture timing stamp: binary fractions partitioning exactly —
/// `lex_parse + rewrite + lower = parse`, `result_generation < execute`,
/// `parse + plan + execute < elapsed` — and a plan-cache hit, so the
/// outcome spelling rides the assertions too.
fn fixture_stamp() -> wire::TimingStampMs {
    wire::TimingStampMs {
        elapsed_ms: 0.5,
        parse_ms: 0.25,
        plan_ms: 0.1875,
        execute_ms: 0.0625,
        lex_parse_ms: 0.125,
        rewrite_ms: 0.0625,
        lower_ms: 0.0625,
        result_generation_ms: 0.03125,
        plan_cache: "hit-raw-fastpath",
    }
}

/// Render one `a = 1` fixture under `mode` carrying the fixture stamp —
/// through the suite's common scaffold, so the `columnTypes` the fixtures
/// see is derived by the canonical first-row mapping like every other
/// scaffold's (the stamp is the only fact this suite adds).
fn render_timings(mode: Mode) -> String {
    render_with_meta(
        Formatter::new(mode),
        &["a".to_string()],
        &[vec![CypherValue::Int(1)]],
        None,
        None,
        Some(fixture_stamp()),
    )
}

/// The `envelope` mode carries the block: one `timings` object with every
/// duration key (the exact key set, by name — a renamed or missing key
/// fails here) and the `planCache` outcome beside it.
#[test]
fn envelope_carries_the_timings_object_and_plan_cache_outcome() {
    let rendered = render_timings(Mode::Envelope);
    let envelope: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    let timings = &envelope["timings"];
    for key in [
        "elapsed_ms",
        "parse_ms",
        "plan_ms",
        "execute_ms",
        "lex_parse_ms",
        "rewrite_ms",
        "lower_ms",
        "result_generation_ms",
    ] {
        assert!(
            timings.get(key).is_some_and(serde_json::Value::is_number),
            "the envelope's timings object carries `{key}`: {rendered}"
        );
    }
    assert_eq!(timings["parse_ms"], serde_json::json!(0.25));
    assert_eq!(envelope["planCache"], serde_json::json!("hit-raw-fastpath"));
}

/// The `json` mode rides the same block as a rider: the rows wrap into
/// `{"data": [...], "timings": {...}, "planCache": ...}` — a plain run
/// with a result behind it no longer renders the bare array.
#[test]
fn json_wraps_the_rows_with_the_timings_rider() {
    let rendered = render_timings(Mode::Json);
    let output: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert!(
        output["data"].is_array(),
        "the rows ride under `data` beside the timing rider: {rendered}"
    );
    assert_eq!(output["timings"]["lex_parse_ms"], serde_json::json!(0.125));
    assert_eq!(output["planCache"], serde_json::json!("hit-raw-fastpath"));
}

/// The `jsonl` mode closes the statement's stream with one keyed terminal
/// line — the timing block, never confusable with a data row.
#[test]
fn jsonl_closes_with_the_timings_terminal_line() {
    let rendered = render_timings(Mode::Jsonl);
    let lines: Vec<&str> = rendered.lines().collect();
    let last = lines
        .last()
        .expect("the timings line terminates the stream");
    let terminal: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(
        terminal["timings"]["result_generation_ms"],
        serde_json::json!(0.03125)
    );
    assert_eq!(terminal["planCache"], serde_json::json!("hit-raw-fastpath"));
    // The row line before it is a data row, not the timing block.
    let row: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(row["a"], serde_json::json!(1));
}

/// ADR-0581: `Formatter::render_timings`, off, suppresses the jsonl
/// terminal line while the stamp still rides every other mode's rider —
/// the setting the CLI's `--timings` flag writes once per run. The
/// library default stays on (render-when-passed), which the test above
/// pins.
#[test]
fn jsonl_timings_off_suppresses_the_terminal_line() {
    let mut formatter = Formatter::new(Mode::Jsonl);
    formatter.render_timings = false;
    let rendered = render_with_meta(
        formatter,
        &["a".to_string()],
        &[vec![CypherValue::Int(1)]],
        None,
        None,
        Some(fixture_stamp()),
    );
    // Pure rows: the row line is the whole stream.
    assert_eq!(rendered, "{\"a\":1}\n", "no terminal line: {rendered}");
}

/// The `toon` mode renders terminal `timings:` / `planCache:` entries —
/// the nested object inline, the outcome as a bare TOON token.
#[test]
fn toon_renders_terminal_timings_entries() {
    let rendered = render_timings(Mode::Toon);
    assert!(
        rendered.lines().any(|line| line == "timings:"),
        "the timings entry header: {rendered}"
    );
    // The inline object's key set, parsed (the map's own order is its
    // serializer's, not a contract — the keys and values are).
    let inline = rendered
        .lines()
        .find(|line| line.starts_with("  {\"elapsed_ms\""))
        .expect("the timings object inline");
    let parsed: serde_json::Value = serde_json::from_str(inline.trim()).unwrap();
    assert_eq!(
        parsed,
        serde_json::json!({
            "elapsed_ms": 0.5, "parse_ms": 0.25, "plan_ms": 0.1875,
            "execute_ms": 0.0625, "lex_parse_ms": 0.125, "rewrite_ms": 0.0625,
            "lower_ms": 0.0625, "result_generation_ms": 0.03125
        }),
        "every duration key inline: {rendered}"
    );
    assert!(
        rendered.lines().any(|line| line == "  hit-raw-fastpath"),
        "the plan-cache outcome as a bare TOON token: {rendered}"
    );
}

/// The strict and human modes render nothing of the block: `csv` keeps its
/// row contract, `table` keeps the human `Time:` line the CLI owns. A
/// `timings: None` caller renders no keys in ANY mode — the presence
/// policy of `plan`, so the formatter's own fixtures change nothing.
#[test]
fn strict_and_human_modes_and_absent_timings_render_nothing() {
    for mode in [Mode::Csv, Mode::Table, Mode::Markdown, Mode::Cypher] {
        let rendered = render_timings(mode.clone());
        assert!(
            !rendered.contains("timings") && !rendered.contains("planCache"),
            "{mode:?} renders no timing block: {rendered}"
        );
    }
    let rendered = render_with(
        Formatter::new(Mode::Envelope),
        &["a".to_string()],
        &[vec![CypherValue::Int(1)]],
    );
    assert!(
        !rendered.contains("timings") && !rendered.contains("planCache"),
        "a None stamp omits the keys in every mode: {rendered}"
    );
}
