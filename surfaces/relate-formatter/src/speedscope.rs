//! The speedscope renderer: a `PROFILE` plan tree exported as one
//! speedscope evented-profile JSON document, loadable at
//! <https://www.speedscope.app> (upstream file-format spec: the
//! `$schema` URL below, `shared.frames`, one `evented` profile with
//! `startValue`/`endValue` and `O`/`C` events whose `at` values are
//! non-decreasing and balanced).

use std::io::{self, Write};

use indexmap::IndexMap;
use relate_model::wire::{plan_node_wire, PlanNodeWire};
use relate_model::CypherValue;
use serde_json::{json, Value};

use crate::{Formatter, StatementMeta};

/// The speedscope file-format schema URL, the one speedscope.app itself
/// stamps on its own exports — single-sourced here so the document and
/// the tests share it.
const SCHEMA_URL: &str = "https://www.speedscope.app/file-format-schema.json";

/// The profile name every speedscope export carries.
const PROFILE_NAME: &str = "relate PROFILE";

/// The `exporter` field: the producing program, speedscope's recommended
/// `name@version` shape.
const EXPORTER: &str = concat!("relate@", env!("CARGO_PKG_VERSION"));

impl Formatter {
    /// Speedscope evented-profile rendering: one document with one frame
    /// per plan operator (`"{operator} {id}"`) and one `O` + one `C` event
    /// per operator, `at` values in nanoseconds.
    ///
    /// The plan tree carries per-operator inclusive times only — no
    /// offsets — so the event timeline is an approximation, not a
    /// recording: each parent's interval is its own inclusive time, and
    /// its children are laid out sequentially within it. When the
    /// children's inclusive times sum past the parent's (whole-arm
    /// measurement can double-count shared work), the children's
    /// intervals are scaled down proportionally to fit, so every parent's
    /// interval contains its children's and the event order stays
    /// balanced — the property speedscope's importer requires.
    ///
    /// The mode requires a `PROFILE` tree: every operator must carry the
    /// metric `args` (`Rows`/`TimeNs`/`DbHits`/`MemoryBytes`). A plain
    /// query or an `EXPLAIN` tree (no metrics) is an error — there are no
    /// per-operator times to export, and an empty profile would misrepresent
    /// the run.
    pub(crate) fn format_speedscope(
        &self,
        w: &mut dyn Write,
        _columns: &[String],
        _rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> io::Result<()> {
        let plan = meta
            .plan
            .map(|view| view.tree)
            .ok_or_else(|| no_profile_times("the statement carries no plan tree"))?;
        let doc = speedscope_document(plan)?;
        // Compact one-line JSON, like `jsonl`/`envelope`: the document is
        // machine-consumed (imported into speedscope), not read at a
        // terminal.
        let mut line = serde_json::to_string(&doc).map_err(io::Error::other)?;
        line.push('\n');
        w.write_all(line.as_bytes())
    }
}

/// The one "no PROFILE times" error — a single message policy for both
/// shapes (no plan tree at all, and a tree whose operators carry no
/// metrics), so the two arms cannot drift apart.
fn no_profile_times(why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "speedscope mode requires a PROFILE statement: per-operator \
             times are what it exports ({why})"
        ),
    )
}

/// Build the full speedscope file-format document from a PROFILE plan
/// tree: `shared.frames` (one per operator, DFS order) and one `evented`
/// profile whose `events` open and close every frame.
fn speedscope_document(plan: &CypherValue) -> io::Result<Value> {
    let root = plan_node_wire(plan);
    let root_time = root
        .metrics
        .ok_or_else(|| no_profile_times("the plan tree carries no PROFILE metrics"))?
        .time_ns;
    let mut frames: Vec<String> = Vec::new();
    let mut events: Vec<Value> = Vec::new();
    walk_events(&root, 0, root_time, &mut frames, &mut events)?;
    Ok(json!({
        "$schema": SCHEMA_URL,
        "shared": { "frames": frames.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>() },
        "profiles": [{
            "type": "evented",
            "name": PROFILE_NAME,
            "unit": "nanoseconds",
            "startValue": 0,
            "endValue": root_time,
            "events": events,
        }],
        "exporter": EXPORTER,
    }))
}

/// Append the `O`/`C` events of one operator (and, recursively, its
/// children) to `events`, registering the operator's frame in `frames`.
/// The operator's interval is `[start, start + duration]`; children are
/// laid out sequentially inside it (scaled down to fit when their
/// inclusive times sum past it — see [`Formatter::format_speedscope`]).
/// The node arrives already parsed, so one `plan_node_wire` call per
/// operator feeds the fused skip, the time read and the recursion.
fn walk_events(
    node: &PlanNodeWire<'_>,
    start: i64,
    duration: i64,
    frames: &mut Vec<String>,
    events: &mut Vec<Value>,
) -> io::Result<()> {
    let frame = frames.len();
    frames.push(format!("{} {}", node.operator_type, node.id));
    events.push(json!({ "type": "O", "frame": frame, "at": start }));
    // The executed children with their own inclusive times — each must
    // carry PROFILE metrics too; a half-metriced tree is a wire-contract
    // break, kept loud. A fused virtual child is a display-only breakdown
    // step with no metrics of its own, so it is skipped: the timeline is
    // the executed operators, whose totals it would otherwise be counted
    // against. The pair keeps the child and its time together, so the
    // layout can never read them out of step.
    let mut executed: Vec<(PlanNodeWire<'_>, i64)> = Vec::new();
    for child in node.children {
        let child_node = plan_node_wire(child);
        if child_node.fused.is_none() {
            let time = child_node
                .metrics
                .ok_or_else(|| no_profile_times("an operator carries no PROFILE metrics"))?
                .time_ns;
            executed.push((child_node, time));
        }
    }
    let child_times: Vec<i64> = executed.iter().map(|(_, time)| *time).collect();
    for ((child, _), (child_start, child_span)) in
        executed
            .iter()
            .zip(laid_out_children(&child_times, start, duration))
    {
        walk_events(child, child_start, child_span, frames, events)?;
    }
    events.push(json!({ "type": "C", "frame": frame, "at": start + duration }));
    Ok(())
}

/// The children's `(start, duration)` intervals inside a parent's
/// `[start, start + duration]`: sequential, each child keeping its own
/// inclusive time, except when the children's times sum past the parent's
/// — then each child gets `child_time * duration / total` (the last child
/// the remainder, so the intervals always tile the parent's exactly and
/// the event order stays balanced).
fn laid_out_children(child_times: &[i64], start: i64, duration: i64) -> Vec<(i64, i64)> {
    let total: i64 = child_times.iter().sum();
    let mut laid: Vec<(i64, i64)> = Vec::with_capacity(child_times.len());
    let mut cursor = start;
    for (i, &child_time) in child_times.iter().enumerate() {
        let last = i + 1 == child_times.len();
        let span = if total <= duration {
            child_time
        } else if last {
            // The remainder of the parent's interval, whatever the raw
            // proportion rounds to — the tiling rule, not a second policy.
            duration.saturating_sub(cursor - start)
        } else {
            child_time.saturating_mul(duration) / total
        };
        laid.push((cursor, span));
        cursor += span;
    }
    laid
}
