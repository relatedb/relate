//! Per-mode smoke tests: table, CSV, JSON, JSONL, Markdown, Cypher, null
//! and empty-column handling.

use super::*;

#[test]
fn test_table_format() {
    let cols = vec!["name".to_string(), "age".to_string()];
    let s = render(
        Mode::Table,
        &cols,
        &[
            vec![CypherValue::Str("Alice".into()), CypherValue::Int(30)],
            vec![CypherValue::Str("Bob".into()), CypherValue::Int(25)],
        ],
    );
    assert!(s.contains("Alice"));
    assert!(s.contains("Bob"));
    assert!(s.contains("+"));
    assert!(s.contains("|"));
}

#[test]
fn test_csv_format() {
    let cols = vec!["name".to_string(), "age".to_string()];
    let s = render(
        Mode::Csv,
        &cols,
        &[vec![CypherValue::Str("Alice".into()), CypherValue::Int(30)]],
    );
    let lines: Vec<&str> = s.trim().split('\n').collect();
    assert_eq!(lines[0], "name,age");
    assert_eq!(lines[1], "Alice,30");
}

#[test]
fn test_csv_no_headers() {
    let cols = vec!["name".to_string()];
    let mut f = Formatter::new(Mode::Csv);
    f.headers = false;
    let s = render_with(f, &cols, &[vec![CypherValue::Str("Alice".into())]]);
    assert_eq!(s.trim(), "Alice");
}

#[test]
fn test_csv_escaping() {
    let cols = vec!["value".to_string()];
    let s = render(
        Mode::Csv,
        &cols,
        &[
            vec![CypherValue::Str("hello, world".into())],
            vec![CypherValue::Str("say \"hi\"".into())],
        ],
    );
    assert!(s.contains("\"hello, world\""));
    assert!(s.contains("\"say \"\"hi\"\"\""));
}

#[test]
fn test_json_format() {
    let cols = vec!["name".to_string(), "age".to_string()];
    let s = render(
        Mode::Json,
        &cols,
        &[vec![CypherValue::Str("Alice".into()), CypherValue::Int(30)]],
    );
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(parsed.is_array());
    assert_eq!(parsed[0]["name"], "Alice");
    assert_eq!(parsed[0]["age"], 30);
}

#[test]
fn test_json_pretty_vs_compact() {
    // ADR-0240 (LABS-233): `Formatter::new` keeps the historical pretty
    // default; `pretty = false` renders one compact line.
    let cols = vec!["name".to_string(), "age".to_string()];
    let rows: Vec<Vec<CypherValue>> =
        vec![vec![CypherValue::Str("Alice".into()), CypherValue::Int(30)]];
    let pretty = render(Mode::Json, &cols, &rows);
    assert!(pretty.lines().count() > 1, "{pretty}");

    let mut f = Formatter::new(Mode::Json);
    f.pretty = false;
    let compact = render_with(f, &cols, &rows);
    // One line, and the same value as the pretty form (key order inside a
    // JSON object is the serializer's, not the contract).
    assert_eq!(compact.lines().count(), 1, "{compact}");
    let parsed: serde_json::Value = serde_json::from_str(&compact).unwrap();
    assert_eq!(parsed[0]["name"], "Alice");
    assert_eq!(parsed[0]["age"], 30);
}

#[test]
fn test_jsonl_format() {
    let cols = vec!["name".to_string()];
    let s = render(
        Mode::Jsonl,
        &cols,
        &[
            vec![CypherValue::Str("Alice".into())],
            vec![CypherValue::Str("Bob".into())],
        ],
    );
    let lines: Vec<&str> = s.trim().split('\n').collect();
    assert_eq!(lines.len(), 2);
}

