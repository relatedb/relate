//! The plan table presentation (ADR-0344/0345, LABS-449's home): the
//! boxed operator table the table mode renders for an EXPLAIN/PROFILE
//! result — the column vocabulary, the shared cell assembly, and the
//! operator-table walk, living with the one consumer that renders it
//! (the engine's own text renderer was deleted, which left the old
//! shared module with a single reader). The wire contract the engine
//! reads — the warning vocabulary, the estimation-miss policy — stayed
//! in `relate-model`. Reached only through [`crate::table`]'s
//! `format_plan` — one entry point, its helpers private to this module.

use std::io::Write;

use relate_model::wire::{
    estimation_miss, metric_u64, profiled_facts, ratio_defined, time_ns_ms, KernelRungWire,
    PlanMetricsWire, PlanProfiledFacts, PlanWarning, PLAN_UNAVAILABLE,
};
use relate_model::CypherValue;

use crate::escape::escape_control;
use crate::layout::{compute_layout, separator_line, write_row_lines};
use crate::notifications::format_notifications;
use crate::PlanView;

/// The plan-presentation path of [`Formatter::format_table`]
/// (ADR-0344/0345): the `Query:` echo and the `Plan:` marker (the
/// engine's one header rendering, passed as `header`) print raw — a
/// statement can be arbitrarily large and a box stretched to its width
/// would bury the operator table — and the operator table is built from the
/// STRUCTURED plan via the typed wire accessor (never a re-parse of the
/// text the engine rendered from the same structure), one column model
/// driving the header, the estimate gate, and every row, each line
/// boxed as its own row so the eye cannot slip into the next
/// operator's line, cells escaping through the one boundary every
/// other cell passes. The typed warnings footer (typed node warnings)
/// prints as one escaped line after the boxed table — the shared footer
/// policy in wire, the same entries every renderer prints.
pub(crate) fn format_plan(
    w: &mut dyn Write,
    tree: &CypherValue,
    header: &[String],
    width: Option<usize>,
) -> std::io::Result<()> {
    // Escape every header line exactly once (the table path's boundary):
    // the `Query:` echo embeds the statement text, so terminal control
    // characters must not reach it raw — the same pass every cell takes.
    for line in header {
        writeln!(w, "{}", escape_control(line, false))?;
    }
    // The walk reports its facts: fixed-shape rows (a cell per column,
    // the estimate cell the shared unavailable spelling when the node
    // carries none) and the column model the table renders.
    let mut walk = PlanWalk::default();
    // The subtree totals, one pass over the tree (the one map every row's
    // cumulative cell reads — never a re-fold per node).
    let totals = relate_model::wire::plan_subtree_totals(tree);
    // The typed walk reads the engine's own wire form (ADR-0357): the
    // Neo4j-facing `wire` value's float `EstimatedRows` never reaches
    // `plan_node_wire`'s engine-form contract.
    walk.push_node_at(tree, "", true, &totals);
    // LABS-449: the flag bundle, folded per node during the one walk
    // above — the header assembly, the cell assembly and the footer
    // check read it once the walk is done, never a re-walk or a second
    // derivation.
    let flags = walk.flags;
    // Escape every header and cell exactly once (the table path's
    // boundary, applied to the plan table's own materialized strings);
    // layout measures the escaped strings.
    let headers: Vec<String> = plan_columns(flags)
        .into_iter()
        .map(|h| escape_control(h, false))
        .collect();
    let cells: Vec<Vec<String>> = walk
        .shape()
        .into_iter()
        .map(|row| row.iter().map(|c| escape_control(c, false)).collect())
        .collect();
    let widths = compute_layout(&headers, &cells, width);
    let separator = separator_line(&widths);
    writeln!(w, "{separator}")?;
    for row in [headers].into_iter().chain(cells) {
        write_row_lines(w, &row, &widths)?;
        writeln!(w, "{separator}")?;
    }
    if flags.warnings {
        // The footer embeds the same warning texts as the `Warnings` cells —
        // `AllNodesScan` carries catalog index text — so it passes the same
        // one escaping boundary: a control character in an index label must
        // not reach the terminal raw.
        writeln!(w, "{}", escape_control(&walk.warnings_footer(), false))?;
    }
    Ok(())
}

