//! The chain kernel's count fold (LABS-1293): `MATCH (a …)-[:R]->()-[:R]->()-[:R]->(b …)
//! RETURN count(*)` (or `count(<pattern var>)`) answered by the unrolled
//! chain with the last hop tallied, never landed. Default match mode only;
//! every decline keeps the unchanged plan.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{Clause, Direction, MatchMode, NodePattern, PathPattern, PatternElement};

use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::agg_fold::FoldedReturn;
use super::super::graph_stats::GraphStats;
use super::super::pushdown::{flatten_and_owned, match_scope, Residual};
use super::counted_return;
use super::matcher::{self, Contract};
use super::{chain, chain_stages_fit, kernels_stood_down, record_walk_start, scan_segment};
use crate::ctx::WalkStart;

/// The fold entry: `clauses` starts at the `MATCH`, its `RETURN` follows.
pub(crate) fn try_fold_chain_count(
    graph: &ExecGraph<'_>,
    rows: &[Row],
    clauses: &[Clause],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Option<FoldedReturn>> {
    let Some(Clause::Match {
        optional: false,
        match_mode: MatchMode::DifferentRelationships,
        patterns,
        where_clause,
        search: None,
        using_index: _,
        using_join: _,
    }) = clauses.first()
    else {
        return Ok(None);
    };
    let [entering] = rows else {
        return Ok(None);
    };
    let [pattern] = patterns.as_slice() else {
        return Ok(None);
    };
    if kernels_stood_down(graph.base()) {
        return Ok(None);
    }
    let Some(item) = counted_return(clauses.get(1), pattern) else {
        return Ok(None);
    };
    let conjuncts = where_clause
        .as_ref()
        .map(flatten_and_owned)
        .unwrap_or_default();
    // `*k` spellings unroll virtually onto the fixed-hop chain; the
    // unrolled pattern lives for the compile.
    let unrolled = unroll_fixed(pattern);
    let compiled = unrolled
        .as_ref()
        .map(std::slice::from_ref)
        .unwrap_or(patterns.as_slice());
    let Some(Contract::Chain(contract)) = matcher::compile(
        compiled,
        MatchMode::DifferentRelationships,
        entering,
        &conjuncts,
    ) else {
        return Ok(None);
    };
    // Undirected hops keep the unchanged plan for now: their trail
    // corrections (a rel met from both ends) are the walker's.
    if contract
        .hops
        .iter()
        .any(|hop| hop.rel.direction == Direction::Both)
    {
        return Ok(None);
    }
    let stats = GraphStats::new(graph.base(), params);
    // The cost gate prices the hops the walk lands; the tally hop is the
    // one it never lands. An empty `hops` cannot happen (the compile's
    // rel gate enforces `2..=MAX_CHAIN_HOPS`), but the split states it
    // without a panicking slice.
    let Some((_, landed)) = contract.hops.split_last() else {
        return Ok(None);
    };
    if !chain_stages_fit(&stats, &conjuncts, &contract, landed) {
        return Ok(None);
    }
    #[cfg(test)]
    super::kernel_engaged();
    let ctx = ExecCtx::new(graph, params, cypher_version);
    let residual = Residual {
        conjuncts: &conjuncts,
        scope: &match_scope(entering, compiled),
    };
    record_walk_start(WalkStart::seed_side_forward());
    let starts = scan_segment(ctx, &contract.start, &residual)?;
    let count = chain::count(ctx, &contract, starts)?;
    Ok(super::count_fold_tail(graph, u128::from(count), item))
}

/// The pattern with every exact `*k` relationship spelled out as `k`
/// anonymous single hops (`(a)-[:R*3]->(b)` becomes
/// `(a)-[:R]->()-[:R]->()-[:R]->(b)`), or `None` when no relationship is
/// var-length or a spelling is not exactly unrollable: a named variable
/// (it binds a list), a property map, an inline `WHERE`, a quantified or
/// ranged hop. The unrolled pattern has the same default-mode (trail)
/// semantics — one relationship per hop, pairwise distinct.
fn unroll_fixed(path: &PathPattern) -> Option<PathPattern> {
    if !path.rels.iter().any(|rel| rel.var_length) || path.rels.len() + 1 != path.nodes.len() {
        return None;
    }
    let mut nodes = vec![path.nodes[0].clone()];
    let mut rels = Vec::new();
    for (rel, next) in path.rels.iter().zip(&path.nodes[1..]) {
        let copies = if rel.var_length {
            let k = rel.min_hops;
            if rel.quantified
                || rel.max_hops != Some(k)
                || !(1..=super::matcher::MAX_CHAIN_HOPS as u32).contains(&k)
                || rel.variable.is_some()
                || rel.properties.is_some()
                || rel.where_clause.is_some()
            {
                return None;
            }
            k
        } else {
            1
        };
        for hop in 0..copies {
            let mut single = rel.clone();
            single.var_length = false;
            single.min_hops = 1;
            single.max_hops = Some(1);
            rels.push(single);
            nodes.push(if hop + 1 == copies {
                next.clone()
            } else {
                NodePattern::anonymous()
            });
        }
    }
    let mut elements = vec![PatternElement::Node(nodes[0].clone())];
    for (rel, node) in rels.iter().zip(&nodes[1..]) {
        elements.push(PatternElement::Relationship(rel.clone()));
        elements.push(PatternElement::Node(node.clone()));
    }
    Some(PathPattern {
        elements,
        nodes,
        rels,
        ..path.clone()
    })
}