#[test]
fn test_json_non_finite_floats_render_as_display_strings() {
    // ADR-0280: NaN/±Inf render as the JSON strings "NaN"/"Inf"/"-Inf" (the
    // `CypherValue` Display spellings) in both JSON and JSONL modes — never
    // `null`, which was indistinguishable from a genuine null (LABS-165).
    let cols = vec!["val".to_string()];
    let rows = vec![
        vec![CypherValue::Float(f64::NAN)],
        vec![CypherValue::Float(f64::INFINITY)],
        vec![CypherValue::Float(f64::NEG_INFINITY)],
        vec![CypherValue::Null],
    ];

    let parsed: serde_json::Value =
        serde_json::from_str(&render(Mode::Json, &cols, &rows)).unwrap();
    assert_eq!(parsed[0]["val"], serde_json::json!("NaN"));
    assert_eq!(parsed[1]["val"], serde_json::json!("Inf"));
    assert_eq!(parsed[2]["val"], serde_json::json!("-Inf"));
    // A genuine null stays JSON null — the encodings never collide.
    assert_eq!(parsed[3]["val"], serde_json::Value::Null);

    let s = render(Mode::Jsonl, &cols, &rows);
    let lines: Vec<&str> = s.trim().split('\n').collect();
    assert_eq!(lines.len(), 4);
    let vals: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["val"].clone())
        .collect();
    assert_eq!(
        vals,
        vec![
            serde_json::json!("NaN"),
            serde_json::json!("Inf"),
            serde_json::json!("-Inf"),
            serde_json::Value::Null,
        ]
    );
}

#[test]
fn test_markdown_format() {
    let cols = vec!["name".to_string(), "age".to_string()];
    let s = render(
        Mode::Markdown,
        &cols,
        &[vec![CypherValue::Str("Alice".into()), CypherValue::Int(30)]],
    );
    let lines: Vec<&str> = s.trim().split('\n').collect();
    assert!(lines[0].starts_with('|'));
    assert!(lines[1].contains("---"));
    assert!(lines[2].contains("Alice"));
}

#[test]
fn test_cypher_format() {
    let cols = vec!["name".to_string()];
    let s = render(
        Mode::Cypher,
        &cols,
        &[vec![CypherValue::Str("Alice".into())]],
    );
    assert!(s.contains("CREATE"));
    assert!(s.contains("'Alice'"));
}

#[test]
fn test_null_values() {
    let cols = vec!["val".to_string()];
    let s = render(Mode::Table, &cols, &[vec![CypherValue::Null]]);
    assert!(s.contains("<null>"));
}

#[test]
fn test_empty_columns() {
    let s = render(Mode::Table, &[], &[]);
    assert!(s.is_empty());
}

#[test]
fn envelope_has_columns_rows_and_truncated_on_one_line() {
    let cols = vec!["a".to_string(), "l".to_string()];
    let rows = vec![vec![
        CypherValue::Int(1),
        CypherValue::List(vec![CypherValue::Int(1), CypherValue::Int(2)]),
    ]];
    let out = render(Mode::Envelope, &cols, &rows);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert_eq!(
        out,
        concat!(
            "{\"columnTypes\":[\"INTEGER\",\"LIST\"],\"columns\":[\"a\",\"l\"],",
            "\"notifications\":[],\"rows\":[{\"a\":1,\"l\":[1,2]}],\"truncated\":false}\n"
        )
    );
}

#[test]
fn envelope_of_an_empty_result_still_lists_its_columns() {
    let cols = vec!["n".to_string(), "p".to_string()];
    let out = render(Mode::Envelope, &cols, &[]);
    assert_eq!(
        out,
        "{\"columnTypes\":[\"ANY\",\"ANY\"],\"columns\":[\"n\",\"p\"],\"notifications\":[],\
         \"rows\":[],\"truncated\":false}\n"
    );
}

#[test]
fn envelope_of_a_columnless_result_is_empty_arrays() {
    let out = render(Mode::Envelope, &[], &[]);
    assert_eq!(
        out,
        "{\"columnTypes\":[],\"columns\":[],\"notifications\":[],\"rows\":[],\"truncated\":false}\n"
    );
}