/// One column of the plan table (LABS-449, the descriptor list): the
/// vocabulary — the header name, the gate that renders it, and the cell
/// each walked node produces — spelled once, in [`PlanColumn::ALL`]'s
/// render order. The header assembly filters the list by the gate; the
/// cell assembly walks the same filtered list — never a parallel
/// `len()`, a group-membership arithmetic, or a second gated
/// push-spelling that can drift from the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlanColumn {
    /// The operator cell, already carrying its tree-glyph prefix.
    Operator,
    /// The node's pre-order id.
    Id,
    /// The one-line details (the walk's chosen start rides it, LABS-369).
    Details,
    /// The run-state mark (LABS-343): renders only when some operator
    /// never ran.
    Executed,
    /// The cardinality estimate: renders when any node carries one.
    EstimatedRows,
    /// The operator's produced rows (the metric group, ADR-0344 phase 2).
    Rows,
    /// The operator's own elapsed time.
    TimeMs,
    /// The subtree total of that time (cumulative vs self, ADR-0396).
    TotalMs,
    /// The store-access total.
    DbHits,
    /// The node-record reads (ADR-0397): renders only when some operator
    /// read node records; a zero reads empty, so the counts stand out.
    Nodes,
    /// The relationships followed (ADR-0397): same gate and zero-empty
    /// policy as [`PlanColumn::Nodes`].
    Rels,
    /// The adjacency entries enumerated (ADR-0397): same gate and
    /// zero-empty policy as [`PlanColumn::Nodes`].
    Degree,
    /// The row-materialization bytes.
    MemoryBytes,
    /// The expression allocation bytes (ADR-0463): the metric group's
    /// closer, reported separately from the row bytes beside it.
    ExpressionBuiltBytes,
    /// The estimated-vs-actual ratio: renders where the metric group and
    /// the estimate column both render.
    Ratio,
    /// The typed warnings' messages (typed node warnings).
    Warnings,
}

impl PlanColumn {
    /// The full list, in render order — the one order the header and the
    /// cells both walk. A hand-maintained list over the enum's
    /// vocabulary: the drift guard is the tests' concrete-name pins
    /// (the all-off and widest bundles assert the exact headers), so a
    /// variant added without a render-list entry fails there — not the
    /// compiler.
    const ALL: [Self; 16] = [
        Self::Operator,
        Self::Id,
        Self::Details,
        Self::Executed,
        Self::EstimatedRows,
        Self::Rows,
        Self::TimeMs,
        Self::TotalMs,
        Self::DbHits,
        Self::Nodes,
        Self::Rels,
        Self::Degree,
        Self::MemoryBytes,
        Self::ExpressionBuiltBytes,
        Self::Ratio,
        Self::Warnings,
    ];

