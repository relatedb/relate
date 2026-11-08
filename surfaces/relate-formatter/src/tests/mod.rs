//! Formatter test suites, split by concern: per-mode smoke tests
//! (`formats`), CSV quoting/formula/control-char handling (`csv`), the
//! shared `cypher_literal` / `cypher_identifier` rendering
//! (`cypher_literal`), unicode-width table layout and `.width` truncation
//! (`table_layout`), `.mode cypher` emission (`cypher_mode`), terminal escape /
//! control-char hardening (`escape`), the JSON/JSONL rendering pins
//! (`json_pin`), the speedscope evented-profile export pins
//! (`speedscope`), the plan-text rendering pins over real engine plan
//! trees (`plan_text`, ADR-0359), and the per-mode notification rendering
//! (`notifications`, ADR-0363).

use super::*;
use relate_model::wire;
use unicode_width::UnicodeWidthStr;

mod csv;
mod cypher_literal;
mod cypher_mode;
mod escape;
mod formats;
mod json_pin;
mod notifications;
mod plan_table;
mod plan_text;
mod speedscope;
mod table_layout;
mod timings;

/// The one render scaffold for the whole suite: run `f` over a single
/// column-ordered fixture and return the output as a `String`. `plan` is
/// the structured-plan marker (ADR-0345) an EXPLAIN statement carries —
/// `None` for plain runs. Tests that need a knob (`headers`,
/// `column_widths`) configure their own `Formatter` and call this
/// directly; everything else goes through `render`/`render_plan`.
fn render_with_plan(
    f: Formatter,
    cols: &[String],
    rows: &[Vec<CypherValue>],
    plan: Option<&PlanFixture>,
    header: &[String],
) -> String {
    // The fixtures carry one plan value in both roles: the engine's own
    // form and the Neo4j-facing form coincide here, so the typed walk
    // and the serializing modes see the same fixture — the degenerate
    // form of [`render_with_plan_roles`], built through the one
    // degenerate-view constructor (`notified_view`). The plan-less
    // fixtures carry no notifications, matching the engine's rule that
    // the list derives with the tree.
    render_with_meta(
        f,
        cols,
        rows,
        plan.map(|fixture| notified_view(&fixture.value, header, &[], fixture.profiled)),
        None,
        None,
    )
}

/// The roles form of the scaffold: run `f` over a single column-ordered
/// fixture carrying the plan value in its two distinct roles (ADR-0357)
/// — `wire`, the Neo4j-facing decorated value the serializing modes emit,
/// and `tree`, the engine's own wire form the typed table/speedscope
/// walk reads. [`render_with_plan`] is the single-value degenerate form
/// every other fixture uses.
fn render_with_plan_roles(
    f: Formatter,
    cols: &[String],
    rows: &[Vec<CypherValue>],
    wire: &CypherValue,
    fixture: &PlanFixture,
    header: &[String],
) -> String {
    render_with_meta(
        f,
        cols,
        rows,
        Some(PlanView {
            wire,
            tree: &fixture.value,
            header,
            notifications: &[],
            profiled: fixture.profiled,
        }),
        None,
        None,
    )
}

/// The common core of the scaffolds: run `f` over a single column-ordered
/// fixture, carrying the plan view (with its notification list) and the
/// LABS-1120 timing stamp.
fn render_with_meta(
    f: Formatter,
    cols: &[String],
    rows: &[Vec<CypherValue>],
    plan: Option<PlanView<'_>>,
    row_cap: Option<RowCap>,
    timings: Option<wire::TimingStampMs>,
) -> String {
    let rows = ordered_rows(cols, rows);
    // The fixtures see the `columnTypes` the engine would stamp, derived
    // via the one canonical mapping (ADR-0224): the first row's value
    // names the type; a missing first row or a missing cell stamps ANY
    // (Null's name), so no separate no-rows arm is needed.
    let column_types: Vec<String> = cols
        .iter()
        .map(|c| {
            wire::column_type_name(
                rows.first()
                    .and_then(|first| first.get(c))
                    .unwrap_or(&CypherValue::Null),
            )
            .to_string()
        })
        .collect();
    let mut out = Vec::new();
    f.format(
        &mut out,
        cols,
        &rows,
        StatementMeta {
            column_types: &column_types,
            row_cap,
            plan,
            timings,
        },
    )
    .unwrap();
    String::from_utf8(out).unwrap()
}

/// Render `rows` under `mode` with a row-cap cut (ADR-0403): the caller
/// states both counts — the fixture's `rows` are the kept ones.
fn render_cap(mode: Mode, cols: &[String], rows: &[Vec<CypherValue>], cap: RowCap) -> String {
    render_with_meta(Formatter::new(mode), cols, rows, None, Some(cap), None)
}

fn render_with(f: Formatter, cols: &[String], rows: &[Vec<CypherValue>]) -> String {
    render_with_plan(f, cols, rows, None, &[])
}

/// The one column-ordered-fixture mapping: pair each value with its column
/// name, so the row-shaped fixture the tests write turns into the
/// name-keyed rows the formatter takes.
fn ordered_rows(cols: &[String], rows: &[Vec<CypherValue>]) -> Vec<IndexMap<String, CypherValue>> {
    rows.iter()
        .map(|vals| cols.iter().cloned().zip(vals.iter().cloned()).collect())
        .collect()
}

/// Render `rows` under `mode` with a default `Formatter`.
fn render(mode: Mode, cols: &[String], rows: &[Vec<CypherValue>]) -> String {
    render_with(Formatter::new(mode), cols, rows)
}

