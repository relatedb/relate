//! Relate output formatter.
//!
//! Formats query results as table, CSV, JSON, JSONL, Markdown, TOON,
//! Cypher, or a speedscope evented profile. The renderers live in
//! per-family modules: [`table`] (table + markdown) and [`plan_table`]
//! (the EXPLAIN/PROFILE plan table), both laying out through the shared
//! grid engine in [`layout`], plus [`csv`], [`json`] (json/jsonl/envelope
//! and the shared row projection), [`cypher`] (`.mode cypher` and the
//! canonical literal rendering), [`toon`], and `speedscope` (a PROFILE
//! plan tree as a speedscope evented-profile document).

mod csv;
mod cypher;
mod escape;
mod json;
mod layout;
mod notifications;
mod plan_table;
pub mod recording;
mod speedscope;
mod table;
mod toon;

pub use cypher::{cypher_identifier, cypher_literal, is_cypher_scalar};
pub(crate) use cypher::{is_bare_identifier, quoted_literal};
pub use escape::{is_unsafe_output_char, sanitize_error_text, strip_unsafe_output_chars};
pub use plan_table::{plan_header_lines, plan_presentation, plan_text_lines};

use std::io::Write;

use indexmap::IndexMap;
use relate_model::wire::Notification;
use relate_model::CypherValue;

/// One row of the mode vocabulary table ([`Mode::ALL`]): the variant,
/// its canonical name, its accepted aliases, and whether it is
/// machine-readable JSON — named fields, the `NotificationRow`
/// precedent, so accessors read what they mean, never a tuple
/// position.
struct ModeRow {
    mode: Mode,
    name: &'static str,
    aliases: &'static [&'static str],
    is_json: bool,
}

/// Output format mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Table,
    Csv,
    Json,
    Jsonl,
    /// One compact `{columns, rows, truncated}` object per result set
    /// (ADR-0195).
    Envelope,
    Markdown,
    Cypher,
    /// Token-Oriented Object Notation: one `rows[N]{col,…}:` header plus one
    /// comma-separated line per row (ADR-0239).
    Toon,
    /// A `PROFILE` plan tree as one speedscope evented-profile JSON
    /// document (`speedscope` module) — one frame per operator, `O`/`C`
    /// events in nanoseconds. A `PROFILE` statement only; a plain query
    /// or an `EXPLAIN` tree is an error (no per-operator times).
    Speedscope,
}

impl Mode {
    /// Every mode with its canonical name, its accepted aliases, and whether
    /// it is machine-readable JSON. The one table the `--mode` parser, the
    /// "expected one of" text, the help line and `is_json` are all built from,
    /// so adding a mode is one row here (the `format` dispatch and telemetry
    /// `From` matches are exhaustive, so the compiler flags those).
    const ALL: &'static [ModeRow] = &[
        ModeRow {
            mode: Mode::Table,
            name: "table",
            aliases: &["ascii"],
            is_json: false,
        },
        ModeRow {
            mode: Mode::Csv,
            name: "csv",
            aliases: &[],
            is_json: false,
        },
        ModeRow {
            mode: Mode::Json,
            name: "json",
            aliases: &[],
            is_json: true,
        },
        ModeRow {
            mode: Mode::Jsonl,
            name: "jsonl",
            aliases: &[],
            is_json: true,
        },
        ModeRow {
            mode: Mode::Envelope,
            name: "envelope",
            aliases: &[],
            is_json: true,
        },
        ModeRow {
            mode: Mode::Markdown,
            name: "markdown",
            aliases: &["md"],
            is_json: false,
        },
        ModeRow {
            mode: Mode::Cypher,
            name: "cypher",
            aliases: &[],
            is_json: false,
        },
        ModeRow {
            mode: Mode::Toon,
            name: "toon",
            aliases: &[],
            is_json: false,
        },
        ModeRow {
            mode: Mode::Speedscope,
            name: "speedscope",
            aliases: &[],
            is_json: true,
        },
    ];

    /// The canonical mode names in table order — the enumeration
    /// consumers iterate (tests included) and [`Mode::names`] joins:
    /// derived from the one table, so there is no second literal to
    /// drift.
    pub fn names_in_order() -> impl Iterator<Item = &'static str> {
        Self::ALL.iter().map(|row| row.name)
    }

    /// The canonical mode names joined with `", "`, in the order the CLI lists
    /// them: the text of every "expected one of" message and help line.
    pub fn names() -> String {
        Self::names_in_order().collect::<Vec<_>>().join(", ")
    }

    /// Parse a mode string (case-insensitive, canonical name or alias).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.to_lowercase();
        Self::ALL
            .iter()
            .find(|row| row.name == s || row.aliases.contains(&s.as_str()))
            .map(|row| row.mode.clone())
    }

    /// This mode's row of the table — the one lookup every accessor
    /// ([`Mode::name`], [`Mode::is_json`]) reads from, so the table has
    /// one find and one completeness invariant, not one per column.
    fn row(&self) -> &ModeRow {
        Self::ALL
            .iter()
            .find(|row| &row.mode == self)
            .expect("every Mode variant has a table row")
    }

    /// The mode's canonical name — the spelling [`Mode::names`] lists —
    /// from the same table, so a parsed alias (`md`) reports its
    /// canonical spelling (`markdown`) wherever a consumer prints it.
    pub fn name(&self) -> &'static str {
        self.row().name
    }

    /// Whether the mode is machine-readable JSON, so a failure also carries
    /// the ADR-0194 error envelope on stdout.
    pub fn is_json(&self) -> bool {
        self.row().is_json
    }
}