    /// The column's header name — the one spelling, read by the header
    /// assembly and by every test that pins a column by name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Operator => "Operator",
            Self::Id => "Id",
            Self::Details => "Details",
            Self::Executed => "Executed",
            Self::EstimatedRows => "Estimated Rows",
            Self::Rows => "Rows",
            Self::TimeMs => "Time (ms)",
            Self::TotalMs => "Total (ms)",
            Self::DbHits => "Db Hits",
            Self::Nodes => "Nodes",
            Self::Rels => "Rels",
            Self::Degree => "Degree",
            Self::MemoryBytes => "Memory (Bytes)",
            Self::ExpressionBuiltBytes => "Expression Built (Bytes)",
            Self::Ratio => "Est/Actual",
            Self::Warnings => "Warnings",
        }
    }

    /// Whether the column renders for a flag bundle — the one gate the
    /// header and the cells both consult: the base columns always; the
    /// run state when some operator never ran; the estimate when a node
    /// carried one; the metric group on PROFILE trees, its kind columns
    /// each gated on some operator's nonzero count, the ratio where the
    /// estimate and the metrics both render; the warnings when a node
    /// carried one.
    fn renders(self, flags: &PlanColumnFlags) -> bool {
        match self {
            Self::Operator | Self::Id | Self::Details => true,
            Self::Executed => flags.executed,
            Self::EstimatedRows => flags.estimate,
            Self::Rows
            | Self::TimeMs
            | Self::TotalMs
            | Self::DbHits
            | Self::MemoryBytes
            | Self::ExpressionBuiltBytes => flags.metrics.is_some(),
            Self::Nodes => flags.metrics.is_some_and(|k| k.nodes),
            Self::Rels => flags.metrics.is_some_and(|k| k.rels),
            Self::Degree => flags.metrics.is_some_and(|k| k.degree),
            Self::Ratio => flags.metrics.is_some() && flags.estimate,
            Self::Warnings => flags.warnings,
        }
    }

    /// One walked node's cell for this column — the one spelling the cell
    /// assembly walks. The metric columns and the run state share the
    /// triad policies (the never-executed marker / mark, the shared
    /// unavailable spelling for a node the metrics do not describe);
    /// a zero kind count reads empty (ADR-0397); the ratio renders only
    /// where its cell is defined.
    fn cell(self, row: &PlanRow) -> String {
        let PlanRow {
            operator,
            id,
            details,
            estimated,
            metrics,
            warnings,
            ..
        } = row;
        match self {
            Self::Operator => operator.to_string(),
            Self::Id => id.to_string(),
            Self::Details => details_cell(details, metrics.as_ref().map(|f| &f.metrics)),
            Self::Executed => gated_cell(
                row,
                // A node the metrics do not describe (a synthetic joiner)
                // renders the unavailable spelling, never a fabricated
                // "ran" beside unavailable metric cells; a starved
                // operator reads the column's never-ran mark.
                NOT_EXECUTED_MARK,
                |_| EXECUTED_MARK.to_string(),
            ),
            Self::EstimatedRows => estimated
                .map(|n| n.to_string())
                .unwrap_or_else(|| PLAN_UNAVAILABLE.to_string()),
            Self::Rows => metric_cell(row, |f| f.metrics.rows.to_string()),
            Self::TimeMs => metric_cell(row, |f| metric_time_ms(f.metrics.time_ns)),
            // The cumulative cell: the bundle carries the total with the
            // metrics, so the cell reads a plain field — a fallback
            // spelling cannot exist (ADR-0396).
            Self::TotalMs => metric_cell(row, |f| metric_time_ms(f.total_time_ns)),
            Self::DbHits => metric_cell(row, |f| f.metrics.db_hits.to_string()),
            Self::Nodes => metric_cell(row, |f| read_kind_cell(f.metrics.reads.node_reads)),
            Self::Rels => metric_cell(row, |f| read_kind_cell(f.metrics.reads.hops)),
            Self::Degree => metric_cell(row, |f| read_kind_cell(f.metrics.reads.neighbor_walks)),
            Self::MemoryBytes => metric_cell(row, |f| f.metrics.memory_bytes.to_string()),
            Self::ExpressionBuiltBytes => {
                metric_cell(row, |f| f.metrics.expression_built_bytes.to_string())
            }
            Self::Ratio => match (metrics.as_ref(), *estimated) {
                (Some(f), Some(estimated)) => {
                    ratio_cell(metric_u64(estimated), metric_u64(f.metrics.rows))
                }
                _ => None,
            }
            .unwrap_or_else(|| PLAN_UNAVAILABLE.to_string()),
            Self::Warnings => {
                if warnings.is_empty() {
                    PLAN_UNAVAILABLE.to_string()
                } else {
                    warnings.join(", ")
                }
            }
        }
    }
}

/// The `Details` cell: the operator's details, then each PROFILE mark its
/// metrics carry as `  [<mark>]` — the one append spelling every mark
/// rides ([`details_marks`] decides which marks are present).
fn details_cell(details: &str, metrics: Option<&PlanMetricsWire>) -> String {
    let mut cell = details.to_string();
    for mark in metrics.map(details_marks).unwrap_or_default() {
        cell.push_str("  [");
        cell.push_str(&mark);
        cell.push(']');
    }
    cell
}

