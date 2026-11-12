//! The JSON family: `json` (pretty or compact, ADR-0240), `jsonl`, and the
//! `envelope` mode (ADR-0195/ADR-0224), all over the one canonical row
//! projection ([`wire::rows_for_result`])
//! projection.

use std::io::{self, Write};

use indexmap::IndexMap;
use relate_model::wire;
use relate_model::CypherValue;

use crate::{Formatter, RowCap, StatementMeta};

impl Formatter {
    pub(crate) fn format_json(
        &self,
        w: &mut dyn Write,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> std::io::Result<()> {
        // A PROFILE statement's machine-native output carries BOTH its rows
        // and the structured plan (LABS-373): `{"data": [...], "plan": {…}}`.
        // An EXPLAIN statement has no rows (ADR-0359), so its output is the
        // plan object alone — not the rendered text lines. The complete
        // notification list rides the plan object (ADR-0363 decision 9).
        // The value is chosen first so the ADR-0240 pretty/compact policy
        // (indented for humans, one compact line otherwise) exists once.
        // Every other statement renders the rows — the one projection —
        // beside whatever rides with them, the riders accumulated first:
        // the plan's keys (the one helper, so the PROFILE arm cannot
        // drift from the EXPLAIN/jsonl shape) and the cut's counts
        // in-band (ADR-0403). Rows riding alone keep the bare array this
        // mode has always been; any rider wraps — `{"data": [...], …}`,
        // the PROFILE wrapper's shape and gram.rs's bare-vs-wrapped
        // decision — so a capped result's type change is itself the loud
        // signal that the result is not complete.
        let data = serde_json::Value::Array(wire::rows_for_result(columns, rows));
        let mut riders = serde_json::Map::new();
        if let Some(view) = meta.plan {
            riders.extend(plan_json(view.wire, view.notifications));
        }
        if let Some(cap) = meta.row_cap {
            riders.insert(RowCap::WIRE_KEY.to_string(), cap.to_wire_json(rows.len()));
        }
        // LABS-1120: the timing rider — present whenever the caller
        // passed a result behind the render, so the machine modes'
        // consumers read where the time went without the human `Time:`
        // line. The members come from the one helper every JSON-family
        // mode shares.
        if let Some(timings) = meta.timings {
            for (key, value) in timing_members(&timings) {
                riders.insert(key.to_string(), value);
            }
        }
        let output = match meta.plan {
            // The EXPLAIN arm — read off the view's typed classification,
            // never re-derived from the result's shape — renders the
            // accumulated riders (exactly the plan keys, since the cap
            // cuts rows only and an EXPLAIN result has none, the
            // invariant `format_sinks` asserts), never a fresh object
            // that would silently drop a rider.
            Some(view) if !view.profiled => serde_json::Value::Object(riders),
            _ if riders.is_empty() => data,
            _ => {
                riders.insert("data".to_string(), data);
                serde_json::Value::Object(riders)
            }
        };
        let json = if self.pretty {
            serde_json::to_string_pretty(&output).map_err(io::Error::other)?
        } else {
            serde_json::to_string(&output).map_err(io::Error::other)?
        };
        writeln!(w, "{json}")?;
        Ok(())
    }

    pub(crate) fn format_jsonl(
        &self,
        w: &mut dyn Write,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> std::io::Result<()> {
        // The rows first, then the plan as a final line (LABS-373): a
        // PROFILE run keeps its data in the JSONL stream.
        for row in wire::rows_for_result(columns, rows) {
            write_json_line(w, &row)?;
        }
        // The plan rides keyed (`{"plan": {…}, "notifications": […]}`, the
        // notification list beside the tree through the same plan-object
        // convention ADR-0363 decision 9 uses in `json`), like json and
        // toon key it: a bare object in the row stream would be
        // indistinguishable from a data row that happens to carry an
        // `operatorType` key.
        if let Some(view) = meta.plan {
            write_json_line(
                w,
                &serde_json::Value::Object(plan_json(view.wire, view.notifications)),
            )?;
        }
        // ADR-0403: the cut's terminal record, keyed the same way — the
        // last line of the statement's stream names both counts, so a
        // capped result never reads as complete.
        if let Some(cap) = meta.row_cap {
            write_json_line(
                w,
                &serde_json::json!({
                    RowCap::WIRE_KEY: cap.to_wire_json(rows.len())
                }),
            )?;
        }
        // LABS-1120: the timing block as the stream's final keyed line —
        // the same members every JSON-family mode carries, keyed so the
        // line can never be confused with a data row. ADR-0581: the line
        // is the run-level opt-in (`Formatter::render_timings`, the
        // `pretty` shape) — on, the library's default, a passed stamp
        // renders; the CLI sets it from `--timings`, off by default, so
        // its stream is pure rows.
        if let Some(timings) = meta.timings.filter(|_| self.render_timings) {
            let mut line = serde_json::Map::new();
            for (key, value) in timing_members(&timings) {
                line.insert(key.to_string(), value);
            }
            write_json_line(w, &serde_json::Value::Object(line))?;
        }
        Ok(())
    }

    pub(crate) fn format_envelope(
        &self,
        w: &mut dyn Write,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> std::io::Result<()> {
        // ADR-0224: the engine stamps one type name per column, same length
        // and order as `columns` — surfaced here beside `columns`, the same
        // key the wire envelope uses. The stamp is trusted like
        // `wire::response_envelope` trusts it; the invariant is kept loud.
        debug_assert_eq!(
            columns.len(),
            meta.column_types.len(),
            "ADR-0224: one engine-stamped type name per column"
        );
        // The structured plan rides the envelope (ADR-0345) beside the
        // rendered text rows: consumers read the machine-native tree. The
        // envelope is a `Map` by construction — objectness cannot fail and
        // the plan cannot be silently skipped.
        let mut envelope = serde_json::Map::new();
        envelope.insert("columns".to_string(), serde_json::json!(columns));
        envelope.insert(
            "columnTypes".to_string(),
            serde_json::json!(meta.column_types),
        );
        envelope.insert(
            "rows".to_string(),
            serde_json::Value::Array(wire::rows_for_result(columns, rows)),
        );
        envelope.insert(
            "truncated".to_string(),
            serde_json::Value::Bool(meta.row_cap.is_some()),
        );
        // ADR-0403: the cut's counts in-band, one keyed object beside the
        // legacy bool — present only when the rows were capped, the `plan`
        // key's presence policy, so a capped result never reads as
        // complete.
        if let Some(cap) = meta.row_cap {
            envelope.insert(RowCap::WIRE_KEY.to_string(), cap.to_wire_json(rows.len()));
        }
        if let Some(view) = meta.plan {
            envelope.insert("plan".to_string(), wire::value_to_wire_json(view.wire));
        }
        // ADR-0363 decision 9: the complete list, always present — empty on
        // a plain run — under the native keys.
        envelope.insert(
            wire::notification_keys::NOTIFICATIONS.to_string(),
            wire::notifications_to_wire_json(meta.notifications()),
        );
        // LABS-1120: the statement's timing block — the same members
        // every JSON-family mode carries: the machine consumer's answer
        // to "where did the time go, and did the plan cache serve it".
        if let Some(timings) = meta.timings {
            for (key, value) in timing_members(&timings) {
                envelope.insert(key.to_string(), value);
            }
        }
        let json = serde_json::to_string(&serde_json::Value::Object(envelope))
            .map_err(io::Error::other)?;
        writeln!(w, "{json}")?;
        Ok(())
    }
}

/// The LABS-1120 timing rider's two members: one keyed `timings` object
/// (the model's one builder — the same nested form the HTTP v2 envelope
/// carries) with the `planCache` outcome beside it. One spelling for every
/// JSON-family emit site — `json`'s rider, `jsonl`'s terminal line,
/// `envelope`'s keys — so the pair cannot drift between the modes (found
/// in review). Toon renders the same two facts in its own entry form, the
/// per-mode rendering rule `rowCap` also follows.
fn timing_members(
    timings: &relate_model::wire::TimingStampMs,
) -> [(&'static str, serde_json::Value); 2] {
    [
        ("timings", timings.to_timings_json()),
        ("planCache", serde_json::json!(timings.plan_cache)),
    ]
}

/// The machine-native plan object's keys (ADR-0345): the structured tree
/// under the `"plan"` key in wire form, with the complete notification list
/// (ADR-0363 decision 9) under `"notifications"` — the plan convention is
/// already one own object, so the list rides there rather than reshaping
/// the bare row array. One helper feeds every JSON-family arm that carries
/// a plan (EXPLAIN's whole output, PROFILE's `data` object, jsonl's final
/// line), so the keys cannot drift between them.
fn plan_json(
    plan: &CypherValue,
    notifications: &[wire::Notification],
) -> serde_json::Map<String, serde_json::Value> {
    let mut object = serde_json::Map::new();
    object.insert("plan".to_string(), wire::value_to_wire_json(plan));
    object.insert(
        wire::notification_keys::NOTIFICATIONS.to_string(),
        wire::notifications_to_wire_json(notifications),
    );
    object
}

/// One compact JSON value as one output line — the `jsonl` rows', the plan
/// line's and ADR-0403's terminal record's emit pair (serialize, map the
/// serde error, write the line), spelled once.
fn write_json_line(w: &mut dyn Write, value: &serde_json::Value) -> std::io::Result<()> {
    let json = serde_json::to_string(value).map_err(io::Error::other)?;
    writeln!(w, "{json}")
}