#[test]
fn mode_names_parse_and_json_modes_are_flagged() {
    for name in Mode::names_in_order() {
        assert!(Mode::parse(name).is_some(), "{name} does not parse");
    }
    assert!(Mode::parse("ENVELOPE") == Some(Mode::Envelope));
    for (mode, is_json) in [
        (Mode::Table, false),
        (Mode::Csv, false),
        (Mode::Markdown, false),
        (Mode::Cypher, false),
        (Mode::Toon, false),
        (Mode::Json, true),
        (Mode::Jsonl, true),
        (Mode::Envelope, true),
    ] {
        assert_eq!(mode.is_json(), is_json, "{mode:?}");
    }
}

#[test]
fn test_toon_format() {
    // ADR-0239: one `rows[N]{cols}:` header, then one indented
    // comma-separated line per row.
    let cols = vec!["name".to_string(), "age".to_string()];
    let s = render(
        Mode::Toon,
        &cols,
        &[
            vec![CypherValue::Str("Alice".into()), CypherValue::Int(30)],
            vec![CypherValue::Str("Bob".into()), CypherValue::Int(25)],
        ],
    );
    assert_eq!(s, "rows[2]{name,age}:\n  Alice,30\n  Bob,25\n");
}

#[test]
fn toon_quotes_only_strings_that_need_it() {
    let cols = vec!["v".to_string()];
    let rows: Vec<Vec<CypherValue>> = [
        "plain",        // bare
        "hello, world", // comma
        "say \"hi\"",   // quote
        " padded",      // leading whitespace
        "",             // empty
        "42",           // would re-read as a number
        "true",         // would re-read as a boolean
        "null",         // would re-read as null
        "line\nbreak",  // control character
    ]
    .iter()
    .map(|s| vec![CypherValue::Str((*s).into())])
    .collect();
    let s = render(Mode::Toon, &cols, &rows);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "rows[9]{v}:");
    assert_eq!(lines[1], "  plain");
    assert_eq!(lines[2], "  \"hello, world\"");
    assert_eq!(lines[3], "  \"say \\\"hi\\\"\"");
    assert_eq!(lines[4], "  \" padded\"");
    assert_eq!(lines[5], "  \"\"");
    assert_eq!(lines[6], "  \"42\"");
    assert_eq!(lines[7], "  \"true\"");
    assert_eq!(lines[8], "  \"null\"");
    assert_eq!(lines[9], "  \"line\\nbreak\"");
}

#[test]
fn toon_scalars_and_nested_values() {
    // Booleans, null, floats and the temporal scalars bare (the temporal
    // Display is its wire form, e.g. 2024-03-14 — ADR-0239's
    // scalars-bare contract, restored per the #379 rust review); non-finite
    // floats as their JSON strings (ADR-0280); lists/maps as inline
    // compact JSON.
    let cols = vec!["v".to_string()];
    let rows = vec![
        vec![CypherValue::Bool(true)],
        vec![CypherValue::Date(
            chrono::NaiveDate::from_ymd_opt(2024, 3, 14).expect("a valid date"),
        )],
        vec![CypherValue::Null],
        vec![CypherValue::Float(1.5)],
        vec![CypherValue::Float(f64::NAN)],
        vec![CypherValue::List(vec![
            CypherValue::Int(1),
            CypherValue::Int(2),
        ])],
    ];
    let s = render(Mode::Toon, &cols, &rows);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "rows[6]{v}:");
    assert_eq!(lines[1], "  true");
    assert_eq!(lines[2], "  2024-03-14");
    assert_eq!(lines[3], "  null");
    assert_eq!(lines[4], "  1.5");
    // "NaN" re-parses as a float, so it prints JSON-quoted like in `json`.
    assert_eq!(lines[5], "  \"NaN\"");
    assert_eq!(lines[6], "  [1,2]");
}

#[test]
fn toon_of_an_empty_result_still_lists_its_columns() {
    let cols = vec!["n".to_string(), "p".to_string()];
    assert_eq!(render(Mode::Toon, &cols, &[]), "rows[0]{n,p}:\n");
}

