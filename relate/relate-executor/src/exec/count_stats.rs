//! LABS-370 (v1): the count-only statistics short-circuit — the shape
//! recognizer that answers a whole-population count from the maintained
//! statistics instead of scanning its population. Declared beside
//! `count_stats_tests`, its contract tests (ADR-0387).

use std::collections::HashSet;

use relate_core::Value;
use relate_ir::{
    classify_aggregate, AggregateKind, Clause, Direction, Expr, MatchMode, PathPattern, Projection,
};
use relate_names::SmallName;

use crate::graph::{CountProbe, QueryStats};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::{label_expr_ok, QueryResult};

/// The population a pattern node's own shape can answer a whole-count
/// for — the one classification both node arms share (LABS-851's
/// structure round: one enumeration of the predicate-bearing
/// `NodePattern` fields, not two that must stay in sync — round 3,
/// F-1-3's enumeration lives here once, so a new predicate-bearing field
/// lands here and both arms follow):
///
/// * [`Pop::Label`] — exactly one label, no properties, no inline WHERE,
///   and a label expression the counts can mirror (a statically
///   mirroring expression — the raw front-lowered shape — is accepted,
///   round 4, F-4-1; a multi-label node needs label-set intersections
///   the per-label counts cannot answer).
/// * [`Pop::Any`] — no label and no label expression at all (a label
///   expression on an unlabeled node is dynamic or degenerate; the
///   whole-population count cannot mirror it) — the whole node
///   population, the label-less nodes included.
/// * [`Pop::No`] — anything else: the scan's filters own the answer.
enum Pop<'a> {
    Label(&'a SmallName),
    Any,
    No,
}

fn node_population(np: &relate_ir::NodePattern) -> Pop<'_> {
    // The node-level predicate fields the counts never check, one
    // spelling: no properties, no inline WHERE.
    let plain = np.properties.is_none() && np.where_clause.is_none();
    if np.labels.len() == 1 && plain && label_expr_ok(&np.label_expr, &np.labels) {
        Pop::Label(&np.labels[0].0)
    } else if np.labels.is_empty() && np.label_expr.is_none() && plain {
        Pop::Any
    } else {
        Pop::No
    }
}

/// The one pattern of a plain whole-population `MATCH` — the recognizer
/// ladder's first rung, shared by the count-only shapes and the label-set
/// group shapes (ADR-0589): non-optional, the default match mode, exactly
/// one pattern, no clause WHERE, no search, and no path-level predicate (a
/// path WHERE, a mode (WALK/TRAIL/ACYCLIC/SIMPLE) or a selector
/// (ANY/SHORTEST/...) changes which rows the scan emits; round 3, F-1-3:
/// every predicate-bearing field on the IR's pattern types is enumerated,
/// none silently accepted).
pub(super) fn plain_match_pattern(clause: &Clause) -> Option<&PathPattern> {
    let Clause::Match {
        optional: false,
        match_mode: MatchMode::DifferentRelationships,
        patterns,
        where_clause: None,
        search: None,
        using_index: _,
        using_join: _,
    } = clause
    else {
        return None;
    };
    let [pattern] = &patterns[..] else {
        return None;
    };
    if pattern.where_clause.is_some() || pattern.mode.is_some() || pattern.selector.is_some() {
        return None;
    }
    Some(pattern)
}

/// The variable of a one-node, relationship-free pattern whose node is
/// the whole node population ([`Pop::Any`]: no label, no property, no
/// inline WHERE). `None` for an anonymous node and every other shape.
pub(super) fn whole_population_node_var(pattern: &PathPattern) -> Option<&SmallName> {
    let ([node], true) = (&pattern.nodes[..], pattern.rels.is_empty()) else {
        return None;
    };
    match node_population(node) {
        Pop::Any => node.variable.as_ref(),
        Pop::Label(_) | Pop::No => None,
    }
}

