//! The PROFILE metric args' wire emission (ADR-0344 phase 2 + the metric
//! fields): the width policy every integer goes through ([`put_metric`],
//! [`put_group`]) and one emission helper per riding metric group —
//! read kinds and phase times, the walk start, the kernel rung, the batch
//! chain, the frontier family, the expression memo and the `IN
//! TRANSACTIONS` batch facts — plus the estimate's pair of keys
//! ([`put_estimate`], ADR-0708), which rides every node, not only PROFILE.
//!
//! Split out of `serialize.rs`, which keeps the node/args assembly
//! (`to_value`, `neo4j_args`, `base_args`, `node_map`) and the plan-wire
//! depth/budget guard: a new metric feature extends this file, and the
//! assembly stays put.

use std::collections::BTreeMap;

use relate_model::wire;
use relate_model::wire::plan_keys;

use super::{ClauseMetrics, PlanEstimate};
use crate::Value;

/// One metric arg, through the wire's one width policy (`metric_i64`):
/// saturate at the i64 boundary, never an unchecked cast — the insert
/// idiom every metric field shares. The widening into the `u128` this
/// takes is one spelling per source type: `u128::from` on `u64`
/// operands; `usize` has no `From` impl here (its width is
/// platform-dependent), so its sites spell the lossless `as u128`.
pub(super) fn put_metric(args: &mut BTreeMap<String, Value>, key: &str, value: u128) {
    args.insert(key.to_string(), Value::Int(wire::metric_i64(value)));
}

/// The presence-only form of [`put_metric`]: an optional plan integer is
/// emitted only when it is present, so the key's absence is the whole
/// "the engine did not measure this" fact — never a fabricated zero.
pub(super) fn put_optional_metric(
    args: &mut BTreeMap<String, Value>,
    key: &str,
    value: Option<u128>,
) {
    if let Some(value) = value {
        put_metric(args, key, value);
    }
}

/// The estimate's emission (ADR-0708): `EstimatedRows` and
/// `EstimateProvenance` from the one fact, both or neither.
pub(super) fn put_estimate(args: &mut BTreeMap<String, Value>, estimate: Option<PlanEstimate>) {
    if let Some(estimate) = estimate {
        put_metric(args, plan_keys::ESTIMATED_ROWS, u128::from(estimate.rows));
        args.insert(
            plan_keys::ESTIMATE_PROVENANCE.to_string(),
            Value::String(estimate.provenance.as_str().into()),
        );
    }
}

/// The one integer metric group emitter: every group states its wire key
/// beside the count it feeds, as pairs, never two positionally-zipped
/// lists whose correspondence is a separate invariant. A new group
/// supplies its own `[(key, count); N]` table and calls this.
fn put_group(args: &mut BTreeMap<String, Value>, group: &[(&str, u64)]) {
    for &(key, count) in group {
        put_metric(args, key, u128::from(count));
    }
}