#[test]
fn table_mode_boxes_each_explain_plan_line() {
    // ADR-0359: an EXPLAIN result carries no rows; table mode prints the
    // `Query:` echo header and boxes each operator row — borders between
    // rows so the eye cannot slip into the next operator's line.
    let header = vec!["Query: MATCH (n) RETURN n".to_string(), "Plan:".to_string()];
    let s = render_plan(Mode::Table, &[], &[], &plan_fixture(), &header);
    assert_eq!(
        s,
        concat!(
            "Query: MATCH (n) RETURN n\n",
            "Plan:\n",
            "+------------------+----+-----------+\n",
            "| Operator         | Id | Details   |\n",
            "+------------------+----+-----------+\n",
            "| + ProduceResults | 0  | 1 item(s) |\n",
            "+------------------+----+-----------+\n"
        )
    );
}

#[test]
fn table_mode_escapes_a_control_character_in_a_details_cell() {
    // LABS-825: a string seek bound carries the statement's own text into
    // the `Details` line, raw; the table's one cell-escaping boundary
    // spells the ESC exactly once and no raw control byte reaches the
    // terminal.
    let node = relate::PlanNode::new(
        "NodeIndexSeek",
        "RANGE INDEX n:Person(name) WHERE name = 'a\u{1b}[31mb'".to_string(),
        Vec::new(),
        Vec::new(),
    );
    let s = render_plan(Mode::Table, &[], &[], &PlanFixture::of(&node), &[]);
    assert!(s.contains("name = 'a\\u{1B}[31mb'"), "{s}");
    assert!(!s.contains('\u{1b}'), "raw ESC leaked: {s:?}");
}

#[test]
fn table_mode_renders_profile_metric_columns() {
    // ADR-0344 phase 2 + metric fields: a PROFILE tree (a node carrying
    // metrics) renders the `Rows | Time (ms) | Db Hits | Memory (Bytes)`
    // columns beside the base columns — the walk's own fact, like the
    // estimate column.
    // The header fixture builds through the canonical producer (ADR-0359's
    // header policy), like the json suite's.
    let header = plan_header_lines("PROFILE MATCH (n) RETURN n");
    let plan = profile_fixture();
    let s = render_plan(Mode::Table, &[], &[], &plan, &header);
    assert!(s.contains("Rows") && s.contains("Time (ms)"), "{s}");
    assert!(
        s.contains("Db Hits")
            && s.contains("Memory (Bytes)")
            && s.contains("Expression Built (Bytes)"),
        "{s}"
    );
    // The operator row is pinned whole (the
    // `table_mode_boxes_each_explain_plan_line` pattern): each metric
    // needle is bound to its column, never a loose whole-output substring
    // that a neighboring cell could satisfy, and the numbers come from
    // the fixture's own metrics (the time through the wire's one
    // conversion, 26_000 ns = 0.026 ms).
    let metrics = plan.metrics.expect("the profile fixture carries metrics");
    // Both time cells derive from the fixture's own time_ns through the
    // table's one conversion (`metric_time_ms`) — the single-node tree's
    // own time and its subtree total are the same number (ADR-0396) —
    // and the cells come from the suite's one splitter (`boxed_cells`),
    // so the expected vec pins the real column cells with no edge
    // artifacts.
    let time_ns = i64::try_from(metrics.time_ns).expect("the fixture's time_ns fits i64");
    let time_ms = crate::plan_table::metric_time_ms(time_ns);
    assert_eq!(
        boxed_cells(
            s.lines()
                .find(|line| line.contains("+ ProduceResults"))
                .expect("the operator row")
        ),
        vec![
            "+ ProduceResults",
            "0",
            "1 item(s)",
            &metrics.rows.to_string(),
            &time_ms,
            &time_ms,
            &metrics.db_hits.to_string(),
            &metrics.memory_bytes.to_string(),
            &metrics.expression_built_bytes.to_string(),
        ],
    );
}