/// Formats query results for output.
pub struct Formatter {
    pub mode: Mode,
    pub headers: bool,
    /// Pretty-print the `json` mode (ADR-0240). Only `Mode::Json` consults
    /// it; `jsonl` and `envelope` are always compact.
    pub pretty: bool,
    /// The jsonl terminal timing line's opt-in (ADR-0581). Only
    /// `Mode::Jsonl` consults it: the `json`/`envelope` riders and the
    /// `toon` terminal entries always render when a stamp is passed.
    /// Defaults on — the library's render-when-passed behavior — while the
    /// CLI sets it once per run from its `--timings` flag, off by default,
    /// so its jsonl stream is pure rows. Named distinctly from
    /// [`StatementMeta::timings`], the per-statement stamp this gate
    /// reads: availability and rendering are two facts.
    pub render_timings: bool,
    /// The total display width the grid must fit (LABS-399) — the terminal
    /// width, when the caller knows it. `None` keeps the natural width: the
    /// stable choice for piped output. `table` and `plan_table` wrap cells
    /// to fit it; `markdown` ignores it (a GFM table has no wrapped cells).
    pub width: Option<usize>,
}

/// The row-cap cut (ADR-0196/0403): the one count a renderer cannot
/// derive — the rows the statement returned in full. The kept count is
/// the rendered `rows` themselves (`rows.len()` at every emit site),
/// never a second carrier here, and every machine mode renders the
/// record in-band so a capped result never reads as complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowCap {
    /// The rows the statement returned.
    pub total: usize,
}

impl RowCap {
    /// The record's wire key (ADR-0403) — the `envelope` key, the `jsonl`
    /// terminal line's, the `json` wrapper's, toon's entry header: one
    /// spelling beside the value it names, consulted at every emit site.
    pub(crate) const WIRE_KEY: &str = "rowCap";

    /// The record's one wire shape (ADR-0403): `{"kept": N, "total": M}`,
    /// built from the cut's own total and the caller's kept count (its
    /// rendered rows) — one spelling for the `envelope` mode's
    /// `"rowCap"` value and the `jsonl` terminal line's payload. The
    /// total rides the receiver, so a call site cannot transpose the two
    /// counts it renders.
    pub(crate) fn to_wire_json(self, kept: usize) -> serde_json::Value {
        serde_json::json!({ "kept": kept, "total": self.total })
    }
}