/// LABS-370 (v1): the count-only shape recognizer (LABS-425, the round-6
/// decomposition of the round-1 F-1-4 finding — the recognizer, the
/// gates, and the answer are three concerns in three places, not one
/// 160-line function). Deliberately conservative — every shape it does
/// not recognize returns `None` and the caller falls back to the clause
/// loop's scan:
/// exactly `[Match, Return]`; the MATCH is non-optional, the default
/// match mode, one pattern, no WHERE, no search; the pattern carries no
/// path-level predicate either (no path WHERE, mode or selector — round
/// 3, F-1-3: every predicate-bearing field on the IR's pattern types is
/// enumerated, none silently accepted); the pattern's nodes each answer
/// one population ([`node_population`]): exactly one plain label, or —
/// LABS-851 — no label at all (the whole-population arms: the total
/// node count, the per-type and total rel counts); its rel — when present — is
/// single-hop, single-type or untyped, property-free, predicate-free and
/// directed (`Both` counts both orientations, which the one-directional
/// counts cannot answer); the pattern's node variables do not repeat
/// (round 3, F-1-4: a same-variable self-loop shape would answer from
/// the pair count, which includes distinct-endpoint rels the scan
/// excludes); the RETURN projects exactly one `count(*)` or `count(var)`
/// with no DISTINCT, grouping, ORDER BY, SKIP or LIMIT. `count(var)`
/// equals `count(*)` when the pattern binds the variable on every row
/// it emits — the binding check is recognition, so it lives here.
fn recognize_count_shape(clauses: &[Clause]) -> Option<(CountProbe, &Projection)> {
    if clauses.len() != 2 {
        return None;
    }
    let (
        Some(pattern),
        Clause::Return {
            distinct: false,
            group_by: None,
            items,
            order,
            skip: None,
            limit: None,
        },
    ) = (plain_match_pattern(&clauses[0]), &clauses[1])
    else {
        return None;
    };
    if !order.is_empty() || items.len() != 1 {
        return None;
    }
    let item = &items[0];
    let counted_var: Option<&SmallName> = match &item.expr {
        Expr::CountStar => None,
        // The aggregate vocabulary's own classifier (ADR-0217): match
        // the kind, not a re-derived name literal.
        Expr::Function {
            name,
            distinct: false,
            args,
        } if name.with_str(classify_aggregate) == Some(AggregateKind::Count) && args.len() == 1 => {
            match &args[0] {
                Expr::Variable(v) => Some(v),
                _ => return None,
            }
        }
        _ => return None,
    };
    // Round 3, F-1-4: repeated node variables (a same-variable self-loop
    // shape, `(a:A)-[:R]->(a:A)`) would answer from the (A,R,A) pair
    // count, which includes distinct-endpoint rels the scan's
    // same-variable shape excludes — decline when the variables repeat.
    let mut node_vars: HashSet<&SmallName> = HashSet::new();
    if pattern
        .nodes
        .iter()
        .filter_map(|n| n.variable.as_ref())
        .any(|v| !node_vars.insert(v))
    {
        return None;
    }
    let probe = if pattern.rels.is_empty() {
        // The single-node shape: the whole label population, or —
        // LABS-851 — the whole node population when the node carries no
        // label at all.
        let [node] = &pattern.nodes[..] else {
            return None;
        };
        match node_population(node) {
            // The exact per-label live count — the label index's rows are a
            // plan-time upper bound (deleted ids stay until checkpoint GC),
            // this map follows every mutation seam exactly.
            Pop::Label(label) => Some(CountProbe::Label(label.to_string())),
            // LABS-851: the whole node population, the label-less nodes
            // included (the per-label map never sees them).
            Pop::Any => Some(CountProbe::AllNodes),
            Pop::No => None,
        }
    } else if pattern.rels.len() == 1 && pattern.nodes.len() == 2 {
        let rp = &pattern.rels[0];
        // Round 3, F-1-3: the rel-side predicate fields — a dynamic or
        // non-mirroring type expression and an inline WHERE change the
        // scan's row set (a statically mirroring type expression is the
        // raw front-lowered shape and is accepted — round 4, F-4-2).
        if rp.properties.is_some()
            || !label_expr_ok(&rp.type_expr, &rp.types)
            || rp.where_clause.is_some()
            || rp.var_length
            || rp.quantified
        {
            return None;
        }
        // The one direction fact, stated once for every endpoint arm:
        // the counts hold each rel once (the pair in its own direction,
        // the type and total counts direction-free); an undirected
        // pattern matches each rel twice minus self-loops, which no
        // maintained count expresses.
        if !matches!(rp.direction, Direction::Outgoing | Direction::Incoming) {
            return None;
        }
        match (
            node_population(&pattern.nodes[0]),
            node_population(&pattern.nodes[1]),
            &rp.types[..],
        ) {
            // The exact (start-label, type, end-label) pair count. `In`
            // is the same single rel seen from the other end: the pair
            // the stats hold is (start, type, end) in the rel's own
            // direction, whichever way the pattern spells it.
            (Pop::Label(a), Pop::Label(b), [(rel_type, _)]) => {
                // Text render: the count probe owns its label/type text
                // (String domain, interned on the count-state side).
                let (s, e) = match rp.direction {
                    Direction::Outgoing => (a, b),
                    _ => (b, a),
                };
                Some(CountProbe::Pair(
                    s.to_string(),
                    rel_type.to_string(),
                    e.to_string(),
                ))
            }
            // LABS-851: both endpoints unlabeled — the whole rel
            // population, one type ([`CountProbe::RelType`]) or every
            // type ([`CountProbe::AllRels`]); direction is irrelevant
            // (the same rels, seen from either end).
            (Pop::Any, Pop::Any, [(rel_type, _)]) => {
                Some(CountProbe::RelType(rel_type.to_string()))
            }
            (Pop::Any, Pop::Any, []) => Some(CountProbe::AllRels),
            // One labeled endpoint beside one unlabeled endpoint (the
            // (label, type) side counts are not maintained — the
            // LABS-851 follow-up arm), a type alternation, or a shape
            // the per-label counts cannot mirror — the scan answers.
            _ => None,
        }
    } else {
        None
    };
    let probe = probe?;
    // `count(var)` is answerable only when the pattern binds the
    // variable on every row it emits — recognition, not answer: an
    // unbound variable's count rides the scan's expression semantics.
    if let Some(v) = counted_var {
        let bound = pattern
            .nodes
            .iter()
            .filter_map(|n| n.variable.as_ref())
            .chain(pattern.rels.iter().filter_map(|r| r.variable.as_ref()))
            .any(|name| name == v);
        if !bound {
            return None;
        }
    }
    Some((probe, item))
}