/// Render `rows` under `mode` carrying a structured plan (ADR-0359) —
/// `header` is the plan table's own header lines (`plan_header_lines`);
/// `rows` are the statement's data rows (empty for an EXPLAIN run, which
/// has no rows of its own).
fn render_plan(
    mode: Mode,
    cols: &[String],
    rows: &[Vec<CypherValue>],
    plan: &PlanFixture,
    header: &[String],
) -> String {
    render_with_plan(Formatter::new(mode), cols, rows, Some(plan), header)
}

/// One boxed-table line's cells, normalized: the box's leading and
/// trailing pipes leave empty cells at the line's edges on some rows and
/// not others — drop them so the header's column indexes address every
/// operator row's cells the same way (the suite's one splitter — hoisted
/// here beside the fixtures so formats and plan_text share it).
fn boxed_cells(line: &str) -> Vec<&str> {
    let mut cells: Vec<&str> = line.split('|').map(str::trim).collect();
    while cells.first().is_some_and(|c| c.is_empty()) {
        cells.remove(0);
    }
    while cells.last().is_some_and(|c| c.is_empty()) {
        cells.pop();
    }
    cells
}

/// A plan fixture with its typed class (ADR-0344): the wire value plus
/// the classification its typed node states before conversion — the
/// scaffolds carry the pair, never a bare bool threaded by hand (the
/// class is a fact of the tree, `is_profiled()`).
struct PlanFixture {
    value: CypherValue,
    profiled: bool,
    /// The node's metrics (ADR-0344), carried beside the value so the
    /// assertions that render them consult the fixture's own facts,
    /// never magic copies of the numbers.
    metrics: Option<relate::ClauseMetrics>,
}

impl PlanFixture {
    /// One fixture from a typed node: the canonical conversion plus the
    /// node's own classification and metrics.
    fn of(node: &relate::PlanNode) -> PlanFixture {
        PlanFixture {
            value: relate::value_to_cypher(&node.to_value()),
            profiled: node.is_profiled(),
            metrics: node.metrics,
        }
    }
}

/// The structured plan fixture: a hand-built tree through the canonical
/// producer's own output (ADR-0344/0345). Never a hand-copy of the wire
/// form, so producer-to-consumer vocabulary drift fails in the tests
/// instead of shipping. Presence selects the plan presentation in table
/// mode, and the content (operatorType/args) builds the operator table.
fn plan_fixture() -> PlanFixture {
    PlanFixture::of(&relate::PlanNode::new(
        "ProduceResults",
        "1 item(s)".to_string(),
        Vec::new(),
        Vec::new(),
    ))
}

/// The PROFILE counterpart: the same ProduceResults node carrying metrics
/// (ADR-0344: metrics anywhere classifies the tree PROFILE) — the one
/// metrics-node spelling, so a test that wants the PROFILE shape and a
/// test that wants the metric columns read the same fixture.
fn profile_fixture() -> PlanFixture {
    PlanFixture::of(
        &relate::PlanNode::new(
            "ProduceResults",
            "1 item(s)".to_string(),
            Vec::new(),
            Vec::new(),
        )
        .with_metrics(Some(relate::ClauseMetrics {
            rows: 1,
            db_hits: 3,
            memory_bytes: 25,
            expression_built_bytes: 7,
            time_ns: 26_000,
            reads: Default::default(),
            phases: Default::default(),
            start: None,
            kernel: None,
            batched: false,
            zone_skips: 0,
            frontier: Default::default(),
            frontier_decline: None,
        })),
    )
}

/// The degenerate plan view the notified fixtures carry: one value in both
/// roles (the engine form and the Neo4j-facing form coincide), the table
/// header, and the engine-derived notification list.
fn notified_view<'a>(
    plan: &'a CypherValue,
    header: &'a [String],
    notifications: &'a [wire::Notification],
    profiled: bool,
) -> PlanView<'a> {
    PlanView {
        wire: plan,
        tree: plan,
        header,
        notifications,
        profiled,
    }
}

/// Render `rows` under `mode` carrying a structured plan and its
/// engine-derived notification list (ADR-0363) — the EXPLAIN/PROFILE
/// shape with notifications, `header` the plan table's own header lines
/// (`plan_header_lines`).
fn render_plan_notified(
    mode: Mode,
    cols: &[String],
    rows: &[Vec<CypherValue>],
    plan: &PlanFixture,
    header: &[String],
    notifications: &[wire::Notification],
) -> String {
    render_with_meta(
        Formatter::new(mode),
        cols,
        rows,
        Some(notified_view(
            &plan.value,
            header,
            notifications,
            plan.profiled,
        )),
        None,
        None,
    )
}

/// CSV output for a single-column fixture: `header` is the column name and
/// `value` the single data cell. Returns the full output; the wrappers below
/// pick the line they assert on.
fn csv_output(header: &str, value: CypherValue) -> String {
    let cols = vec![header.to_string()];
    render(Mode::Csv, &cols, &[vec![value]])
}

/// CSV output for a single string value, returning the data line.
fn csv_value_line(value: &str) -> String {
    csv_output("v", CypherValue::Str(value.into()))
        .lines()
        .nth(1)
        .unwrap()
        .to_string()
}

/// CSV output for a single column name, returning the header line.
fn csv_header_line(header: &str) -> String {
    csv_output(header, CypherValue::Int(0))
        .lines()
        .next()
        .unwrap()
        .to_string()
}

/// Count markdown table pipes that are not escaped with a backslash.
fn count_unescaped_pipes(line: &str) -> usize {
    let mut chars = line.chars();
    let mut n = 0;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '|' => n += 1,
            _ => {}
        }
    }
    n
}

/// Display width of a rendered line (unicode-aware), used to assert that
/// every border/row line in a table lines up to the same visual width.
fn line_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}