/// The per-statement metadata [`Formatter::format`] carries beside columns
/// and rows: one bundle, so new engine-stamped metadata extends the struct
/// instead of widening the shared signature. Which mode consumes which
/// field is documented on each field.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatementMeta<'a> {
    /// The engine-stamped per-column type names (ADR-0224) — same length
    /// and order as `columns`; the `envelope` mode reports it as
    /// `"columnTypes"` beside `"columns"`.
    pub column_types: &'a [String],
    /// The row-cap cut (ADR-0196/0403): `Some` with the statement's total
    /// row count when the caller capped `rows` below it — the kept count
    /// is the rendered `rows` themselves, read at the emit sites. Every
    /// machine mode renders the record **in-band** (ADR-0403) — the
    /// `envelope` mode as `"truncated"` plus `"rowCap"`, `jsonl` as a
    /// keyed terminal line, `json` as a `{"data", "rowCap"}` wrapper,
    /// `toon` as a `rowCap:` entry — so a capped result never reads as
    /// complete (the strict formats `csv`/`cypher` and the human `table`
    /// mode keep the CLI's stderr warning; `markdown` adds a footer line).
    pub row_cap: Option<RowCap>,
    /// The bundled plan-presentation facts of an `EXPLAIN`/`PROFILE`
    /// statement (ADR-0344/0345, ADR-0357) — a borrowed
    /// [`PlanPresentation`] view, `None` on plain runs; the bundle's
    /// inventory and invariants are spelled once on its own doc. The
    /// serializing modes (`json`,
    /// `jsonl`, `envelope`, `toon`) emit `PlanView::wire` — one
    /// `{"plan": ...}` object (json family) or one `plan:` entry with the
    /// tree beneath it (toon's nested-value rule) — while the typed
    /// table/speedscope walk reads `PlanView::tree`, the undecorated
    /// engine form (speedscope renders one document from it). The `table`
    /// mode's plan presentation reads `PlanView::header` — the header
    /// only, never the whole rendered text; its rows come from the wire
    /// tree's walk, never the result's data rows. `csv` and `cypher`
    /// render only the rows (the plan reaches them only for `EXPLAIN`,
    /// whose rows are the rendered lines); `markdown` renders the rows
    /// plus the complete notification list, never the plan table. In
    /// `table` mode its presence is the explicit marker that selects the
    /// plan presentation — never the result's data shape. The view owns
    /// the engine-derived notification list (ADR-0363), so a non-empty
    /// list without its plan view is unrepresentable.
    pub plan: Option<PlanView<'a>>,
    /// The statement's timing stamp (LABS-1120): the ADR-0221 total, its
    /// PARSE / PLAN / EXECUTE split, the fine sub-split, and the
    /// plan-cache outcome — the one projection (`relate::timing_stamp_ms`)
    /// every envelope-building surface calls. `Some` renders one keyed
    /// `timings` object (plus the `planCache` outcome beside it) in the
    /// serializing modes (`envelope`, `json`, `toon`); in `jsonl` the
    /// terminal line renders only while [`Formatter::render_timings`] is
    /// on (ADR-0581) — the stamp's availability and the run-level render
    /// gate are two facts, named distinctly. `None` omits both keys —
    /// the presence policy of `plan`, so a caller that renders without a
    /// result behind it changes nothing. The strict row formats (`csv`,
    /// `cypher`) and the human `table`/`markdown` modes never render it.
    pub timings: Option<relate_model::wire::TimingStampMs>,
}

impl<'a> StatementMeta<'a> {
    /// The statement's notification list (ADR-0363): the plan view's list,
    /// or the empty slice on a plain run. The list lives inside
    /// [`PlanView`] because the engine derives it with the plan tree, so
    /// the one place a list can come from is the view that owns it — no
    /// mode can be handed a list without the plan it belongs to, and the
    /// empty-on-a-plain-run rule is derived here, never re-spelled per
    /// renderer. Crate-internal: the renderers read it, a surface reads
    /// `PlanView::notifications` (or the statement's own result list).
    pub(crate) fn notifications(&self) -> &'a [Notification] {
        self.plan.map_or(&[], |view| view.notifications)
    }
}

/// The bundled plan-presentation facts on [`StatementMeta`] — the
/// renderer-facing **borrowed view** of every [`PlanPresentation`]
/// fact, mirrored by [`PlanPresentation::as_view`]. What the bundle
/// holds and the invariants that hold it together are spelled once, on
/// the bundle's own doc (the sealed field list is the inventory); this
/// view carries them, it never re-spells them.
#[derive(Debug, Clone, Copy)]
pub struct PlanView<'a> {
    /// The Neo4j-facing plan value (ADR-0357), already in wire form
    /// (`PlanNode::to_neo4j_value` converted once at the caller): the root
    /// decorated with the Neo4j-facing args. The serializing modes
    /// (`json`, `jsonl`, `envelope`, `toon`) emit this one.
    pub wire: &'a CypherValue,
    /// The engine's own wire form (`PlanNode::to_value`), converted once at
    /// the caller: the typed table/speedscope walk reads this one, whose
    /// `EstimatedRows` stays an integer — the decorated `wire` form's float
    /// mirror never reaches the engine-form accessors.
    pub tree: &'a CypherValue,
    /// The table presentation's header lines (`Query:` echo + `Plan:`
    /// marker) — this crate's one header rendering
    /// ([`plan_header_lines`], ADR-0359: the engine emits the tree only).
    pub header: &'a [String],
    /// The engine-derived query notifications (ADR-0363), one flat list
    /// derived with this plan tree; empty when the statement raised none.
    /// The `envelope` mode emits the complete list under `"notifications"`
    /// (native keys, always present, empty array included); `json`/`jsonl`
    /// carry it inside the plan object; `toon` renders a `notifications:`
    /// section beside `plan:`; `table` renders one `Notifications:` text
    /// block after the plan table, filtered to the kinds the plan table
    /// does not already render (ADR-0363 decision 8), and `markdown` —
    /// which never renders the plan table — renders the complete list.
    /// `csv` and `cypher` render only the rows. The list derives with the
    /// plan tree, so it is one fact with this view.
    pub notifications: &'a [Notification],
    /// The typed classification ([`relate::PlanNode::is_profiled`], ADR-0344):
    /// `true` for a `PROFILE` statement, `false` for `EXPLAIN` — stamped
    /// once at the boundary where the typed tree lives, never re-derived
    /// here from the result's shape. The serializing modes' plan-only arm
    /// and the row-cap invariant both consult it.
    pub profiled: bool,
}

