//! LABS-370 (v1): the count-only statistics short-circuit — the shape
//! recognizer that answers a whole-population count from the maintained
//! statistics instead of scanning its population. The pattern population
//! helpers and the count grammar live in [`shortcut_common`](super::shortcut_common);
//! this module owns the count-only recognizer and its answer. Declared
//! beside `count_stats_tests`, its contract tests (ADR-0387).

use relate_core::Result;
use relate_ir::{Clause, Direction, Projection};

use crate::graph::CountProbe;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::label_expr_ok;
use super::project::return_result;
use super::shortcut_common::{
    count_of, count_value, match_return_pattern, node_population, node_vars_unique, Count, Pop,
};
use super::QueryResult;

/// LABS-370 (v1): the count-only shape recognizer (LABS-425, the round-6
/// decomposition of the round-1 F-1-4 finding — the recognizer, the
/// gates, and the answer are three concerns in three places, not one
/// 160-line function). Deliberately conservative — every shape it does
/// not recognize returns `None` and the caller falls back to the clause
/// loop's scan:
/// exactly `[Match, Return]` (the `MATCH` half is
/// [`match_return_pattern`](super::shortcut_common::match_return_pattern), the one
/// plain-pattern gate every short-circuit recognizer starts from); the
/// pattern's nodes each answer
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
    let pattern = match_return_pattern(clauses)?;
    let Clause::Return {
        distinct: false,
        group_by: None,
        items,
        order,
        skip: None,
        limit: None,
    } = &clauses[1]
    else {
        return None;
    };
    if !order.is_empty() || items.len() != 1 {
        return None;
    }
    let item = &items[0];
    // The one count grammar (ADR-0217: the vocabulary's own classifier
    // via `count_of`, never a re-derived name literal): `count(*)`, or
    // `count(var)` — every other expression is the scan's.
    let counted_var = count_of(&item.expr)?;
    // Round 3, F-1-4: repeated node variables decline (the pair count
    // includes distinct-endpoint rels the same-variable shape excludes).
    if !node_vars_unique(pattern) {
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
    if let Count::Of(v) = counted_var {
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

/// LABS-370 (v1): recognize the count-only shape and answer it from the
/// statistics. The answer lives here; the exactness and seed gate is
/// [`stats_view_serves`](super::stats_short_circuit::stats_view_serves),
/// checked once by the short-circuit's owner; the shape decline-ladder
/// lives in [`recognize_count_shape`] (LABS-425, the round-6 decomposition
/// of the round-1 F-1-4 finding). The contract is two states plus the
/// result boundary's error: `Ok(Some)` = answered, `Ok(None)` = decline to
/// the clause loop's scan, `Err` = the shared result boundary failed. The
/// seed rows must be exactly one row with no bindings (round 3, F-1-3:
/// `run_clauses_mode` runs per part with the previous stage's rows — a
/// NEXT/WHEN/ELSE body seeded with N rows must answer N × population, 0
/// when N = 0, which the single-population read cannot express).
pub(super) fn count_only_stats_result(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
) -> Result<Option<(Vec<Row>, QueryResult)>> {
    let Some((probe, item)) = recognize_count_shape(clauses) else {
        return Ok(None);
    };
    // Round 3, F-1-5: the consistency window — the exactness check and
    // the count read under BOTH count-map locks (the pre-check above
    // only filters; a writer that opens between it and here is closed by
    // the under-lock re-check). The None≡0-by-exactness boundary lives
    // inside the stats family (a drained entry leaves the map).
    let Some(count) = graph
        .base()
        .windowed_stats_count(|| graph.sees_exact_base_stats(), probe)
    else {
        return Ok(None);
    };
    // Round 3, F-1-18: the value is constructed once.
    let value = count_value(count);
    // The same result boundary the ladder's other two rungs finish
    // through — columns, the strict cell move, the view's own stats.
    let mut rows = vec![Row::from([(item.output_name(0), value)])];
    let result = return_result(graph, std::slice::from_ref(item), clauses, &mut rows)?;
    Ok(Some((rows, result)))
}
