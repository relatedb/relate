//! The response-envelope family: the canonical success envelope (with the
//! ADR-0491 phase-timing split), the error envelope, and the timing stamp
//! that bundles the total with its split.
//!
//! Lives in its own module beside the other wire families (the plan wire
//! form, the notification wire form), re-exported at the `wire` root so
//! `wire::response_envelope` stays the one public path.

use serde_json::{Map, Value};

use super::{json_f64, notification_keys, notifications_to_wire_json, Notification};

/// The statement's timing block as one wire value — the wire-side spelling
/// of the engine's result-header timing stamp: the ADR-0221 wall-clock
/// total (`elapsed_ms`) together with its always-on PARSE / PLAN / EXECUTE
/// split (ADR-0491) and the fine sub-split plus the plan-cache outcome
/// (LABS-1120), flattened to the envelope keys' own milliseconds. Bundled
/// like the engine bundles its stamp, so an envelope construction site
/// cannot carry the total without its split — and the canonical builder
/// stays under `clippy::too_many_arguments`.
///
/// PARSE is the front-end parse and lowering; PLAN is binding, logical
/// planning and the admission classification; EXECUTE is the dispatch —
/// or, for `EXPLAIN`, the plan render, that path's execute-equivalent
/// stage. The three are disjoint sub-windows of the window `elapsed_ms`
/// spans, so their sum never exceeds the total (the inter-stage gaps stay
/// in the total alone); milliseconds because this is the `elapsed_ms`
/// family it extends (Bolt's seconds rendering is that surface's own
/// conversion, per ADR-0491 §3). The LABS-1120 fine sub-windows partition
/// their coarse phase the same way; `plan_cache` explains a hit's
/// near-zero parse window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimingStampMs {
    /// The ADR-0221 wall-clock total, in milliseconds.
    pub elapsed_ms: f64,
    /// The PARSE stage, in milliseconds.
    pub parse_ms: f64,
    /// The PLAN stage, in milliseconds.
    pub plan_ms: f64,
    /// The EXECUTE stage, in milliseconds.
    pub execute_ms: f64,
    /// The PARSE stage's lexing-plus-AST-build sub-window, in milliseconds
    /// (LABS-1120) — zero on a plan-cache hit, which `plan_cache` explains.
    pub lex_parse_ms: f64,
    /// The PARSE stage's rewrite sub-window (the CIP-231 resolver), in
    /// milliseconds (LABS-1120) — zero on a plan-cache hit.
    pub rewrite_ms: f64,
    /// The PARSE stage's validation-and-lowering sub-window, in
    /// milliseconds (LABS-1120) — zero on a plan-cache hit.
    pub lower_ms: f64,
    /// The EXECUTE stage's result-generation sub-window, in milliseconds
    /// (LABS-1120) — zero on the `EXPLAIN` path, whose render is itself
    /// its result generation.
    pub result_generation_ms: f64,
    /// The statement's plan-cache outcome (LABS-1120) as the engine
    /// enum's own wire spelling (`PlanCacheOutcome::as_str`, kebab-case)
    /// — never re-spelled here: the engine crate pins the vocabulary
    /// against its enum with a test, so this field cannot drift from it.
    pub plan_cache: &'static str,
}

impl TimingStampMs {
    /// The nested `timings` object (LABS-1120): every duration key of the
    /// stamp under one object — the form the surfaces that had no flat
    /// `elapsed_ms` family before (the CLI machine modes, the HTTP Query
    /// API v2 envelope) carry, so those envelopes stay forward-compatible
    /// (a later phase joins the object, never the top level). The
    /// plan-cache outcome rides **beside** the object, never inside it —
    /// it is an outcome, not a duration. One builder for every surface, so
    /// the key set cannot drift between them.
    pub fn to_timings_json(&self) -> Value {
        let mut object = Map::new();
        insert_duration_keys(&mut object, self);
        Value::Object(object)
    }
}

/// The stamp's eight duration keys, each through the one f64 conversion —
/// THE table, consulted by both wire forms (the nested `timings` object
/// and the canonical envelope's flat family), so a key can never be added
/// to one form and not the other (found in review).
fn insert_duration_keys(object: &mut Map<String, Value>, stamp: &TimingStampMs) {
    object.insert("elapsed_ms".into(), json_f64(stamp.elapsed_ms));
    object.insert("parse_ms".into(), json_f64(stamp.parse_ms));
    object.insert("plan_ms".into(), json_f64(stamp.plan_ms));
    object.insert("execute_ms".into(), json_f64(stamp.execute_ms));
    object.insert("lex_parse_ms".into(), json_f64(stamp.lex_parse_ms));
    object.insert("rewrite_ms".into(), json_f64(stamp.rewrite_ms));
    object.insert("lower_ms".into(), json_f64(stamp.lower_ms));
    object.insert(
        "result_generation_ms".into(),
        json_f64(stamp.result_generation_ms),
    );
}