/// ADR-0357: `PlanView::wire` (the Neo4j-facing, decorated value) and
/// `PlanView::tree` (the engine's own wire form) are distinct roles — the
/// serializing modes emit `wire`, the typed table/speedscope walk reads
/// `tree`, whose `EstimatedRows` stays an integer. Every other fixture
/// feeds one value to both roles, so a swap cannot show there; this test
/// feeds the decorated value as `wire` and the engine form as `tree` — a
/// swapped walk would panic on the decorated float estimate.
#[test]
fn the_wire_and_tree_roles_are_distinct() {
    let mut node = relate::PlanNode::new(
        "ProduceResults",
        "1 item(s)".to_string(),
        Vec::new(),
        Vec::new(),
    );
    node.estimated_rows = Some(3);
    let wire = relate::value_to_cypher(&node.to_neo4j_value("Query: RETURN 1\nPlan:"));
    let tree = PlanFixture::of(&node);
    let header = vec!["Query: RETURN 1".to_string(), "Plan:".to_string()];
    let cols = vec!["plan".to_string()];

    // Table mode walks `tree`: the decorated `wire` would panic here.
    let table = render_with_plan_roles(
        Formatter::new(Mode::Table),
        &cols,
        &[],
        &wire,
        &tree,
        &header,
    );
    assert!(table.contains("ProduceResults"), "{table}");

    // Json mode serializes the decorated `wire`.
    let json = render_with_plan_roles(
        Formatter::new(Mode::Json),
        &cols,
        &[],
        &wire,
        &tree,
        &header,
    );
    assert!(json.contains("string-representation"), "{json}");
}

#[test]
fn table_mode_renders_a_profile_plan_beside_data_rows() {
    // A PROFILE result keeps the wrapped statement's own data rows (an
    // integer cell, not a string line) — the plan presentation reads the
    // tree's own rendered lines, so no data-cell shape is assumed and no
    // cell can panic the renderer.
    let cols = vec!["n".to_string()];
    let rows = vec![vec![CypherValue::Int(1)]];
    let plan_lines = vec!["Query: RETURN 1 AS n".to_string(), "Plan:".to_string()];
    let s = render_plan(Mode::Table, &cols, &rows, &plan_fixture(), &plan_lines);
    // LABS-373: the data table prints first, then a blank line, then the
    // plan table headed by the statement's own lines.
    assert!(
        s.contains("| n |") && s.contains("| 1 |"),
        "the data table renders first: {s}"
    );
    assert!(
        s.contains("\n\nQuery: RETURN 1 AS n\nPlan:\n"),
        "the plan table follows it: {s}"
    );
    assert!(
        s.contains("| + ProduceResults"),
        "the boxed operator table: {s}"
    );
}

#[test]
fn table_mode_renders_a_zero_column_profile_plan() {
    // A bare CREATE/SET/DELETE under PROFILE returns no columns and no
    // rows, but the plan still renders: the plan arm sits before the
    // empty-columns guard, so the zero-column result shows its plan
    // (the old shape returned early on empty columns and never did).
    let header = vec!["Query: CREATE (n)".to_string(), "Plan:".to_string()];
    let s = render_plan(Mode::Table, &[], &[], &plan_fixture(), &header);
    assert!(
        s.starts_with("Query: CREATE (n)\nPlan:\n"),
        "the engine's header lines head the plan table: {s}"
    );
    assert!(
        s.contains("| + ProduceResults"),
        "the boxed operator table renders past the empty-columns guard: {s}"
    );
}

#[test]
fn table_mode_boxes_a_plan_shaped_query_result() {
    // A query that happens to return one `plan` column of strings is a
    // normal result: without the structured-plan marker on `meta`, no
    // shape ever selects the plan presentation.
    let cols = vec!["plan".to_string()];
    let rows = vec![
        vec![CypherValue::Str("step 1".into())],
        vec![CypherValue::Int(2)],
    ];
    let s = render(Mode::Table, &cols, &rows);
    assert!(s.contains("plan"), "boxed table: {s}");
    assert!(s.contains("+"), "boxed table: {s}");
}