/// The plan presentation of one result (ADR-0345, ADR-0357, ADR-0359): the
/// Neo4j-facing value the serializing modes emit, the engine's own wire form
/// the typed table/speedscope walk reads (its `EstimatedRows` stays an Int),
/// the table header lines, the engine-derived notification list, and the
/// typed EXPLAIN/PROFILE classification — one owned bundle so the five
/// cannot desync, constructed once at the boundary that holds the typed
/// `PlanNode`. The header and the plan text the Neo4j-facing
/// `string-representation` carries are this crate's one rendering
/// (ADR-0359: the engine emits the tree only).
/// The fields are sealed (`pub(crate)`): [`PlanPresentation::of`] and
/// [`PlanPresentation::as_view`] are the only paths, so the
/// cannot-desync claim in the doc above holds by compiler enforcement,
/// never by review.
pub struct PlanPresentation<'a> {
    /// The Neo4j-facing wire value, converted once from the typed tree.
    pub(crate) wire: CypherValue,
    /// The engine's own wire form, converted once.
    pub(crate) tree: CypherValue,
    /// The table presentation's header lines.
    pub(crate) header: Vec<String>,
    /// The engine-derived notification list, borrowed from the result.
    pub(crate) notifications: &'a [Notification],
    /// The typed classification ([`PlanView::profiled`]'s source), stamped
    /// from the typed tree — never re-derived from the result's shape.
    pub(crate) profiled: bool,
}

impl<'a> PlanPresentation<'a> {
    /// Build the presentation for a result's plan and its notification list,
    /// `None` on a plain run. A PROFILE result's rows are the wrapped
    /// statement's own data rows, so the header comes from the tree, not the
    /// rows. The notification list is the engine's (derived with the same
    /// tree), stored on the bundle so the view never takes facts beside it.
    pub fn of(tree: &relate::PlanNode, notifications: &'a [Notification]) -> PlanPresentation<'a> {
        PlanPresentation {
            wire: relate::value_to_cypher(&plan_presentation(tree)),
            tree: relate::value_to_cypher(&tree.to_value()),
            header: plan_header_lines(tree.statement_echo()),
            notifications,
            profiled: tree.is_profiled(),
        }
    }

    /// The formatter's view of this bundle: every presentation fact comes
    /// from `self`, so the table renderer's diagnostic (stderr) path prints
    /// the header built here rather than re-deriving one from an echo.
    pub fn as_view(&self) -> PlanView<'_> {
        PlanView {
            wire: &self.wire,
            tree: &self.tree,
            header: self.header.as_slice(),
            notifications: self.notifications,
            profiled: self.profiled,
        }
    }
}

impl Formatter {
    /// Create a new formatter with the given mode. Pretty-printing defaults
    /// on, the historical library behavior; the CLI overrides it from the
    /// terminal detection (ADR-0240). The jsonl timing line likewise
    /// defaults on (ADR-0581); the CLI overrides it from `--timings`.
    pub fn new(mode: Mode) -> Self {
        Formatter {
            width: None,
            mode,
            headers: true,
            pretty: true,
            render_timings: true,
        }
    }