/// The ordered twin of [`put_group`], for a group whose membership and
/// order come from one shared key table (`plan_keys::*_GROUP_KEYS`): the
/// counts are supplied in that same order, and the two same-length arrays
/// cannot drift or truncate. A group with no shared table states its keys
/// and counts as pairs through [`put_group`] instead.
fn put_group_values<const N: usize>(
    args: &mut BTreeMap<String, Value>,
    keys: [&'static str; N],
    counts: [u64; N],
) {
    for (key, count) in keys.into_iter().zip(counts) {
        put_metric(args, key, u128::from(count));
    }
}

/// The PROFILE metrics' arg emission (ADR-0344 phase 2 + metric
/// fields, LABS-369's kinds and start, LABS-448's phase times): the
/// scalar block here — the metric block's own five keys, one width
/// policy for every integer (wire's `metric_i64`: saturate, never an
/// unchecked cast on one field and a clamp on the other) — and one
/// emission helper per riding group beside this fn, so base_args
/// reads as base fields + row estimate + peak workspace + metrics.
pub(super) fn put_metrics(args: &mut BTreeMap<String, Value>, metrics: &ClauseMetrics) {
    put_metric(args, plan_keys::ROWS, u128::from(metrics.rows));
    put_metric(args, plan_keys::TIME_NS, metrics.time_ns);
    put_metric(args, plan_keys::DB_HITS, u128::from(metrics.db_hits));
    // ADR-0463: the expression allocation charged by the operator's
    // clause, beside the row-materialization bytes.
    put_metric(
        args,
        plan_keys::MEMORY_BYTES,
        u128::from(metrics.memory_bytes),
    );
    put_metric(
        args,
        plan_keys::EXPRESSION_BUILT_BYTES,
        u128::from(metrics.expression_built_bytes),
    );
    put_read_kinds_and_phases(args, metrics);
    put_start(args, metrics.start);
    put_kernel(args, metrics.kernel);
    put_batch_chain(args, metrics);
    put_frontier(args, metrics);
    put_memo(args, metrics);
    put_intx(args, &metrics.intx);
}

/// LABS-369: the read-kind mix behind the one `DbHits` total — the
/// facts an opaque `Expand` hid — and LABS-448's phase times, the
/// store work each kind spent. Emitted together with the metric
/// block, so a PROFILE tree carries them or none.
fn put_read_kinds_and_phases(args: &mut BTreeMap<String, Value>, metrics: &ClauseMetrics) {
    put_group(
        args,
        &[
            (plan_keys::HOPS, metrics.reads.hops),
            (plan_keys::NEIGHBOR_WALKS, metrics.reads.neighbor_walks),
            (plan_keys::NODE_READS, metrics.reads.node_reads),
        ],
    );
    put_metric(args, plan_keys::SEEK_TIME_NS, metrics.phases.seek_ns);
    put_metric(args, plan_keys::WALK_TIME_NS, metrics.phases.walk_ns);
}

/// LABS-369: the walk's chosen start (element, direction, reason) —
/// emitted only when the clause walked, a presence-only trio.
fn put_start(args: &mut BTreeMap<String, Value>, start: Option<relate_executor::WalkStart>) {
    if let Some(start) = start {
        put_metric(args, plan_keys::START_ELEMENT, start.element as u128);
        args.insert(
            plan_keys::START_BACKWARD.to_string(),
            Value::Bool(start.backward),
        );
        args.insert(
            plan_keys::START_REASON.to_string(),
            Value::String(start.reason.as_str().into()),
        );
    }
}

/// LABS-1117: which rung served the clause's MATCH walk — the chain,
/// the hub-star, the frontier-NFA rung, or the per-row walker.
/// Emitted only when a rung served the clause, with the metric set;
/// the chain's hop count rides beside it.
fn put_kernel(args: &mut BTreeMap<String, Value>, kernel: Option<relate_executor::KernelRung>) {
    if let Some(kernel) = kernel {
        args.insert(
            plan_keys::KERNEL_RUNG.to_string(),
            Value::String(kernel.as_str().into()),
        );
        if let relate_executor::KernelRung::Chain { hops } = kernel {
            put_metric(args, plan_keys::KERNEL_HOPS, hops as u128);
        }
    }
}

/// ADR-0468 (LABS-359): the batch scan/filter/project chain ran this
/// clause — emitted only when true, never a `false` beside the
/// absent fact — and LABS-645's zone skips, only when nonzero (a
/// count's presence-only shape).
fn put_batch_chain(args: &mut BTreeMap<String, Value>, metrics: &ClauseMetrics) {
    if metrics.batched {
        args.insert(plan_keys::BATCHED.to_string(), Value::Bool(true));
    }
    if metrics.zone_skips > 0 {
        put_metric(args, plan_keys::ZONE_SKIPS, u128::from(metrics.zone_skips));
    }
}

/// ADR-0478: the frontier family's run counters — the operator
/// marker (the plan table's `[frontier …]`) and its own set —
/// emitted only when the family ran the clause (`levels > 0`: a run
/// always completes its level 0, a declined clause is all zeros),
/// the integers together like the start trio. The NFA rung's own
/// facts ride the same presence (ADR-0516): the cells, the rule
/// memo's hits and misses, the admission asks by verdict, and the
/// budget fraction — the one float metric, the counter's own
/// derivation, never a re-spelled division. ADR-0516's run-time
/// decline — the typed reason's stable wire name — emits only when
/// the rung fell through, the start trio's presence-only shape.
fn put_frontier(args: &mut BTreeMap<String, Value>, metrics: &ClauseMetrics) {
    if metrics.frontier.levels > 0 {
        put_group_values(
            args,
            plan_keys::FRONTIER_GROUP_KEYS,
            metrics.frontier.counters(),
        );
        args.insert(
            plan_keys::FRONTIER_BUDGET_FRACTION.to_string(),
            Value::Float(metrics.frontier.budget_fraction()),
        );
    }
    if let Some(reason) = metrics.frontier_decline {
        args.insert(
            plan_keys::FRONTIER_DECLINE_REASON.to_string(),
            Value::String(reason.as_str().into()),
        );
    }
}

/// ADR-0654: the expression-memo group — emitted only when a memo
/// was probed on the operator (the `frontier` group's presence
/// rule), the six integers together; the root's memo types ride
/// beside it. `MemoBytes` is its own field, never `MemoryBytes`.
fn put_memo(args: &mut BTreeMap<String, Value>, metrics: &ClauseMetrics) {
    if metrics.memo.probes > 0 {
        put_group_values(args, plan_keys::MEMO_GROUP_KEYS, metrics.memo.counters());
        if !metrics.memo_kinds.is_empty() {
            args.insert(
                plan_keys::MEMO_KINDS.to_string(),
                Value::List(
                    metrics
                        .memo_kinds
                        .names()
                        .map(|name| Value::String(name.into()))
                        .collect::<Vec<_>>()
                        .into(),
                ),
            );
        }
    }
}

/// REL8-2047: the `IN TRANSACTIONS` batch facts — emitted only when
/// the arm ran batches (`batches > 0`, every other clause's zeroed
/// facts), the seven integers together like the memo group.
fn put_intx(args: &mut BTreeMap<String, Value>, intx: &relate_executor::InTxBatches) {
    if intx.ran() {
        put_group_values(args, plan_keys::IN_TX_GROUP_KEYS, intx.counters());
    }
}