/// The PROFILE marks one operator's metrics carry, in render order.
/// LABS-369: the walk's chosen start — the fact an opaque `Expand` hid,
/// right beside the pattern it walked. ADR-0468 (LABS-359): the batch
/// chain's mark, the scan/filter/project interior the clause ran; LABS-645
/// its zone-skip count, the conjunct-batch evaluations the chunk's derived
/// min/max zone metadata provably replaced with the correct empty
/// survivor set. ADR-0478: the frontier family's mark, the BFS operator
/// the clause ran with its run counters; ADR-0516 the NFA rung's own facts
/// (the re-arrival rule's budget fraction, memo, and admission verdicts)
/// beside it, both only when the family ran (`levels > 0`), and the
/// run-time decline's typed reason — the gate that kept the DFS, named on
/// the row it declined.
fn details_marks(metrics: &PlanMetricsWire) -> Vec<String> {
    let mut marks = Vec::new();
    if let Some(start) = &metrics.start {
        let direction = if start.backward {
            "backward"
        } else {
            "forward"
        };
        marks.push(format!(
            "start #{}, {direction}, {}",
            start.element, start.reason
        ));
    }
    // LABS-1117: the traversal rung that served the clause's MATCH —
    // the chain (with its hop count), the hub-star, the frontier-NFA
    // rung, or the per-row walker. The typed value carries the pairing,
    // so no string comparison can silently drop the hop count.
    if let Some(rung) = metrics.kernel {
        let name = rung.as_str();
        marks.push(match rung {
            KernelRungWire::Chain { hops } => format!("kernel {name} hops={hops}"),
            _ => format!("kernel {name}"),
        });
    }
    if metrics.batched {
        marks.push("batched".to_string());
    }
    if metrics.zone_skips > 0 {
        marks.push(format!("zone-skips {}", metrics.zone_skips));
    }
    let frontier = &metrics.frontier;
    if frontier.levels > 0 {
        marks.push(format!(
            "frontier edges={} levels={} endpoints={} cells={}",
            frontier.edge_visits, frontier.levels, frontier.endpoints, frontier.cells
        ));
        marks.push(format!(
            "frontier-rule budget={:.4} memo-hits={} memo-misses={} \
             representative-proofs={} search-proven={} \
             search-disproven={} declined-exhausted={}",
            frontier.budget_fraction,
            frontier.memo_hits,
            frontier.memo_misses,
            frontier.representative_proofs,
            frontier.search_proven,
            frontier.search_disproven,
            frontier.declined_exhausted
        ));
    }
    if let Some(reason) = metrics.frontier_decline {
        marks.push(format!("frontier-declined {reason}"));
    }
    marks
}

/// One metric-state column's cell triad — the one match over the three
/// states every such column renders: the shared unavailable spelling for
/// a node the metrics do not describe, the starved spelling (the metric
/// cells' `NEVER_EXECUTED` marker, the `Executed` column's
/// `NOT_EXECUTED_MARK`) for an operator that did no work, else the value
/// — the value closure reads the bundled facts, so a cell cannot render
/// beside unavailable metrics or fabricate one. The two leaf policies
/// (which starved spelling, which value) arrive as parameters; the
/// branch structure lives here, spelled once.
fn gated_cell(
    row: &PlanRow,
    starved: &str,
    value: impl FnOnce(&PlanProfiledFacts) -> String,
) -> String {
    match row.metrics.as_ref() {
        None => PLAN_UNAVAILABLE.to_string(),
        Some(f) if never_executed_metrics(&f.metrics, row.input_rows, row.is_root) => {
            starved.to_string()
        }
        Some(f) => value(f),
    }
}

/// One metric column's cell: the triad with the metric group's starved
/// spelling — `gated_cell` with [`NEVER_EXECUTED`].
fn metric_cell(row: &PlanRow, value: impl FnOnce(&PlanProfiledFacts) -> String) -> String {
    gated_cell(row, NEVER_EXECUTED, value)
}