    /// Format query results and write to the given writer. `meta` is the
    /// per-statement metadata bundle ([`StatementMeta`]); both are
    /// arguments, not formatter state, because they vary per statement.
    /// This is the one-writer form: table mode's plan presentation shares
    /// this writer — [`Formatter::format_with`] is the diagnostic-writer
    /// form.
    pub fn format(
        &self,
        w: &mut dyn Write,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> std::io::Result<()> {
        self.format_sinks(w, None, columns, rows, meta)
    }

    /// Format query results with a separate diagnostic writer. Table mode
    /// is the one mode that owns two streams: the data table goes to `w`
    /// and the plan presentation plus its notification block
    /// (ADR-0359/LABS-373, ADR-0363 decision 8) goes to `diagnostics`.
    /// Every other mode ignores `diagnostics` — its plan and notification
    /// payload rides `w` — so a caller may always pass both writers.
    pub fn format_with(
        &self,
        w: &mut dyn Write,
        diagnostics: &mut dyn Write,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> std::io::Result<()> {
        self.format_sinks(w, Some(diagnostics), columns, rows, meta)
    }

    /// The one dispatch behind both public entry points: `diagnostics` is
    /// the table arm's separate plan stream, or `None` to share `w`.
    fn format_sinks(
        &self,
        w: &mut dyn Write,
        diagnostics: Option<&mut dyn Write>,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> std::io::Result<()> {
        // The cap cuts rows only, so an EXPLAIN result (no data rows,
        // ADR-0359) can never carry one — the invariant every mode rests
        // on, kept loud in the one place they share, read off the view's
        // typed classification (never re-derived from the shape). The
        // kept count needs no contract: it is `rows.len()` at every emit
        // site, never a second carrier (ADR-0403).
        if meta.plan.is_some_and(|view| !view.profiled) {
            debug_assert!(
                meta.row_cap.is_none(),
                "ADR-0359/0403: an EXPLAIN result carries no rows to cap"
            );
        }
        match self.mode {
            Mode::Table => self.format_table(w, diagnostics, columns, rows, meta),
            Mode::Csv => self.format_csv(w, columns, rows),
            Mode::Json => self.format_json(w, columns, rows, meta),
            Mode::Jsonl => self.format_jsonl(w, columns, rows, meta),
            Mode::Envelope => self.format_envelope(w, columns, rows, meta),
            Mode::Markdown => self.format_markdown(w, columns, rows, meta),
            Mode::Cypher => self.format_cypher(w, columns, rows),
            Mode::Toon => self.format_toon(w, columns, rows, meta),
            Mode::Speedscope => self.format_speedscope(w, columns, rows, meta),
        }
    }
}

/// The cell for `col` in `row`, or `Null` when the row lacks the column —
/// the one fallback rule shared by the JSON projection and the
/// toon/cypher renderers.
pub(crate) fn cell<'a>(row: &'a IndexMap<String, CypherValue>, col: &str) -> &'a CypherValue {
    static NULL: CypherValue = CypherValue::Null;
    row.get(col).unwrap_or(&NULL)
}

/// True for the CypherValue variants with no scalar rendering (graph and
/// vector values): the one enumeration of the set, shared by
/// [`cypher::is_cypher_scalar`] and `json`'s display-string fallback, so
/// the renderers cannot disagree about it — a new non-scalar variant is a
/// compile error here (the match is exhaustive), not a silent `_` fallthrough
/// in two places.
pub(crate) fn is_graph_value(val: &CypherValue) -> bool {
    match val {
        CypherValue::Node { .. }
        | CypherValue::Relationship { .. }
        | CypherValue::Path { .. }
        | CypherValue::Geometry(_)
        | CypherValue::Vector { .. } => true,
        CypherValue::Null
        | CypherValue::Bool(_)
        | CypherValue::Int(_)
        | CypherValue::Float(_)
        | CypherValue::Str(_)
        | CypherValue::List(_)
        | CypherValue::Map(_)
        | CypherValue::Date(_)
        | CypherValue::Time(..)
        | CypherValue::DateTime(..)
        | CypherValue::LocalDateTime(_)
        | CypherValue::Duration(_)
        | CypherValue::Point(_) => false,
    }
}

/// The one writer-to-`Vec<String>` policy behind every public
/// line-returning renderer: run `render` over a byte buffer and split the
/// UTF-8 output into owned lines. Writing to a `Vec` cannot fail, and the
/// renderers only ever write UTF-8, so both invariants are loud here
/// instead of re-spelled at each entry point.
pub(crate) fn rendered_lines(
    render: impl FnOnce(&mut Vec<u8>) -> std::io::Result<()>,
) -> Vec<String> {
    let mut out = Vec::new();
    render(&mut out).expect("writing rendered lines to a Vec never fails");
    String::from_utf8(out)
        .expect("the rendering writes UTF-8")
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests;