#[test]
fn json_mode_emits_the_structured_plan_object() {
    // ADR-0359: an EXPLAIN result has no rows, so the machine-native
    // output is the structured plan object alone.
    let header = vec!["Query: RETURN 1".to_string(), "Plan:".to_string()];
    let plan = plan_fixture();
    let s = render_plan(Mode::Json, &[], &[], &plan, &header);
    let json: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(json["plan"]["operatorType"], "ProduceResults");
    // The text rows are not emitted beside the tree: the plan IS the
    // machine-native output. ADR-0363 decision 9 adds the complete (here
    // empty) notification list beside it, the plan object's second key.
    assert!(json.get("rows").is_none());
    assert_eq!(json["notifications"], serde_json::json!([]));
    assert_eq!(json.as_object().unwrap().len(), 2);
}

#[test]
fn json_mode_emits_profile_rows_beside_the_plan_object() {
    // LABS-373: a PROFILE result keeps its rows, so the machine-native
    // output carries both — `{"data": [...], "plan": {…}}`. A PROFILE
    // fixture (a metrics-carrying node): the class is the tree's own
    // fact, `PlanFixture` derives it, never a hand-stamped bool.
    let cols = vec!["n".to_string()];
    let rows = vec![vec![CypherValue::Int(1)]];
    // The header fixture builds through the canonical producer (ADR-0359's
    // header policy), never a hand-copy of its product.
    let header = plan_header_lines("RETURN 1 AS n");
    let plan = profile_fixture();
    let s = render_plan(Mode::Json, &cols, &rows, &plan, &header);
    let json: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(json["data"][0]["n"], 1, "{json}");
    assert_eq!(json["plan"]["operatorType"], "ProduceResults", "{json}");
}

#[test]
fn toon_mode_renders_the_structured_plan_tree() {
    // ADR-0359/LABS-373: an EXPLAIN result has no columns, so toon's
    // structural form is the `plan:` entry with the tree beneath it,
    // rendered by toon's own nested-value rule (inline compact JSON).
    let header = vec!["Query: RETURN 1".to_string(), "Plan:".to_string()];
    let plan = plan_fixture();
    let s = render_plan(Mode::Toon, &[], &[], &plan, &header);
    // The `plan:` entry plus the tree — no `rows[...]` header, no text
    // lines: the tree IS the output.
    assert_eq!(
        s,
        concat!(
            "plan:\n",
            "  {\"args\":{\"Details\":\"1 item(s)\",\"Id\":0},\"identifiers\":[],\"operatorType\":\"ProduceResults\"}\n",
            // ADR-0363 decision 9: the complete list beside `plan:`.
            "notifications:\n",
            "  []\n")
    );
}

#[test]
fn table_mode_renders_the_warnings_column_and_footer_line() {
    // Typed node warnings: the wire warnings render beside the affected
    // operator in the `Warnings` column and in the footer line after the
    // boxed table — the shared vocabulary's text.
    let header = vec![
        "Query: MATCH (a:Person), (b:City) RETURN a, b".to_string(),
        "Plan:".to_string(),
    ];
    let mut node = relate::PlanNode::new(
        "ProduceResults",
        "2 item(s)".to_string(),
        Vec::new(),
        vec![relate::PlanNode::new(
            "Expand",
            "(a:Person), (b:City)".to_string(),
            Vec::new(),
            Vec::new(),
        )
        .with_warnings(vec![relate::PlanWarning::CartesianProduct])],
    );
    node.assign_ids();
    let plan = PlanFixture::of(&node);
    let s = render_plan(Mode::Table, &[], &[], &plan, &header);
    assert!(s.contains("Warnings"), "the column and the footer: {s}");
    assert!(
        s.contains("the MATCH patterns share no join anchor"),
        "the shared vocabulary's message: {s}"
    );
    assert_eq!(
        s.lines().last().unwrap(),
        "Warnings: Id 1: the MATCH patterns share no join anchor",
        "the footer line names the affected operator's id: {s}"
    );
}