/// Build the canonical success response envelope (ADR-0491): the one
/// construction every JSON surface renders — the pre-0491 envelope plus
/// the statement's per-phase timing split beside `elapsed_ms`.
///
/// `{ "columns": [...], "columnTypes": [...], "rows": [...], "stats": {...},
/// "notifications": [...], "elapsed_ms": <f64>, "parse_ms": <f64>,
/// "plan_ms": <f64>, "execute_ms": <f64>, "lex_parse_ms": <f64>,
/// "rewrite_ms": <f64>, "lower_ms": <f64>, "result_generation_ms": <f64>,
/// "plan_cache": "<outcome>", "plan": {...} | null, "error": null }`.
/// The `plan` key (LABS-418, ADR-0381) is always present too: the
/// `relate.plan/v1` document, `null` on a plain run.
/// `elapsed_ms` is the engine-reported statement elapsed in wall-clock
/// milliseconds (ADR-0221); `parse_ms`/`plan_ms`/`execute_ms` are its
/// PARSE / PLAN / EXECUTE split (ADR-0491, see [`TimingStampMs`]) — always
/// populated on this path, never null, riding the one `timings` parameter
/// so a construction site cannot carry the total without its split.
/// `columnTypes` carries the engine-stamped
/// per-column type names (ADR-0224), beside `columns`, same length and order.
/// `notifications` is always present — empty on a plain run, populated by
/// `EXPLAIN`/`PROFILE` (ADR-0363 decision 9) — and its entries are the
/// engine-native objects `super::notification_to_wire_json` emits.
pub fn response_envelope(
    columns: &[String],
    column_types: &[String],
    rows: Vec<Value>,
    stats: Value,
    notifications: &[Notification],
    timings: TimingStampMs,
    plan: Option<Value>,
) -> Value {
    let mut obj = Map::new();
    obj.insert(
        "columns".into(),
        Value::Array(columns.iter().map(|c| Value::String(c.clone())).collect()),
    );
    // ADR-0224: the engine-stamped per-column type names, beside `columns`.
    obj.insert(
        "columnTypes".into(),
        Value::Array(
            column_types
                .iter()
                .map(|t| Value::String(t.clone()))
                .collect(),
        ),
    );
    obj.insert("rows".into(), Value::Array(rows));
    obj.insert("stats".into(), stats);
    // ADR-0363 decision 9: always present, empty on a plain run.
    obj.insert(
        notification_keys::NOTIFICATIONS.into(),
        notifications_to_wire_json(notifications),
    );
    // ADR-0221: the engine-reported statement elapsed, in milliseconds;
    // ADR-0491: its PARSE / PLAN / EXECUTE split beside it; LABS-1120: the
    // fine sub-split and the plan-cache outcome — the duration keys from
    // the one table both wire forms share, the outcome beside them.
    insert_duration_keys(&mut obj, &timings);
    obj.insert(
        "plan_cache".into(),
        Value::String(timings.plan_cache.into()),
    );
    obj.insert("error".into(), Value::Null);
    // The structured plan document (LABS-418, ADR-0381): always present,
    // `null` when the statement carried no plan — the same presence policy
    // `notifications` (ADR-0363) and `columnTypes` (ADR-0224) use, owned by
    // the canonical builder. The HTTP surface's Neo4j-shaped envelope has
    // its own builder and its own plan keys.
    obj.insert("plan".into(), plan.unwrap_or(Value::Null));
    Value::Object(obj)
}

/// Build the canonical error response envelope: empty `columns`/`rows`, empty
/// `stats`, and the error message string under `error`.
pub fn error_envelope(message: &str) -> Value {
    let mut obj = Map::new();
    obj.insert("columns".into(), Value::Array(Vec::new()));
    obj.insert("rows".into(), Value::Array(Vec::new()));
    obj.insert("stats".into(), Value::Object(Map::new()));
    obj.insert("error".into(), Value::String(message.to_string()));
    Value::Object(obj)
}