/// The eligibility prologue both statistics short-circuits share.
///
/// The exactness precondition: the base stats are only exact for a view
/// of the latest published state with no overlay — anything else
/// (in-tx scratch, pinned snapshot, in-flight apply) declines to the
/// scan, which reads its own writes (the overlay's predicate, ADR-0387
/// follow-up). A check that passes here can go stale (a writer opens
/// after it) — the windowed reads re-check under the stats lock and
/// close that gap; a check that fails is stable (a finished writer
/// advances `commit_ts`, so the pinned-equality never turns true again
/// for this view).
///
/// Round 3, F-1-3: the short-circuits answer a whole-population
/// statement — one unbound seed row, the statement's opening stage.
/// Anything else (zero rows, N > 1 rows, or a bound row) is a seeded
/// stage whose answer scales with the seeds — decline to the scan.
pub(super) fn stats_view_serves(graph: &ExecGraph<'_>, seed_rows: &[Row]) -> bool {
    graph.sees_exact_base_stats() && seed_rows.len() == 1 && seed_rows[0].is_empty()
}

/// LABS-370 (v1): recognize the count-only shape and answer it from the
/// statistics. The answer lives here; the exactness and seed gate is
/// [`stats_view_serves`], checked once by the short-circuit's owner; the
/// shape decline-ladder lives in [`recognize_count_shape`] (LABS-425,
/// the round-6 decomposition of the round-1 F-1-4 finding). The boundary
/// never fails — `Option` is the whole contract: `Some` = answered,
/// `None` = decline to the clause loop's scan. The seed rows must be
/// exactly one row with no bindings (round 3, F-1-3:
/// `run_clauses_mode` runs per part with the previous stage's rows — a
/// NEXT/WHEN/ELSE body seeded with N rows must answer N × population, 0
/// when N = 0, which the single-population read cannot express).
pub(super) fn count_only_stats_result(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
) -> Option<(Vec<Row>, QueryResult)> {
    let (probe, item) = recognize_count_shape(clauses)?;
    // Round 3, F-1-5: the consistency window — the exactness check and
    // the count read under BOTH count-map locks (the pre-check above
    // only filters; a writer that opens between it and here is closed by
    // the under-lock re-check). The None≡0-by-exactness boundary lives
    // inside the stats family (a drained entry leaves the map).
    let count = graph
        .base()
        .windowed_stats_count(|| graph.sees_exact_base_stats(), probe)?;
    let column = item.output_name(0);
    // Round 3, F-1-18: the value is constructed once, shared by the row
    // and the result.
    let value =
        Value::Int(i64::try_from(count).expect("a node or rel count past i64 is a broken store"));
    let row = Row::from([(column.clone(), value.clone())]);
    Some((
        vec![row],
        QueryResult::intermediate(vec![column], vec![vec![value]], QueryStats::default()),
    ))
}