/// One kind column's cell (ADR-0397): the count, empty when zero — a
/// measured none renders as absence so the nonzero counts stand out
/// vertically. Distinct from the shared unavailable spelling: that
/// one means "not produced" (no metric set at all), never zero.
fn read_kind_cell(value: i64) -> String {
    if value == 0 {
        String::new()
    } else {
        value.to_string()
    }
}

/// The warning marker the ratio cell carries when the estimate
/// overshoots the actual rows by 10x or more.
const RATIO_WARNING: &str = "!";
/// The column's two marks: ran, never ran. The never-ran mark is the
/// column's compact form of the same state `NEVER_EXECUTED` spells out
/// in the metric cells — one column cannot hold the phrase, so a
/// starved row reads `x` in this column beside `(never executed)` in
/// each metric cell; both derive from the one `never_executed_node`
/// predicate, never two policies.
pub(crate) const EXECUTED_MARK: &str = "✓";
pub(crate) const NOT_EXECUTED_MARK: &str = "x";

/// The one footer-line policy for the plan's typed warnings: one
/// `Id <id>: <message>` entry per warning in the tree's pre-order,
/// `; `-joined after the `Warnings` header — one spelling, in the module that renders it, so the footer cannot drift between the text forms.
pub(crate) fn warnings_footer_line(entries: &[(i64, &str)]) -> String {
    let joined = entries
        .iter()
        .map(|(id, message)| format!("Id {id}: {message}"))
        .collect::<Vec<_>>()
        .join("; ");
    format!("{}: {joined}", PlanColumn::Warnings.name())
}

/// The one spelling of an operator's elapsed-time cell (the `Time
/// (ms)` column): milliseconds, three decimals — shared by the engine
/// renderer and every surface that lays the plan table out, so the
/// precision cannot drift between the text forms. The wire form's
/// consumer side: a wire time is non-negative when present (the
/// producer saturates), so the gate is here — the one conversion, at
/// the function the wire's `i64` times feed.
pub(crate) fn metric_time_ms(time_ns: i64) -> String {
    format!("{:.3}", time_ns_ms(time_ns))
}

/// The never-executed marker (ADR-0344 metric fields): an operator
/// whose input and output both carried zero rows did no work — its
/// metric cells render this marker, never zeros. One spelling shared
/// by the one table layout.
const NEVER_EXECUTED: &str = "(never executed)";
/// The one never-executed derivation policy (ADR-0344 metric
/// fields): an operator never executed when it is not the tree's
/// root and it produced no rows and its input clause produced none
/// either (`input_rows` is the child operator's rows in the plan
/// tree's bottom-up nesting; a missing child means the statement's
/// one seed row, so a first clause never matches). The root is
/// excluded here — the projection always executes, and its zeros
/// are the answer, not a marker — so both halves of the policy live
/// in this one function, never a `!is_root` guard beside it at the
/// caller.
pub(crate) fn never_executed(rows_out: u64, input_rows: Option<u64>, is_root: bool) -> bool {
    !is_root && rows_out == 0 && input_rows == Some(0)
}

/// The one composition of the raw three-fact predicate above over a
/// node's matched metric set (LABS-343): both of the policy's readers
/// call here directly on the bundle they already matched — the cell
/// triad's guard (`gated_cell`, which renders the metric cells' marker
/// and the `Executed` column's mark) and the flag fold's executed gate —
/// so the policy is applied in exactly one place, never a re-wrapped
/// Option beside the match that opened it.
fn never_executed_metrics(m: &PlanMetricsWire, input_rows: Option<u64>, is_root: bool) -> bool {
    never_executed(metric_u64(m.rows), input_rows, is_root)
}
/// The one estimated-vs-actual ratio cell policy (ADR-0344 metric
/// fields): the ratio renders as `{n.n}x` on nodes carrying both an
/// estimate and actual rows, carrying the warning marker when
/// [`estimation_miss`] fires. `None` when the ratio is
/// undefined (no actual rows — division by zero stays a skip, never
/// a fabricated number).
pub(crate) fn ratio_cell(estimated: u64, actual: u64) -> Option<String> {
    ratio_defined(actual).then(|| {
        let ratio = estimated as f64 / actual as f64;
        let mut cell = format!("{ratio:.1}x");
        if estimation_miss(estimated, actual) {
            cell.push(' ');
            cell.push_str(RATIO_WARNING);
        }
        cell
    })
}
/// The tree-glyph vocabulary: the node prefix and the per-ancestor
/// indent step of the operator column's drawing.
const GLYPH_NODE: &str = "+ ";
const GLYPH_INDENT: &str = "| ";