#[test]
fn table_mode_escapes_the_warnings_footer() {
    // The footer embeds the same warning texts as the `Warnings` cells, and
    // `AllNodesScan` carries catalog index text — a control sequence in an
    // index label must not reach the terminal raw.
    let cols = vec!["plan".to_string()];
    let rows = vec![
        vec![CypherValue::Str("Query: MATCH (n) RETURN n".into())],
        vec![CypherValue::Str("Plan:".into())],
    ];
    let mut node = relate::PlanNode::new(
        "ProduceResults",
        "1 item(s)".to_string(),
        Vec::new(),
        vec![
            relate::PlanNode::new("AllNodesScan", "(n)".to_string(), Vec::new(), Vec::new())
                .with_warnings(vec![relate::PlanWarning::AllNodesScan {
                    index: ":Person(\u{1b}[31m)".to_string(),
                }]),
        ],
    );
    node.assign_ids();
    let plan = PlanFixture::of(&node);
    let s = render_plan(Mode::Table, &cols, &rows, &plan, &[]);
    assert!(
        s.lines()
            .last()
            .is_some_and(|line| line.starts_with("Warnings: Id ")),
        "the escaped footer is the last line: {s}"
    );
    assert!(
        s.contains("\\u{1B}[31m"),
        "the index label's ESC is escaped: {s}"
    );
    assert!(
        !s.contains('\u{1b}'),
        "no raw control character reaches the terminal: {s}"
    );
}

/// The row-cap record rides **in-band** in every mode that can carry it
/// (ADR-0403): one capped fixture — two of five rows kept — pinned per
/// mode. The strict `csv` stream stays clean (its only channel is the
/// CLI's stderr warning, outside this crate); the uncut shapes are
/// pinned by the mode tests above and are unchanged.
#[test]
fn row_cap_records_ride_in_band_per_mode() {
    let cols = vec!["i".to_string()];
    let kept = vec![vec![CypherValue::Int(1)], vec![CypherValue::Int(2)]];
    // The cut's one stored fact is the total; the kept count is the
    // fixture's rows themselves.
    let cap = RowCap { total: 5 };

    // jsonl: the rows, then the keyed terminal record — the last line of
    // the statement's stream names both counts.
    assert_eq!(
        render_cap(Mode::Jsonl, &cols, &kept, cap),
        "{\"i\":1}\n{\"i\":2}\n{\"rowCap\":{\"kept\":2,\"total\":5}}\n"
    );

    // json: the capped result is wrapped — the PROFILE wrapper's shape —
    // so the type change from the bare array is itself the loud signal.
    // The default `Formatter` is pretty (ADR-0240), so the pin is the
    // indented form.
    assert_eq!(
        render_cap(Mode::Json, &cols, &kept, cap),
        "{\n  \"data\": [\n    {\n      \"i\": 1\n    },\n    {\n      \"i\": 2\n    }\n  ],\n  \"rowCap\": {\n    \"kept\": 2,\n    \"total\": 5\n  }\n}\n"
    );

    // envelope: the legacy `truncated` bool plus the counts, the `plan`
    // key's presence policy (present only when cut).
    assert_eq!(
        render_cap(Mode::Envelope, &cols, &kept, cap),
        concat!(
            "{\"columnTypes\":[\"INTEGER\"],\"columns\":[\"i\"],\"notifications\":[],",
            "\"rowCap\":{\"kept\":2,\"total\":5},\"rows\":[{\"i\":1},{\"i\":2}],",
            "\"truncated\":true}\n"
        )
    );

    // toon: a terminal `rowCap:` entry, the counts as one inline compact
    // JSON value — TOON's nested-value rule.
    assert_eq!(
        render_cap(Mode::Toon, &cols, &kept, cap),
        "rows[2]{i}:\n  1\n  2\nrowCap:\n  {\"kept\":2,\"total\":5}\n"
    );

    // markdown: the human transcript footer after the table.
    let out = render_cap(Mode::Markdown, &cols, &kept, cap);
    assert!(out.ends_with("> truncated: 2 of 5 rows shown\n"), "{out}");

    // csv: the strict format carries no record — the stream stays pure.
    let out = render_cap(Mode::Csv, &cols, &kept, cap);
    assert_eq!(out, "i\n1\n2\n");
}
