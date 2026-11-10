//! The plan-warning detection at tree construction (ADR-0344, typed node
//! warnings): the MATCH-shape checks that construct the typed warnings a
//! plan node carries. The vocabulary itself — the [`PlanWarning`] enum,
//! its names, message texts, and notification projection — lives in the
//! model beside the notification vocabulary it projects into and the
//! wire parse it coordinates (LABS-461: one warning type); this module
//! is its engine-side construction site.
//!
//! [`PlanWarning::EstimationMiss`] is not detected at MATCH shape —
//! [`estimation_miss_warning`] derives it where the PROFILE metrics are
//! stamped (the same 10x policy the ratio cell's warning marker reads,
//! never a second computation).

use relate_ir::{Expr, NodePattern, PathPattern};
use relate_model::wire::estimation_miss;
pub use relate_model::wire::PlanWarning;
use relate_names::SmallName;

use crate::stats::PlanStats;

use super::seek::IndexSeek;

/// The `EstimationMiss` warning (typed node warnings), derived from the
/// one 10x overshoot policy ([`estimation_miss`]) the
/// ratio cell's warning marker reads — never a second computation — so
/// the warning fires exactly where the cell carries the `!`. `None`
/// where the policy does not fire or the operator carries no estimate to
/// judge.
pub(super) fn estimation_miss_warning(
    estimated: Option<u64>,
    actual: Option<u64>,
) -> Option<PlanWarning> {
    match (estimated, actual) {
        (Some(estimated), Some(actual)) if estimation_miss(estimated, actual) => {
            Some(PlanWarning::EstimationMiss)
        }
        _ => None,
    }
}

/// The warnings a `MATCH` subject carries, detected at tree construction
/// (typed node warnings): a multi-pattern `MATCH` whose patterns share no
/// join anchor, and the two scan shapes — the label-less scan
/// (`AllNodesScan`, ADR-0363) and the folded label disjunction no seek
/// serves (`LabelDisjunctionScan`, LABS-1083). The stats contract
/// ([`PlanStats`], the one every plan-tree stats read reads) supplies
/// the store's property indexes — the suggestions both scan warnings
/// name.
pub(super) fn match_warnings(
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    stats: &dyn PlanStats,
    seek: Option<&IndexSeek>,
) -> Vec<PlanWarning> {
    let mut warnings = Vec::new();
    if patterns.len() > 1 && !patterns_share_an_anchor(patterns) {
        warnings.push(PlanWarning::CartesianProduct);
    }
    // The scan warnings: the same subject shape the operator naming
    // derives (`lone_node_of`), never a second shape derivation — the
    // clause's own variables and the narrated seek threaded through, so
    // a bound the keyed lookup could never evaluate never drives the
    // suggestion, and a seek the plan narrates suppresses the scan
    // warning it would contradict.
    if let Some(node) = super::subject::lone_node_of(patterns) {
        let pattern_vars = super::subject::clause_node_variables(patterns);
        if let Some(warning) = scan_warning(node, where_clause, &pattern_vars, stats, seek) {
            warnings.push(warning);
        }
    }
    warnings
}

/// The lone-node scan's typed warning (ADR-0363; LABS-1083's fix): the
/// scan shape selects the warning from one fact — never a shared
/// payload over two shapes. The label-less scan (`MATCH (n)`) keeps
/// ADR-0363's `AllNodesScan`, the suggestion naming the first property
/// index the store carries, in the catalog's own listing order. The
/// folded label disjunction — `MATCH (p) WHERE p:Model OR p:Source`
/// after the rewrite: no conjunction labels, the `Or` in the label
/// expression, no seek serving it — carries its own
/// `LabelDisjunctionScan` (the scan answers per disjunct label, never
/// every node): the suggestion names the predicate's own per-disjunct
/// `:Label(property)` pairs, never an unrelated store index the
/// predicate never mentions. `None` otherwise: a labeled node, a
/// disjunction a seek serves (no scan to warn about), a map that
/// exists but names no eligible key (a `$param` whole map, a
/// disqualified bound — nothing to suggest, never an every-node claim),
/// or a label shape beyond the plain `Or` of single labels. The bare
/// `Or` with nothing keyed warns `AllNodesScan` — the every-node shape
/// the naming's own derivation answers.
fn scan_warning(
    node: &NodePattern,
    where_clause: Option<&Expr>,
    pattern_vars: &[SmallName],
    stats: &dyn PlanStats,
    seek: Option<&IndexSeek>,
) -> Option<PlanWarning> {
    // The `AllNodesScan` warning's suggestion, both every-node shapes
    // below read: the first property index the store carries, in the
    // catalog's own listing order (no predicate pair to name in
    // either).
    let all_nodes_scan = || {
        stats
            .property_index_texts()
            .first()
            .cloned()
            .map(|index| PlanWarning::AllNodesScan { index })
    };
    // The classic label-less scan, whatever predicates filter it — the
    // naming's derivation's own half, read here (ADR-0363).
    if super::subject::is_label_less(node) {
        return all_nodes_scan();
    }
    if seek.is_some() {
        return None;
    }
    // The fold's plain-`Or` shape: every arm a single label — an empty
    // shared base (a base-carrying shape keeps its labels and its
    // `NodeByLabelScan` narration, which names them). The warning's
    // WHERE-aware refinement over the where-blind naming: a keyed
    // equality — the pattern map's or a WHERE conjunct's — selects the
    // disjunction suggestion instead of the every-node warning.
    let arms = node.static_disjunction_arms()?;
    if !arms.base().is_empty() {
        return None;
    }
    let keys = super::subject::keyed_equalities(node, where_clause, pattern_vars);
    // A map that exists but names no eligible key (a `$param` whole
    // map, or every bound disqualified by the keyed recognition) keys
    // nothing the suggestion can name — stay silent rather than claim
    // or suggest (LABS-1083 review).
    if !super::subject::map_keys_nothing(node) && keys.map_keys.is_empty() {
        return None;
    }
    // The predicate's own filtered property: the first equality-filtered
    // key — the pair each disjunct label would want indexed. With
    // nothing keyed (no map, no eligible conjunct — the silent gate
    // above passed the map-present shapes), the scan answers every
    // node: the naming's own where-blind shape for the bare `Or`, the
    // same `AllNodesScan` warning the classic scan reads.
    let key = match keys.map_keys.iter().chain(&keys.conjunct_keys).next() {
        Some(key) => key.clone(),
        None => return all_nodes_scan(),
    };
    let indexes = arms
        .disjuncts()
        .iter()
        .map(|label| relate_catalog::index_schema_text(&label.to_string(), &key))
        .collect::<Vec<_>>()
        .join(", ");
    Some(PlanWarning::LabelDisjunctionScan { indexes })
}