/// The full column list of the operator table (LABS-449): the one
/// descriptor list, filtered by each column's gate — the header
/// assembly every renderer reads, so the column order cannot drift
/// between them, and the cell assembly provably matches it (it walks
/// the same filtered list).
pub(crate) fn plan_columns(flags: PlanColumnFlags) -> Vec<&'static str> {
    rendered(&flags).map(PlanColumn::name).collect()
}

/// The one filtered descriptor list (LABS-449): the render order gated by
/// each column's flag predicate — the single pipeline the header
/// assembly and the cell assembly both consume, so "the header and the
/// cells walk the same list" holds by construction, not by two
/// textually-identical expressions.
fn rendered<'a>(flags: &'a PlanColumnFlags) -> impl Iterator<Item = PlanColumn> + 'a {
    PlanColumn::ALL
        .iter()
        .copied()
        .filter(|column| column.renders(flags))
}

/// One node's row cells, the per-node assembly this module
/// builds through: the same descriptor list the header walks, filtered by
/// the same gates — each column's cell from the column's own cell policy
/// — so the cells cannot drift from the headers in count, order, or
/// content.
pub(crate) fn plan_row_cells(row: &PlanRow, flags: PlanColumnFlags) -> Vec<String> {
    rendered(&flags).map(|column| column.cell(row)).collect()
}

/// The plan table's column flags (LABS-449): the metric group's presence
/// carries its kind gates — a kind without the metric set is
/// unrepresentable, so [`PlanColumn::renders`] reads the nesting directly,
/// never a defensive conjunction.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlanColumnFlags {
    /// The estimate column renders (any node carries an estimate).
    pub(crate) estimate: bool,
    /// The metric columns render (a PROFILE tree) — and when they do, the
    /// kinds that render with them.
    pub(crate) metrics: Option<MetricKinds>,
    /// The warnings column renders (any node carries a typed warning).
    pub(crate) warnings: bool,
    /// The executed column renders (any operator was never executed) —
    /// LABS-343: one at-a-glance run/never-ran cell instead of reading the
    /// marker out of every metric cell.
    pub(crate) executed: bool,
}

/// The read-kind columns that render with the metric group (ADR-0397):
/// each gates on some operator's nonzero count of that kind, folded
/// under the metric set that carries it.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MetricKinds {
    /// `Nodes` — node-store reads, a scan/label-match/index-seek
    /// candidate each.
    pub(crate) nodes: bool,
    /// `Rels` — relationships followed to their other endpoint (the
    /// wire's `hops` kind).
    pub(crate) rels: bool,
    /// `Degree` — adjacency enumerations, one per neighbor a walk read
    /// (the wire's `neighbor_walks` kind).
    pub(crate) degree: bool,
}

impl PlanColumnFlags {
    /// Fold one walked node's bundle into the flags (LABS-449): the walk
    /// calls this per node as it collects — the one derivation over the
    /// rows the walk already builds, through the one per-node contract
    /// the cell assembly reads, never a second traversal of the tree or
    /// a re-split into positionals.
    fn fold_row(&mut self, row: &PlanRow) {
        self.estimate |= row.estimated.is_some();
        if let Some(metrics) = row.metrics.as_ref() {
            let kinds = MetricKinds {
                nodes: metrics.metrics.reads.node_reads > 0,
                rels: metrics.metrics.reads.hops > 0,
                degree: metrics.metrics.reads.neighbor_walks > 0,
            };
            // The kinds fold into the group the first metrics-carrying
            // node opened — a kind cannot exist without the metric set
            // that carries it.
            self.metrics = Some(match self.metrics {
                None => kinds,
                Some(k) => MetricKinds {
                    nodes: k.nodes || kinds.nodes,
                    rels: k.rels || kinds.rels,
                    degree: k.degree || kinds.degree,
                },
            });
            self.executed |= never_executed_metrics(&metrics.metrics, row.input_rows, row.is_root);
        }
        self.warnings |= !row.warnings.is_empty();
    }
}

/// The operator-table walk over the structured plan wire form
/// (ADR-0344/0345): one fixed-shape row per node and the column model —
/// the header names, the estimate column's presence (a fact the walk
/// reports, not a downstream row-length re-derivation) — in one place.
#[derive(Default)]
struct PlanWalk {
    /// The column flags, folded per node as the walk collects (LABS-449)
    /// — the one derivation over the rows the walk builds, read once the
    /// walk is done.
    flags: PlanColumnFlags,
    /// Typed facts collected during the walk — one entry per node; the
    /// string cells materialize once in [`PlanWalk::shape`], after the
    /// column model is known. The column model derives from these rows
    /// (a node carried an estimate / metrics), never from parallel bools.
    rows: Vec<PlanRow>,
}

/// One node's typed facts: the operator with its tree-glyph prefix
/// (glyphs are layout facts, materialized with the operator), the id,
/// the details, the estimate when the node carries one, the PROFILE
/// metric set, and the node's structural metric-derivation facts: the
/// input clause's row count (the bottom-up nesting puts each earlier
/// clause under the one it feeds) and whether this node is the tree's
/// root (the projection always executes, so the never-executed marker
/// never applies to it).
pub(crate) struct PlanRow {
    pub(crate) operator: String,
    pub(crate) id: i64,
    pub(crate) details: String,
    pub(crate) estimated: Option<i64>,
    /// The PROFILE metric set — one Option, because the wire form emits
    /// the four metric fields together; a half-present set fails loudly
    /// at the wire accessor, never here.
    pub(crate) metrics: Option<relate_model::wire::PlanProfiledFacts>,
    /// The input clause's produced rows — this operator's input, `None`
    /// when the node has no input child (the statement's seed row).
    pub(crate) input_rows: Option<u64>,
    /// Whether this node is the tree's root.
    pub(crate) is_root: bool,
    /// The typed warnings the node carries (typed node warnings): the
    /// message texts, materialized at walk time through the typed
    /// warning's own [`PlanWarning::message`] — the vocabulary's one
    /// spelling (the suggested property index included), so the consumer
    /// renders the engine's own message text, never a second form.
    pub(crate) warnings: Vec<String>,
}

impl PlanWalk {
    /// The recursive walk. `is_root` marks
    /// the tree's root (only the first pushed node): the projection
    /// always executes, so the never-executed marker never applies to
    /// it. Each row's `input_rows` — the input clause's produced rows,
    /// the structural fact the shared never-executed derivation reads —
    /// is read from the node's own first child (the bottom-up nesting
    /// puts each earlier clause under the one it feeds): a node with no
    /// child started from the statement's one seed row, `None`.
    fn push_node_at(
        &mut self,
        plan: &CypherValue,
        indent: &str,
        is_root: bool,
        totals: &relate_model::IndexMap<i64, i64>,
    ) {
        let node = relate_model::wire::plan_node_wire(plan);
        // The one input-rows derivation (LABS-449): the typed view's own
        // method, never a second walk-local spelling.
        let input_rows = node.input_rows();
        // The row is built as a local (LABS-449): the fold reads its
        // bundle before it is pushed, never a push-then-read-back.
        let row = PlanRow {
            operator: format!("{indent}{GLYPH_NODE}{}", node.operator_type),
            id: node.id,
            details: node.details.to_string(),
            estimated: node.estimated_rows,
            metrics: node
                .metrics
                .map(|metrics| profiled_facts(metrics, totals, node.id)),
            input_rows,
            is_root,
            warnings: node.warnings.iter().map(PlanWarning::message).collect(),
        };
        // LABS-449: fold this node's facts into the column flags — the one
        // derivation, during the one walk, over the row's own bundle.
        self.flags.fold_row(&row);
        self.rows.push(row);
        for child in node.children {
            self.push_node_at(child, &format!("{indent}{GLYPH_INDENT}"), false, totals);
        }
    }