/// Whether the patterns share a join anchor: a node variable that appears
/// in more than one pattern. A variable reused within one pattern joins
/// inside it and is not an anchor across patterns.
pub(crate) fn patterns_share_an_anchor(patterns: &[PathPattern]) -> bool {
    let per_pattern: Vec<Vec<SmallName>> = patterns
        .iter()
        .map(relate_ir::pattern_node_anchor_variables)
        .collect();
    per_pattern.iter().enumerate().any(|(i, variables)| {
        variables.iter().any(|variable| {
            per_pattern
                .iter()
                .enumerate()
                .any(|(j, others)| j != i && others.contains(variable))
        })
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate_ir::{BinaryOp, Expr, NodePattern};

    use super::super::fixtures::{comparison, node, or_node, seeking};
    use super::*;

    /// A lone-node pattern — the scan shape.
    fn lone(node: NodePattern) -> PathPattern {
        PathPattern {
            variable: None,
            subpath_variable: None,
            elements: Vec::new(),
            nodes: vec![node],
            rels: Vec::new(),
            where_clause: None,
            mode: None,
            selector: None,
            is_shortest_path_function: false,
        }
    }

    #[test]
    fn patterns_without_a_shared_anchor_do_not_join() {
        let disjoint = vec![
            lone(node(Some("a"), &["Person"])),
            lone(node(Some("b"), &["City"])),
        ];
        assert!(!patterns_share_an_anchor(&disjoint), "no shared variable");
        let anchored = vec![
            lone(node(Some("a"), &["Person"])),
            lone(node(Some("a"), &["City"])),
        ];
        assert!(patterns_share_an_anchor(&anchored), "a joins both");
        // A variable reused within one pattern only: not an anchor
        // across patterns.
        let mut within = lone(node(Some("a"), &["Person"]));
        within.nodes.push(node(Some("a"), &["City"]));
        assert!(
            !patterns_share_an_anchor(&[within, lone(node(Some("b"), &[]))]),
            "a within one pattern is not a cross-pattern anchor"
        );
    }

    /// LABS-1083: the folded label disjunction that no seek serves warns
    /// with the predicate's own `:Label(property)` pairs — the
    /// disjunct labels times the equality-filtered property — never an
    /// unrelated store index. A seek that serves the union suppresses
    /// the warning (no scan to warn about), and a disjunction with no
    /// equality predicate names nothing (there is no pair to suggest).
    #[test]
    fn the_folded_disjunction_scan_names_the_predicates_own_pairs() {
        let stats = seeking(&[("Model", 4), ("Source", 6)], &[], &[], BTreeMap::new());
        let patterns = [lone(or_node(Some("p"), &["Model", "Source"]))];
        let where_eq = comparison("p", "id", BinaryOp::Eq, Expr::Int(7));
        assert_eq!(
            match_warnings(&patterns, Some(&where_eq), &stats, None),
            vec![PlanWarning::LabelDisjunctionScan {
                indexes: ":Model(id), :Source(id)".to_string()
            }]
        );
        // The pattern-map spelling of the same predicate suggests the
        // same pairs.
        let mut mapped = or_node(Some("p"), &["Model", "Source"]);
        mapped.properties = Some(Expr::Map(vec![("id".to_string(), Expr::Int(7))]));
        assert_eq!(
            match_warnings(&[lone(mapped)], None, &stats, None),
            vec![PlanWarning::LabelDisjunctionScan {
                indexes: ":Model(id), :Source(id)".to_string()
            }]
        );
        // A narrated seek suppresses the scan warning (no scan to warn
        // about) — the same `Option` the clause node threads.
        let served = stats.clone().indexed(&[("Model", "id"), ("Source", "id")]);
        let narrated = super::super::seek::IndexSeek {
            operator: super::super::seek::NODE_BY_LABEL_SEEK,
            line: String::new(),
            rows: None,
        };
        assert!(match_warnings(&patterns, Some(&where_eq), &served, Some(&narrated)).is_empty());
        // No equality predicate: no pair to suggest.
        assert!(match_warnings(&patterns, None, &stats, None).is_empty());
    }
}