    /// The warnings footer line (typed node warnings): every warning in
    /// the walk's pre-order — the same order the `Id` column numbers —
    /// through the shared footer policy in wire.
    fn warnings_footer(&self) -> String {
        let entries: Vec<(i64, &str)> = self
            .rows
            .iter()
            .flat_map(|row| {
                row.warnings
                    .iter()
                    .map(move |message| (row.id, message.as_str()))
            })
            .collect();
        warnings_footer_line(&entries)
    }

    /// The rows as string cells, materialized once against the column
    /// model: the estimate cell renders only when its column does (the
    /// shared unavailable spelling fills an estimate-less node there).
    fn shape(&self) -> Vec<Vec<String>> {
        self.rows
            .iter()
            .map(|row| plan_row_cells(row, self.flags))
            .collect()
    }
}

/// The boundary's plan presentation for one result (ADR-0359): the engine's
/// Neo4j-facing value with the plan text rendered through the one layout
/// inside it — the composition every surface (CLI, Bolt, HTTP) calls, so
/// the rendering and the decoration cannot drift. The summary KEY stays
/// surface-specific (`plan`/`profile` for Bolt, `queryPlan`/
/// `profiledQueryPlan` for HTTP, the formatter's own modes for the CLI),
/// read off `tree.is_profiled()` at the caller. A caller needing the text
/// itself (a header) reads [`plan_header_lines`], not this return.
///
/// Unlike [`plan_text_lines`], this takes the engine's `PlanNode`: it must
/// call the engine's decorator, and the wire value alone cannot produce the
/// decorated form. ADR-0359's rejected alternative was the *renderer*
/// taking `PlanNode`; the renderer still takes the wire value.
pub fn plan_presentation(tree: &relate::PlanNode) -> relate::Value {
    // The echo is a plain accessor (empty on a hand-built tree): a public
    // helper never panics on a tree the engine did not stamp.
    let text = plan_text_lines(
        &relate::value_to_cypher(&tree.to_value()),
        tree.statement_echo(),
    )
    .join("\n");
    tree.to_neo4j_value(&text)
}

/// The plan's text lines (ADR-0359): the `Query:` echo header and the
/// boxed operator table, rendered through this module's one layout — the
/// boundary's rendering, called by the CLI, Bolt's `string-representation`
/// and the HTTP plan text. `tree` is the engine's own wire form
/// ([`relate::PlanNode::to_value`] converted), `echo` the
/// engine-stamped statement echo (ADR-0359).
pub fn plan_text_lines(tree: &CypherValue, echo: &str) -> Vec<String> {
    let header = plan_header_lines(echo);
    crate::rendered_lines(|out| format_plan(out, tree, &header, None))
}

/// The one composed writer behind the table renderer's plan arm: the plan
/// table, then the filtered `Notifications:` block, both read from the one
/// [`PlanView`] bundle.
pub(crate) fn write_plan_presentation(
    w: &mut dyn Write,
    view: &PlanView<'_>,
    width: Option<usize>,
) -> std::io::Result<()> {
    format_plan(w, view.tree, view.header, width)?;
    format_notifications(w, view.notifications)
}

/// The plan table's header lines: the `Query:` echo and the `Plan:` marker
/// — the same header [`plan_text_lines`] heads its table with. `echo` is
/// the engine-stamped statement echo (ADR-0359: the parse classified the
/// preamble, the engine stripped it once at the producer — the boundary
/// never re-scans for the keyword); whitespace collapses here so a
/// multi-line statement echoes as one line.
pub fn plan_header_lines(echo: &str) -> Vec<String> {
    let collapsed = echo.split_whitespace().collect::<Vec<_>>().join(" ");
    vec![format!("Query: {collapsed}"), "Plan:".to_string()]
}
